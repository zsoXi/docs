//! Painted page bounds for PDF export. Coordinates stay in PDF user space.
use super::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect(pub [f64; 4]);
impl Rect {
    fn point(x: f64, y: f64) -> Self {
        Self([x, y, x, y])
    }
    fn union(self, other: Self) -> Self {
        Self([
            self.0[0].min(other.0[0]),
            self.0[1].min(other.0[1]),
            self.0[2].max(other.0[2]),
            self.0[3].max(other.0[3]),
        ])
    }
    fn intersect(self, other: Self) -> Option<Self> {
        let r = Self([
            self.0[0].max(other.0[0]),
            self.0[1].max(other.0[1]),
            self.0[2].min(other.0[2]),
            self.0[3].min(other.0[3]),
        ]);
        (r.0[0] <= r.0[2] && r.0[1] <= r.0[3]).then_some(r)
    }
    fn transformed(self, m: &Mat) -> Self {
        let [x0, y0, x1, y1] = self.0;
        [(x0, y0), (x0, y1), (x1, y0), (x1, y1)]
            .into_iter()
            .map(|(x, y)| {
                let (x, y) = apply(m, x, y);
                Self::point(x, y)
            })
            .reduce(Self::union)
            .unwrap_or(self)
    }
    fn inflate(self, x: f64, y: f64) -> Self {
        Self([self.0[0] - x, self.0[1] - y, self.0[2] + x, self.0[3] + y])
    }
}
fn include(out: &mut Option<Rect>, rect: Rect) {
    if rect.0.iter().all(|n| n.is_finite()) {
        *out = Some(out.map_or(rect, |old| old.union(rect)));
    }
}

pub(crate) struct PageBounds {
    pub media: [f64; 4],
    pub ink: Option<Rect>,
    pub fills: Vec<(Rect, String)>,
    /// Painted paths, excluding text and images, in page space.
    pub paths: Vec<Rect>,
    pub approximate: bool,
    pub operators: std::collections::BTreeSet<String>,
}
impl PageBounds {
    /// Insets of the accumulated ink bounds. Unsupported painting is flagged
    /// separately. Twip rounding contributes at most 0.025 pt; there is no
    /// symmetry, trailing-space cap or fixed margin.
    pub fn margins(&self) -> [f64; 4] {
        let [x0, y0, x1, y1] = self.media;
        self.ink
            .map(|r| [r.0[0] - x0, y1 - r.0[3], x1 - r.0[2], r.0[1] - y0])
            .unwrap_or([0.0; 4])
            .map(|n| n.max(0.0))
    }
}

pub(crate) fn page_bounds(doc: &Document, page_id: ObjectId) -> Result<PageBounds, Error> {
    let (x0, y0, x1, y1) = page_mediabox(doc, page_id);
    let media = [x0, y0, x1, y1];
    if !media.iter().all(|n| n.is_finite()) || x1 <= x0 || y1 <= y0 {
        return Err(lopdf::Error::Parse(lopdf::ParseError::InvalidContentStream).into());
    }
    let resources = page_resources(doc, page_id)?;
    let mut data = Vec::new();
    for id in doc.get_page_contents(page_id) {
        data.extend(crate::pdfbudget::stream(doc.get_object(id)?.as_stream()?)?);
        data.push(b'\n');
    }
    let mut traversal = Traversal::default();
    if let Ok(value) = doc.get_dictionary(page_id)?.get(b"Annots") {
        let (_, value) = doc.dereference(value)?;
        if !matches!(value, Object::Array(items) if items.is_empty()) {
            traversal.mark(b"Annots");
        }
    }
    for key in [b"Rotate".as_slice(), b"UserUnit", b"CropBox"] {
        let mut id = page_id;
        let mut visited = HashSet::new();
        while visited.insert(id) {
            let dict = doc.get_dictionary(id)?;
            if let Ok(value) = dict.get(key) {
                let ordinary = match key {
                    b"Rotate" => value.as_i64().is_ok_and(|n| n % 360 == 0),
                    b"UserUnit" => value.as_float().is_ok_and(|n| n == 1.0),
                    _ => value.as_array().is_ok_and(|a| {
                        a.len() == 4
                            && a.iter()
                                .zip(media)
                                .all(|(v, n)| v.as_float().is_ok_and(|v| f64::from(v) == n))
                    }),
                };
                if !ordinary {
                    traversal.mark(key);
                }
                break;
            }
            let Ok(parent) = dict.get(b"Parent").and_then(Object::as_reference) else {
                break;
            };
            id = parent;
        }
    }
    let ink = stream_bounds(
        doc,
        &data,
        &resources,
        IDENTITY,
        Rect(media),
        &mut traversal,
        None,
    )?;
    Ok(PageBounds {
        media,
        ink,
        fills: traversal.fills,
        paths: traversal.paths,
        approximate: !traversal.operators.is_empty(),
        operators: traversal.operators,
    })
}

fn resolve_dict<'a>(
    doc: &'a Document,
    resources: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Dictionary> {
    resources.get_deref(key, doc).ok()?.as_dict().ok()
}

#[derive(Clone)]
struct Graphics {
    ctm: Mat,
    clip: Option<Rect>,
    width: f64,
    cap: i32,
    dashed: bool,
    join: i32,
    fill: [f64; 3],
    text: TextParameters,
}

// The editing interpreter restores the CTM at Q, but not text parameters.
// Diagnose a difference only when a show/movement actually consumes it.
#[derive(Clone, PartialEq)]
struct TextParameters {
    font: Vec<u8>,
    values: [f64; 7], // size, Tc, Tw, Tz, Ts, Tr, TL
}
impl Default for TextParameters {
    fn default() -> Self {
        Self {
            font: Vec::new(),
            values: [0.0, 0.0, 0.0, 100.0, 0.0, 0.0, 0.0],
        }
    }
}
impl TextParameters {
    fn set(&mut self, op: &Operator) {
        if op.is("Tf") {
            self.font = op.name(0).unwrap_or_default().to_vec();
            self.values[0] = op.number(1).unwrap_or(0.0);
        } else if let Some(index) = ["Tc", "Tw", "Tz", "Ts", "Tr", "TL"]
            .iter()
            .position(|key| op.is(key))
        {
            self.values[index + 1] = op.number(0).unwrap_or(0.0);
        } else if op.is("TD") {
            self.values[6] = -op.number(1).unwrap_or(0.0);
        } else if op.is("\"") {
            self.values[2] = op.number(0).unwrap_or(0.0);
            self.values[1] = op.number(1).unwrap_or(0.0);
        }
    }
    fn same_paint(&self, other: &Self) -> bool {
        self.font == other.font && self.values[..6] == other.values[..6]
    }
}

#[derive(Default)]
struct Traversal {
    active: HashSet<ObjectId>,
    form_depth: usize,
    fills: Vec<(Rect, String)>,
    paths: Vec<Rect>,
    operators: std::collections::BTreeSet<String>,
}
impl Traversal {
    fn mark(&mut self, operator: &[u8]) {
        self.operators
            .insert(String::from_utf8_lossy(operator).into_owned());
    }
}

fn stream_bounds(
    doc: &Document,
    data: &[u8],
    resources: &Dictionary,
    outer: Mat,
    clip: Rect,
    traversal: &mut Traversal,
    inherited: Option<&Graphics>,
) -> Result<Option<Rect>, Error> {
    let ops = content::parse(data)?;
    fn finite(operand: &Operand) -> bool {
        match operand {
            Operand::Number(n) => n.is_finite(),
            Operand::Array(items) => items.iter().all(finite),
            _ => true,
        }
    }
    if !ops.iter().all(|op| op.operands.iter().all(finite)) {
        return Err(lopdf::Error::Parse(lopdf::ParseError::InvalidContentStream).into());
    }
    let mut fonts = HashMap::new();
    if let Some(dict) = resolve_dict(doc, resources, b"Font") {
        for (name, value) in dict {
            let (_, value) = doc.dereference(value)?;
            fonts.insert(
                name.clone(),
                Rc::new(FontInfo::load(value.as_dict()?, doc).with_geometry_widths()),
            );
        }
    }
    let mut lines = Vec::new();
    let mut page = PageData {
        stream_ids: Vec::new(),
        contents_origins: Vec::new(),
        streams: Vec::new(),
        operators: Vec::new(),
        lines: Vec::new(),
        mediabox: (0.0, 0.0, 0.0, 0.0),
    };
    // Bounds visit Forms below with their clipping rectangle; use the same
    // text state machine here without counting nested glyphs twice.
    Interpreter {
        doc,
        page: &mut page,
        stream: 0,
        ops: ops.clone(),
        fonts,
        resources: resources.clone(),
        depth: 0,
        active: HashSet::new(),
        recurse: false,
        fallback: Rc::new(FontInfo::default()),
    }
    .run(&mut lines, InterpreterState::default())?;
    let mut out = None;
    let mut shows: HashMap<usize, Vec<Show>> = HashMap::new();
    for line in lines {
        for show in line.shows {
            shows.entry(show.operator).or_default().push(show);
        }
    }
    let mut g = Graphics {
        ctm: outer,
        clip: Some(clip),
        width: 1.0,
        cap: 0,
        dashed: false,
        join: 0,
        fill: [0.0; 3],
        text: TextParameters::default(),
    };
    if let Some(inherited) = inherited {
        g = inherited.clone();
        g.ctm = outer;
        g.clip = Some(clip);
    }
    let mut interpreted_text = TextParameters::default();
    let mut state_operator = if inherited.is_some() {
        b"Do".as_slice()
    } else {
        b"Q".as_slice()
    };
    let mut stack = Vec::new();
    let mut path = None;
    let mut straight = Vec::new();
    let mut curved = false;
    let mut rectangular = false;
    let mut subpaths = 0usize;
    let mut current = (0.0, 0.0);
    let mut start = current;
    let mut clipping: Option<&[u8]> = None;
    for (index, op) in ops.iter().enumerate() {
        g.text.set(op);
        interpreted_text.set(op);
        if shows.contains_key(&index) && !g.text.same_paint(&interpreted_text) {
            traversal.mark(state_operator);
        }
        if matches!(op.keyword.as_slice(), b"T*" | b"'" | b"\"")
            && g.text.values[6] != interpreted_text.values[6]
        {
            traversal.mark(state_operator);
        }
        if matches!(op.keyword.as_slice(), b"Tj" | b"TJ" | b"'" | b"\"")
            && !shows.contains_key(&index)
            && op.operands.iter().any(nonempty_string)
        {
            traversal.mark(&op.keyword);
        }
        if let Some(clip) = g.clip
            && let Some(shows) = shows.get(&index)
        {
            for show in shows {
                text_bounds(show, outer, clip, &mut out, traversal, &op.keyword);
            }
        }
        match op.keyword.as_slice() {
            b"q" => stack.push(g.clone()),
            b"Q" => {
                if let Some(old) = stack.pop() {
                    g = old;
                    state_operator = b"Q";
                }
            }
            b"cm" => {
                if let Some(m) = op.tail_numbers::<6>() {
                    g.ctm = mat_mul(&m, &g.ctm);
                }
            }
            b"gs" => {
                let state = op.name(0).and_then(|name| {
                    resolve_dict(doc, resources, b"ExtGState")?
                        .get_deref(name, doc)
                        .ok()?
                        .as_dict()
                        .ok()
                });
                if let Some(state) = state {
                    for (key, value) in state {
                        match key.as_slice() {
                            b"Type" => {}
                            b"LW" => g.width = f64::from(value.as_float()?).abs(),
                            b"LC" => g.cap = value.as_i64()? as i32,
                            b"LJ" => g.join = value.as_i64()? as i32,
                            b"CA" | b"ca" if value.as_float().is_ok_and(|n| n == 1.0) => {}
                            b"BM"
                                if value
                                    .as_name()
                                    .is_ok_and(|n| n == b"Normal" || n == b"Compatible") => {}
                            b"SMask" if value.as_name().is_ok_and(|n| n == b"None") => {}
                            _ => traversal.mark(b"gs"),
                        }
                    }
                } else {
                    traversal.mark(b"gs");
                }
            }
            b"scn" | b"SCN" => {
                if op.operands.iter().any(|v| matches!(v, Operand::Name(_))) {
                    traversal.mark(&op.keyword);
                }
            }
            b"sh" | b"BI" => traversal.mark(&op.keyword),
            b"rg" => {
                if let Some(c) = op.tail_numbers::<3>() {
                    g.fill = c;
                }
            }
            b"g" => {
                if let Some(c) = op.number(0) {
                    g.fill = [c; 3];
                }
            }
            b"w" => g.width = op.number(0).unwrap_or(1.0).abs(),
            b"J" => g.cap = op.number(0).unwrap_or(0.0) as i32,
            b"j" => g.join = op.number(0).unwrap_or(0.0) as i32,
            b"d" => {
                g.dashed = !matches!(op.operands.first(), Some(Operand::Array(a)) if a.is_empty());
            }
            b"m" => {
                if let Some([x, y]) = op.tail_numbers::<2>() {
                    subpaths += 1;
                    rectangular = false;
                    current = apply(&g.ctm, x, y);
                    start = current;
                }
            }
            b"l" => {
                if let Some([x, y]) = op.tail_numbers::<2>() {
                    rectangular = false;
                    include(&mut path, Rect::point(current.0, current.1));
                    let to = apply(&g.ctm, x, y);
                    straight.push((current, to));
                    current = to;
                    include(&mut path, Rect::point(current.0, current.1));
                }
            }
            b"h" => {
                straight.push((current, start));
                include(&mut path, Rect::point(start.0, start.1));
                current = start;
            }
            b"re" => {
                if let Some([x, y, w, h]) = op.tail_numbers::<4>() {
                    subpaths += 1;
                    rectangular = path.is_none() && axis_aligned(&g.ctm);
                    curved = true;
                    include(
                        &mut path,
                        Rect([x.min(x + w), y.min(y + h), x.max(x + w), y.max(y + h)])
                            .transformed(&g.ctm),
                    );
                    current = apply(&g.ctm, x, y);
                    start = current;
                }
            }
            b"c" | b"v" | b"y" => {
                let points = match op.keyword.as_slice() {
                    b"c" => op.tail_numbers::<6>().map(|[a, b, c, d, e, f]| {
                        [
                            apply(&g.ctm, a, b),
                            apply(&g.ctm, c, d),
                            apply(&g.ctm, e, f),
                        ]
                    }),
                    b"v" => op
                        .tail_numbers::<4>()
                        .map(|[a, b, c, d]| [current, apply(&g.ctm, a, b), apply(&g.ctm, c, d)]),
                    _ => op.tail_numbers::<4>().map(|[a, b, c, d]| {
                        [
                            apply(&g.ctm, a, b),
                            apply(&g.ctm, c, d),
                            apply(&g.ctm, c, d),
                        ]
                    }),
                };
                if let Some([p1, p2, p3]) = points {
                    rectangular = false;
                    curved = true;
                    include(&mut path, cubic_bounds([current, p1, p2, p3]));
                    current = p3;
                }
            }
            b"W" | b"W*" => clipping = Some(&op.keyword),
            b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
                if let Some(p) = path {
                    // Some producers emit an axis-aligned rectangle as four
                    // straight edges instead of `re` (including text shading).
                    let filled_rectangle = rectangular
                        || (!curved && subpaths == 1 && straight.len() == 4 && {
                            let [x0, y0, x1, y1] = p.0;
                            straight.iter().all(|(a, b)| {
                                (a.0 == b.0 || a.1 == b.1)
                                    && [a, b].iter().all(|v| {
                                        (v.0 == x0 || v.0 == x1) && (v.1 == y0 || v.1 == y1)
                                    })
                            })
                        });
                    if !op.is("n") {
                        let stroke = matches!(
                            op.keyword.as_slice(),
                            b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*"
                        );
                        if (!stroke && !curved && straight.len() <= 2)
                            || (matches!(op.keyword.as_slice(), b"s" | b"b" | b"b*")
                                && !rectangular)
                        {
                            traversal.mark(&op.keyword);
                        }
                        let painted = if stroke && !curved && straight.len() == 1 {
                            let (a, b) = straight[0];
                            let dx = b.0 - a.0;
                            let dy = b.1 - a.1;
                            let length = dx.hypot(dy);
                            let det = g.ctm[0] * g.ctm[3] - g.ctm[1] * g.ctm[2];
                            if length > 0.0 && det.abs() > 1e-12 {
                                // Transform the normal in user space; a butt
                                // cap adds no ink beyond either endpoint.
                                let ux = (g.ctm[3] * dx - g.ctm[2] * dy) / det;
                                let uy = (-g.ctm[1] * dx + g.ctm[0] * dy) / det;
                                let unit = ux.hypot(uy);
                                let nx = (-g.ctm[0] * uy + g.ctm[2] * ux) / unit * g.width / 2.0;
                                let ny = (-g.ctm[1] * uy + g.ctm[3] * ux) / unit * g.width / 2.0;
                                let r = p.inflate(nx.abs(), ny.abs());
                                if g.cap == 0 {
                                    r
                                } else if g.cap == 2 {
                                    p.inflate(
                                        nx.abs() + dx.abs() / unit * g.width / 2.0,
                                        ny.abs() + dy.abs() / unit * g.width / 2.0,
                                    )
                                } else {
                                    p.inflate(
                                        g.width / 2.0 * g.ctm[0].hypot(g.ctm[2]),
                                        g.width / 2.0 * g.ctm[1].hypot(g.ctm[3]),
                                    )
                                }
                            } else {
                                traversal.mark(&op.keyword);
                                p
                            }
                        } else if stroke {
                            p.inflate(
                                g.width / 2.0 * g.ctm[0].hypot(g.ctm[2]),
                                g.width / 2.0 * g.ctm[1].hypot(g.ctm[3]),
                            )
                        } else {
                            p
                        };
                        if let Some(r) = g.clip.and_then(|c| painted.intersect(c)) {
                            if (r.0 != painted.0 && !rectangular) || (subpaths > 1 && !stroke) {
                                traversal.mark(&op.keyword);
                            }
                            include(&mut out, r);
                            traversal.paths.push(r);
                            if filled_rectangle && !matches!(op.keyword.as_slice(), b"S" | b"s") {
                                let [red, green, blue] =
                                    g.fill.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                                traversal
                                    .fills
                                    .push((r, format!("{red:02X}{green:02X}{blue:02X}")));
                            }
                        }
                    }
                    let stroke = matches!(
                        op.keyword.as_slice(),
                        b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*"
                    );
                    if stroke
                        && (g.dashed
                            || g.width == 0.0
                            || (stroke_complex(op, curved, straight.len())
                                && !(rectangular && g.join != 2 && axis_aligned(&g.ctm))))
                    {
                        traversal.mark(&op.keyword);
                    }
                    if let Some(operator) = clipping {
                        if !rectangular || !axis_aligned(&g.ctm) {
                            traversal.mark(operator);
                        }
                        g.clip = g.clip.and_then(|c| c.intersect(p));
                    }
                }
                if clipping.is_some() && path.is_none() {
                    g.clip = None;
                }
                path = None;
                subpaths = 0;
                clipping = None;
                straight.clear();
                curved = false;
                rectangular = false;
            }
            b"Do" => {
                let Some(value) = op
                    .name(0)
                    .and_then(|name| resolve_dict(doc, resources, b"XObject")?.get(name).ok())
                else {
                    traversal.mark(b"Do");
                    continue;
                };
                let (id, obj) = doc.dereference(value)?;
                let stream = obj.as_stream()?;
                let kind = stream.dict.get(b"Subtype")?.as_name()?;
                if kind == b"Image" {
                    if stream.dict.has(b"SMask")
                        || stream.dict.has(b"Mask")
                        || stream
                            .dict
                            .get(b"ImageMask")
                            .is_ok_and(|v| v.as_bool().unwrap_or(false))
                    {
                        traversal.mark(b"Do");
                    }
                    if let Some(r) = g
                        .clip
                        .and_then(|c| Rect([0.0, 0.0, 1.0, 1.0]).transformed(&g.ctm).intersect(c))
                    {
                        if !axis_aligned(&g.ctm)
                            && r.0 != Rect([0.0, 0.0, 1.0, 1.0]).transformed(&g.ctm).0
                        {
                            traversal.mark(b"Do");
                        }
                        include(&mut out, r);
                    }
                } else if kind == b"Form"
                    && traversal.form_depth < 32
                    && id.is_none_or(|id| !traversal.active.contains(&id))
                {
                    traversal.form_depth += 1;
                    if let Some(id) = id {
                        traversal.active.insert(id);
                    }
                    if stream.dict.has(b"Group") || !stream.dict.has(b"BBox") {
                        traversal.mark(b"Do");
                    }
                    let mut matrix = IDENTITY;
                    if let Ok(a) = stream.dict.get(b"Matrix").and_then(Object::as_array) {
                        if a.len() != 6 {
                            return Err(lopdf::Error::Parse(
                                lopdf::ParseError::InvalidContentStream,
                            )
                            .into());
                        }
                        for (dst, src) in matrix.iter_mut().zip(a) {
                            *dst = f64::from(src.as_float()?);
                        }
                    }
                    let matrix = mat_mul(&matrix, &g.ctm);
                    let mut form_clip = g.clip;
                    if let Ok(a) = stream.dict.get(b"BBox").and_then(Object::as_array)
                        && a.len() == 4
                    {
                        let bbox = Rect([
                            f64::from(a[0].as_float()?),
                            f64::from(a[1].as_float()?),
                            f64::from(a[2].as_float()?),
                            f64::from(a[3].as_float()?),
                        ])
                        .transformed(&matrix);
                        if !axis_aligned(&matrix) {
                            traversal.mark(b"Do");
                        }
                        form_clip = form_clip.and_then(|c| c.intersect(bbox));
                    }
                    if let Some(c) = form_clip {
                        let res =
                            resolve_dict(doc, &stream.dict, b"Resources").unwrap_or(resources);
                        if let Some(r) = stream_bounds(
                            doc,
                            &crate::pdfbudget::stream(stream)?,
                            res,
                            matrix,
                            c,
                            traversal,
                            Some(&g),
                        )? {
                            include(&mut out, r);
                        }
                    }
                    traversal.form_depth -= 1;
                    if let Some(id) = id {
                        traversal.active.remove(&id);
                    }
                } else {
                    traversal.mark(b"Do");
                }
            }
            // These operators either set text state interpreted above, or do
            // not change the support of painted content. Everything else is
            // deliberately diagnosed, including extension operators.
            b"BT" | b"ET" | b"Tf" | b"Tm" | b"Td" | b"TD" | b"T*" | b"Tj" | b"TJ" | b"'"
            | b"\"" | b"Tc" | b"Tw" | b"Tz" | b"TL" | b"Tr" | b"Ts" | b"RG" | b"G" | b"k"
            | b"K" | b"CS" | b"cs" | b"SC" | b"sc" | b"M" | b"ri" | b"i" | b"MP" | b"DP"
            | b"BMC" | b"BDC" | b"EMC" | b"BX" | b"EX" => {}
            _ => traversal.mark(&op.keyword),
        }
    }
    Ok(out)
}

fn nonempty_string(operand: &Operand) -> bool {
    match operand {
        Operand::Str(s) => !s.bytes.is_empty(),
        Operand::Array(items) => items.iter().any(nonempty_string),
        _ => false,
    }
}

fn axis_aligned(m: &Mat) -> bool {
    (m[1].abs() < 1e-12 && m[2].abs() < 1e-12) || (m[0].abs() < 1e-12 && m[3].abs() < 1e-12)
}

fn stroke_complex(op: &Operator, curved: bool, segments: usize) -> bool {
    matches!(
        op.keyword.as_slice(),
        b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*"
    ) && (curved || segments != 1)
}

fn text_bounds(
    show: &Show,
    outer: Mat,
    clip: Rect,
    out: &mut Option<Rect>,
    traversal: &mut Traversal,
    operator: &[u8],
) {
    if show.ink.render_mode == 7 {
        traversal.mark(b"Tr");
    }
    if matches!(show.ink.render_mode, 3 | 7) {
        return;
    }
    if show.ink.render_mode != 0 {
        traversal.mark(b"Tr");
    }
    let combined = mat_mul(&mat_mul(&show.tm, &show.ctm), &outer);
    if !axis_aligned(&combined) {
        traversal.mark(operator);
    }
    let mut x = 0.0;
    for ch in show.decoded.glyph_text.chars() {
        let width = show.font.text_width(&ch.to_string());
        if width.is_none() {
            traversal.mark(operator);
        }
        let advance = width
            .map(|w| w / 1000.0 * show.size)
            .unwrap_or(show.size * 0.5);
        if !ch.is_whitespace() {
            let rect = show.font.glyph_rect(ch);
            if rect.is_none() {
                traversal.mark(operator);
            }
            let [a, b, c, d] = rect.unwrap_or([0.0, -0.2, 0.5, 0.8]);
            let r = Rect([
                (x + a * show.size) * show.ink.scale,
                b * show.size + show.ink.rise,
                (x + c * show.size) * show.ink.scale,
                d * show.size + show.ink.rise,
            ])
            .transformed(&combined);
            if let Some(clipped) = r.intersect(clip) {
                if clipped.0 != r.0 {
                    traversal.mark(operator);
                }
                include(out, clipped);
            }
        }
        x += advance
            + show.ink.char_spacing
            + if ch == ' ' {
                show.ink.word_spacing
            } else {
                0.0
            };
    }
}

fn cubic_bounds(p: [(f64, f64); 4]) -> Rect {
    let mut out = Rect::point(p[0].0, p[0].1).union(Rect::point(p[3].0, p[3].1));
    for axis in 0..2 {
        let v = p.map(|p| if axis == 0 { p.0 } else { p.1 });
        let a = -v[0] + 3.0 * v[1] - 3.0 * v[2] + v[3];
        let b = 2.0 * (v[0] - 2.0 * v[1] + v[2]);
        let c = v[1] - v[0];
        let roots = if a.abs() < 1e-12 {
            [-c / b, f64::NAN]
        } else {
            let d = b * b - 4.0 * a * c;
            [(-b + d.sqrt()) / (2.0 * a), (-b - d.sqrt()) / (2.0 * a)]
        };
        for t in roots.into_iter().filter(|t| *t > 0.0 && *t < 1.0) {
            let eval = |v: [f64; 4]| {
                (1.0 - t).powi(3) * v[0]
                    + 3.0 * (1.0 - t).powi(2) * t * v[1]
                    + 3.0 * (1.0 - t) * t * t * v[2]
                    + t.powi(3) * v[3]
            };
            out = out.union(Rect::point(eval(p.map(|p| p.0)), eval(p.map(|p| p.1))));
        }
    }
    out
}
