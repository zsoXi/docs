//! In-place PDF text editing.
//!
//! The page's content streams are parsed with [`crate::content`], the text state
//! is tracked (`BT/ET`, `Tf`, `Tm`, `Td/TD/T*`, `Tc`, `Tw`, `Tz`, `TL`, `Ts`,
//! `cm`, `q/Q`, colours) and every shown string is decoded through its font.
//! A replacement changes only the bytes of the matched strings: no page is
//! rebuilt, so images, vector graphics, other text, annotations, links and
//! metadata stay exactly as they were.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use printpdf::cmap::ToUnicodeCMap;

use crate::content::{self, Operand, Operator, StrVal};
use crate::error::Error;
use crate::fontdecode::{Decoded, FontInfo};
use crate::messages::{MessageCode, Msg, Params, params};

#[path = "pdfbounds.rs"]
mod bounds;
pub(crate) use bounds::{PageBounds, page_bounds};
#[path = "pdfforms.rs"]
mod forms;

type Mat = [f64; 6];

const IDENTITY: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mat_mul(a: &Mat, b: &Mat) -> Mat {
    let [a0, a1, a2, a3, a4, a5] = *a;
    let [b0, b1, b2, b3, b4, b5] = *b;
    [
        a0 * b0 + a1 * b2,
        a0 * b1 + a1 * b3,
        a2 * b0 + a3 * b2,
        a2 * b1 + a3 * b3,
        a4 * b0 + a5 * b2 + b4,
        a4 * b1 + a5 * b3 + b5,
    ]
}

fn translate(tx: f64, ty: f64) -> Mat {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}

fn apply(mat: &Mat, x: f64, y: f64) -> (f64, f64) {
    let [a, b, c, d, e, f] = *mat;
    (a * x + c * y + e, b * x + d * y + f)
}

/// One shown string inside a line.
struct Show {
    stream: usize,
    operator: usize,
    operand: usize,
    decoded: Decoded,
    font: Rc<FontInfo>,
    size: f64,
    /// Text matrix in effect just before this string is shown.
    tm: Mat,
    /// Current transformation matrix at the same point.
    ctm: Mat,
    /// How far showing this string advances the text position, in text space.
    advance: f64,
    ink: InkState,
    color: String,
    /// Operator index (`Tm`/`Td`/`TD`/`T*`) that positioned this string since
    /// the previous shown string, if any.
    pos_op: Option<usize>,
}

impl Show {
    /// The string's origin in page (user) space.
    fn origin(&self) -> (f64, f64) {
        apply(&mat_mul(&self.tm, &self.ctm), 0.0, 0.0)
    }

    /// The end of the string in page space (origin plus its advance).
    fn end(&self) -> (f64, f64) {
        apply(&mat_mul(&self.tm, &self.ctm), self.advance, 0.0)
    }

    /// Page-space units per text-space unit along the baseline.
    fn scale_x(&self) -> f64 {
        let combined = mat_mul(&self.tm, &self.ctm);
        combined[0].hypot(combined[1])
    }

    /// The font size in page-space points.
    fn size_user(&self) -> f64 {
        let combined = mat_mul(&self.tm, &self.ctm);
        self.size * combined[2].hypot(combined[3])
    }
}

/// One text line: consecutive show operators on one baseline.
struct Line {
    shows: Vec<Show>,
    text: String,
    fill: Vec<Operator>,
    char_spacing: f64,
    word_spacing: f64,
    scale: f64,
    leading: f64,
    rise: f64,
    render_mode: i64,
    stream: usize,
    bt: usize,
}

/// A match inside a line: byte offsets into `Line::text`.
struct Match {
    line: usize,
    start: usize,
    end: usize,
}

/// A byte-range edit and its replacement.
type Edit = (Range<usize>, Vec<u8>);

/// One page prepared for editing.
pub(crate) struct PageData {
    stream_ids: Vec<ObjectId>,
    /// Original Contents ranges in the joined first stream, for byte-preserving edits.
    contents_origins: Vec<(ObjectId, Range<usize>)>,
    streams: Vec<Vec<u8>>,
    operators: Vec<Vec<Operator>>,
    lines: Vec<Line>,
    /// Page `MediaBox` as `(x0, y0, x1, y1)` in page space.
    mediabox: (f64, f64, f64, f64),
}

/// Font faces used when a PDF font lacks a glyph, one per style so bold text
/// gets a bold substitute and italic text an italic one.
#[derive(Debug, Default)]
pub(crate) struct SubstituteFonts {
    pub regular: Vec<u8>,
    pub bold: Vec<u8>,
    pub italic: Vec<u8>,
    pub bold_italic: Vec<u8>,
}

impl SubstituteFonts {
    /// The face matching a style, falling back to the regular face.
    fn for_style(&self, bold: bool, italic: bool) -> &[u8] {
        let styled = match (bold, italic) {
            (true, true) => &self.bold_italic,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (false, false) => &self.regular,
        };
        if styled.is_empty() {
            &self.regular
        } else {
            styled
        }
    }

    #[cfg(test)]
    fn all(bytes: &[u8]) -> Self {
        Self {
            regular: bytes.to_vec(),
            bold: bytes.to_vec(),
            italic: bytes.to_vec(),
            bold_italic: bytes.to_vec(),
        }
    }
}

/// Resource name and `/BaseFont` name of the substitute font for a style.
fn substitute_names(bold: bool, italic: bool) -> (&'static [u8], &'static str) {
    match (bold, italic) {
        (true, true) => (b"DokuSubBI", "DokuSub-BoldItalic"),
        (true, false) => (b"DokuSubB", "DokuSub-Bold"),
        (false, true) => (b"DokuSubI", "DokuSub-Italic"),
        (false, false) => (b"DokuSub", "DokuSub"),
    }
}

/// The outcome of a successful replacement pass.
#[derive(Debug)]
pub(crate) struct ReplaceOutcome {
    pub replaced: usize,
    pub present: usize,
    pub partial_glyphs: usize,
    pub warnings: Vec<Msg>,
}

fn fmt_num(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        let mut out = format!("{value:.4}");
        while out.ends_with('0') {
            out.pop();
        }
        if out.ends_with('.') {
            out.pop();
        }
        out
    }
}

fn serialize_operand(operand: &Operand, out: &mut Vec<u8>) {
    match operand {
        Operand::Number(value) => out.extend_from_slice(fmt_num(*value).as_bytes()),
        Operand::Name(name) => {
            out.push(b'/');
            out.extend_from_slice(name);
        }
        Operand::Str(value) => {
            if value.hex {
                out.extend_from_slice(&content::hex_string(&value.bytes));
            } else {
                out.extend_from_slice(&content::literal(&value.bytes));
            }
        }
        Operand::Array(items) => {
            out.push(b'[');
            for item in items {
                serialize_operand(item, out);
                out.push(b' ');
            }
            out.push(b']');
        }
        Operand::Other => {}
    }
}

fn serialize_operator(operator: &Operator) -> Vec<u8> {
    let mut out = Vec::new();
    for operand in &operator.operands {
        serialize_operand(operand, &mut out);
        out.push(b' ');
    }
    out.extend_from_slice(&operator.keyword);
    out.push(b'\n');
    out
}

/// Decode a page's content and text.
pub(crate) fn load_page(doc: &Document, page_id: ObjectId) -> Result<PageData, Error> {
    // Validate Form content too, before any caller can fall back to an
    // extractor with an unbounded recursive operand decoder.
    walk_page_xobjects(doc, page_id, false)?;
    let resources = page_resources(doc, page_id)?;
    let fonts = load_resource_fonts(doc, &resources)?;
    let stream_ids = doc.get_page_contents(page_id);
    let mut data = Vec::new();
    let mut contents_origins = Vec::new();
    for id in &stream_ids {
        let start = data.len();
        data.extend(crate::pdfbudget::stream(doc.get_object(*id)?.as_stream()?)?);
        contents_origins.push((*id, start..data.len()));
        data.push(b'\n');
        if data.len() > crate::pdfbudget::DOCUMENT_BYTES {
            return Err(crate::pdfbudget::limit(
                "page_bytes",
                crate::pdfbudget::DOCUMENT_BYTES,
            ));
        }
    }
    let operators = vec![content::parse(&data)?];
    let streams = vec![data];
    let mut page = PageData {
        stream_ids: vec![stream_ids.first().copied().unwrap_or((0, 0))],
        contents_origins,
        streams,
        operators,
        lines: Vec::new(),
        mediabox: page_mediabox(doc, page_id),
    };
    let ops = page.operators[0].clone();
    let mut lines = Vec::new();
    Interpreter {
        doc,
        page: &mut page,
        stream: 0,
        ops,
        fonts,
        resources,
        depth: 0,
        active: HashSet::new(),
        recurse: true,
        fallback: Rc::new(FontInfo::default()),
    }
    .run(&mut lines, InterpreterState::default())?;
    page.lines = lines;
    Ok(page)
}

/// The page's `MediaBox`, walking up the page tree; letter size as a fallback.
fn page_mediabox(doc: &Document, page_id: ObjectId) -> (f64, f64, f64, f64) {
    let mut current = Some(page_id);
    let mut hops = 0usize;
    while let Some(id) = current {
        let Ok(dict) = doc.get_dictionary(id) else {
            break;
        };
        if let Ok(Object::Array(values)) = dict.get(b"MediaBox")
            && values.len() == 4
        {
            let mut numbers = [0.0f64; 4];
            let mut ok = true;
            for (slot, value) in numbers.iter_mut().zip(values.iter()) {
                match value.as_float() {
                    Ok(number) => *slot = f64::from(number),
                    Err(_) => ok = false,
                }
            }
            if ok && numbers[2] > numbers[0] {
                return (numbers[0], numbers[1], numbers[2], numbers[3]);
            }
        }
        hops += 1;
        if hops > 16 {
            break;
        }
        current = dict
            .get(b"Parent")
            .ok()
            .and_then(|value| value.as_reference().ok());
    }
    (0.0, 0.0, 612.0, 792.0)
}

/// The nearest Resources entry is inherited as a whole, including direct
/// dictionaries on Pages nodes (not only indirect resource dictionaries).
fn page_resources(doc: &Document, page_id: ObjectId) -> Result<Dictionary, Error> {
    let mut current = page_id;
    let mut seen = HashSet::new();
    for _ in 0..64 {
        if !seen.insert(current) {
            return Err(lopdf::Error::ReferenceCycle(current).into());
        }
        let dict = doc.get_dictionary(current)?;
        if let Ok(value) = dict.get(b"Resources") {
            return Ok(doc.dereference(value)?.1.as_dict()?.clone());
        }
        match dict.get(b"Parent") {
            Ok(value) => current = value.as_reference()?,
            Err(_) => return Ok(Dictionary::new()),
        }
    }
    Err(lopdf::Error::RecursionLimit.into())
}

fn load_resource_fonts(
    doc: &Document,
    resources: &Dictionary,
) -> Result<HashMap<Vec<u8>, Rc<FontInfo>>, Error> {
    let mut fonts = HashMap::new();
    if let Ok(value) = resources.get(b"Font") {
        for (name, value) in doc.dereference(value)?.1.as_dict()? {
            let dict = doc.dereference(value)?.1.as_dict()?;
            fonts.insert(name.clone(), Rc::new(FontInfo::load(dict, doc)));
        }
    }
    Ok(fonts)
}

#[derive(Clone)]
struct InterpreterState {
    ctm: Mat,
    fill: Vec<Operator>,
    font: Rc<FontInfo>,
    size: f64,
    char_spacing: f64,
    word_spacing: f64,
    scale: f64,
    leading: f64,
    rise: f64,
    render_mode: i64,
}

impl Default for InterpreterState {
    fn default() -> Self {
        Self {
            ctm: IDENTITY,
            fill: Vec::new(),
            font: Rc::new(FontInfo::default()),
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            scale: 100.0,
            leading: 0.0,
            rise: 0.0,
            render_mode: 0,
        }
    }
}

struct Interpreter<'a> {
    doc: &'a Document,
    page: &'a mut PageData,
    stream: usize,
    ops: Vec<Operator>,
    fonts: HashMap<Vec<u8>, Rc<FontInfo>>,
    resources: Dictionary,
    depth: usize,
    active: HashSet<ObjectId>,
    recurse: bool,
    fallback: Rc<FontInfo>,
}

impl Interpreter<'_> {
    fn run(self, lines: &mut Vec<Line>, initial: InterpreterState) -> Result<(), Error> {
        let mut ctm = initial.ctm;
        let mut stack: Vec<InterpreterState> = Vec::new();
        let mut tm = IDENTITY;
        let mut tlm = IDENTITY;
        let mut in_text = false;
        let mut bt = 0usize;
        let mut font = initial.font;
        let mut size = initial.size;
        let mut char_spacing = initial.char_spacing;
        let mut word_spacing = initial.word_spacing;
        let mut scale = initial.scale;
        let mut leading = initial.leading;
        let mut rise = initial.rise;
        let mut render_mode = initial.render_mode;
        let mut fill = initial.fill;
        let mut current: Option<Line> = None;
        // The last positioning operator seen since the previous shown string.
        let mut pending_pos: Option<usize> = None;

        // Local macro-free helper closures are awkward with the borrow checker;
        // use small methods on a state struct.
        for (op_index, op) in self.ops.iter().enumerate() {
            let keyword = op.keyword.as_slice();
            match keyword {
                b"q" => {
                    if stack.len() >= crate::pdfbudget::DEPTH {
                        return Err(crate::pdfbudget::limit(
                            "graphics_depth",
                            crate::pdfbudget::DEPTH,
                        ));
                    }
                    stack.push(InterpreterState {
                        ctm,
                        fill: fill.clone(),
                        font: font.clone(),
                        size,
                        char_spacing,
                        word_spacing,
                        scale,
                        leading,
                        rise,
                        render_mode,
                    });
                }
                b"Q" => {
                    if let Some(previous) = stack.pop() {
                        ctm = previous.ctm;
                        fill = previous.fill;
                        font = previous.font;
                        size = previous.size;
                        char_spacing = previous.char_spacing;
                        word_spacing = previous.word_spacing;
                        scale = previous.scale;
                        leading = previous.leading;
                        rise = previous.rise;
                        render_mode = previous.render_mode;
                    }
                }
                b"Do" if self.recurse => {
                    let Some(name) = op.name(0) else { continue };
                    let Ok(objects) = self
                        .resources
                        .get_deref(b"XObject", self.doc)
                        .and_then(Object::as_dict)
                    else {
                        continue;
                    };
                    let Ok(value) = objects.get(name) else {
                        continue;
                    };
                    let (id, object) = self.doc.dereference(value)?;
                    let stream = object.as_stream()?;
                    if !stream
                        .dict
                        .get(b"Subtype")
                        .is_ok_and(|v| v.as_name().is_ok_and(|v| v == b"Form"))
                    {
                        continue;
                    }
                    if self.depth >= crate::pdfbudget::DEPTH
                        || id.is_some_and(|id| self.active.contains(&id))
                    {
                        continue; // Diagnosed by form_warnings at the operation boundary.
                    }
                    flush(&mut current, lines);
                    let data = crate::pdfbudget::stream(stream)?;
                    if self
                        .page
                        .streams
                        .iter()
                        .map(Vec::len)
                        .sum::<usize>()
                        .saturating_add(data.len())
                        > crate::pdfbudget::DOCUMENT_BYTES
                    {
                        return Err(crate::pdfbudget::limit(
                            "expanded_page_bytes",
                            crate::pdfbudget::DOCUMENT_BYTES,
                        ));
                    }
                    let ops = content::parse(&data)?;
                    let resources = match stream.dict.get(b"Resources") {
                        Ok(value) => self.doc.dereference(value)?.1.as_dict()?.clone(),
                        Err(_) => self.resources.clone(),
                    };
                    let fonts = load_resource_fonts(self.doc, &resources)?;
                    let mut matrix = IDENTITY;
                    if let Ok(value) = stream.dict.get(b"Matrix") {
                        let array = self.doc.dereference(value)?.1.as_array()?;
                        if array.len() != 6 {
                            return Err(lopdf::Error::Parse(
                                lopdf::ParseError::InvalidContentStream,
                            )
                            .into());
                        }
                        for (slot, value) in matrix.iter_mut().zip(array) {
                            *slot = f64::from(value.as_float()?);
                        }
                    }
                    let mut active = self.active.clone();
                    if let Some(id) = id {
                        active.insert(id);
                    }
                    let index = self.page.streams.len();
                    self.page.streams.push(data);
                    self.page.operators.push(ops.clone());
                    self.page.stream_ids.push(id.unwrap_or((0, 0)));
                    Interpreter {
                        doc: self.doc,
                        page: self.page,
                        stream: index,
                        ops,
                        fonts,
                        resources,
                        depth: self.depth + 1,
                        active,
                        recurse: true,
                        fallback: self.fallback.clone(),
                    }
                    .run(
                        lines,
                        InterpreterState {
                            ctm: mat_mul(&matrix, &ctm),
                            fill: fill.clone(),
                            font: font.clone(),
                            size,
                            char_spacing,
                            word_spacing,
                            scale,
                            leading,
                            rise,
                            render_mode,
                        },
                    )?;
                }
                b"cm" => {
                    if let Some(values) = op.tail_numbers::<6>() {
                        ctm = mat_mul(&values, &ctm);
                    }
                }
                b"BT" => {
                    in_text = true;
                    bt = op_index;
                    tm = IDENTITY;
                    tlm = IDENTITY;
                    pending_pos = None;
                }
                b"ET" => {
                    in_text = false;
                    flush(&mut current, lines);
                }
                b"Tf" => {
                    if let Some(name) = op.name(0) {
                        font = self
                            .fonts
                            .get(name)
                            .cloned()
                            .unwrap_or_else(|| self.fallback.clone());
                    }
                    size = op.number(1).unwrap_or(0.0);
                }
                b"Tc" => char_spacing = op.number(0).unwrap_or(0.0),
                b"Tw" => word_spacing = op.number(0).unwrap_or(0.0),
                b"Tz" => scale = op.number(0).unwrap_or(100.0),
                b"TL" => leading = op.number(0).unwrap_or(0.0),
                b"Ts" => rise = op.number(0).unwrap_or(0.0),
                b"Tr" => render_mode = op.number(0).unwrap_or(0.0) as i64,
                b"Tm" => {
                    if let Some(values) = op.tail_numbers::<6>() {
                        tm = values;
                        tlm = values;
                    }
                    pending_pos = Some(op_index);
                }
                b"Td" => {
                    if let Some([tx, ty]) = op.tail_numbers::<2>() {
                        tlm = mat_mul(&translate(tx, ty), &tlm);
                        tm = tlm;
                    }
                    pending_pos = Some(op_index);
                }
                b"TD" => {
                    if let Some([tx, ty]) = op.tail_numbers::<2>() {
                        leading = -ty;
                        tlm = mat_mul(&translate(tx, ty), &tlm);
                        tm = tlm;
                    }
                    pending_pos = Some(op_index);
                }
                b"T*" => {
                    tlm = mat_mul(&translate(0.0, -leading), &tlm);
                    tm = tlm;
                    pending_pos = Some(op_index);
                }
                b"g" | b"rg" | b"k" => {
                    fill = vec![op.clone()];
                }
                b"cs" => {
                    fill = vec![op.clone()];
                }
                b"sc" | b"scn" => {
                    fill.push(op.clone());
                }
                b"Tj" if in_text => {
                    if show_operand(
                        &mut current,
                        lines,
                        &mut tm,
                        self.stream,
                        op_index,
                        0,
                        op,
                        &font,
                        size,
                        TextState {
                            char_spacing,
                            word_spacing,
                            scale,
                            leading,
                            rise,
                            render_mode,
                            fill: &fill,
                            bt,
                            ctm,
                            pos_op: pending_pos,
                        },
                    ) {
                        pending_pos = None;
                    }
                }
                b"TJ" if in_text => {
                    if let Some(Operand::Array(items)) = op.operands.first() {
                        let mut element_pos = pending_pos;
                        for (operand, item) in items.iter().enumerate() {
                            match item {
                                Operand::Str(_) => {
                                    if show_operand(
                                        &mut current,
                                        lines,
                                        &mut tm,
                                        self.stream,
                                        op_index,
                                        operand,
                                        op,
                                        &font,
                                        size,
                                        TextState {
                                            char_spacing,
                                            word_spacing,
                                            scale,
                                            leading,
                                            rise,
                                            render_mode,
                                            fill: &fill,
                                            bt,
                                            ctm,
                                            pos_op: element_pos,
                                        },
                                    ) {
                                        element_pos = None;
                                        pending_pos = None;
                                    }
                                }
                                Operand::Number(adjust) => {
                                    let dx = -adjust / 1000.0 * size * scale / 100.0;
                                    tm = mat_mul(&translate(dx, 0.0), &tm);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                b"'" if in_text => {
                    tlm = mat_mul(&translate(0.0, -leading), &tlm);
                    tm = tlm;
                    if show_operand(
                        &mut current,
                        lines,
                        &mut tm,
                        self.stream,
                        op_index,
                        0,
                        op,
                        &font,
                        size,
                        TextState {
                            char_spacing,
                            word_spacing,
                            scale,
                            leading,
                            rise,
                            render_mode,
                            fill: &fill,
                            bt,
                            ctm,
                            pos_op: pending_pos,
                        },
                    ) {
                        pending_pos = None;
                    }
                }
                b"\"" if in_text => {
                    word_spacing = op.number(0).unwrap_or(word_spacing);
                    char_spacing = op.number(1).unwrap_or(char_spacing);
                    tlm = mat_mul(&translate(0.0, -leading), &tlm);
                    tm = tlm;
                    if show_operand(
                        &mut current,
                        lines,
                        &mut tm,
                        self.stream,
                        op_index,
                        2,
                        op,
                        &font,
                        size,
                        TextState {
                            char_spacing,
                            word_spacing,
                            scale,
                            leading,
                            rise,
                            render_mode,
                            fill: &fill,
                            bt,
                            ctm,
                            pos_op: pending_pos,
                        },
                    ) {
                        pending_pos = None;
                    }
                }
                _ => {}
            }
        }
        flush(&mut current, lines);
        Ok(())
    }
}

struct InkState {
    char_spacing: f64,
    word_spacing: f64,
    scale: f64,
    rise: f64,
    render_mode: i64,
}

struct TextState<'a> {
    char_spacing: f64,
    word_spacing: f64,
    scale: f64,
    leading: f64,
    rise: f64,
    render_mode: i64,
    fill: &'a [Operator],
    bt: usize,
    ctm: Mat,
    pos_op: Option<usize>,
}

fn flush(current: &mut Option<Line>, lines: &mut Vec<Line>) {
    if let Some(line) = current.take() {
        lines.push(line);
    }
}

#[allow(clippy::too_many_arguments)]
fn show_operand(
    current: &mut Option<Line>,
    lines: &mut Vec<Line>,
    tm: &mut Mat,
    stream: usize,
    operator: usize,
    operand: usize,
    op: &Operator,
    font: &Rc<FontInfo>,
    size: f64,
    state: TextState<'_>,
) -> bool {
    let Some(Operand::Str(strval)) = operand_of(op, operand) else {
        return false;
    };
    let decoded = font.decode(&strval.bytes);
    if decoded.text.is_empty() {
        return false;
    }
    let advance = advance_width(font, &decoded.glyph_text, size, &state);
    let show = Show {
        stream,
        operator,
        operand,
        decoded,
        font: font.clone(),
        size,
        tm: *tm,
        ctm: state.ctm,
        advance,
        ink: InkState {
            char_spacing: state.char_spacing,
            word_spacing: state.word_spacing,
            scale: state.scale / 100.0,
            rise: state.rise,
            render_mode: state.render_mode,
        },
        pos_op: state.pos_op,
        color: fill_color(state.fill),
    };
    // A new line starts when the baseline moves or the position jumps back
    // to the left of the previous string (a new column or fragment).
    // Positioning operators between glyphs on the same baseline (`Td` chains
    // as emitted by Skia/Chromium) stay inside one line.
    if let Some(line) = current.as_ref()
        && let Some(previous) = line.shows.last()
    {
        let (x, y) = show.origin();
        let (previous_x, previous_y) = previous.origin();
        let size_user = (size * show.scale_x()).max(1.0);
        if (y - previous_y).abs() > (0.45 * size_user).max(2.0) || x < previous_x - 1.0 {
            flush(current, lines);
        }
    }
    let line = current.get_or_insert_with(|| Line {
        shows: Vec::new(),
        text: String::new(),
        fill: state.fill.to_vec(),
        char_spacing: state.char_spacing,
        word_spacing: state.word_spacing,
        scale: state.scale,
        leading: state.leading,
        rise: state.rise,
        render_mode: state.render_mode,
        stream,
        bt: state.bt,
    });
    line.text.push_str(&show.decoded.text);
    line.shows.push(show);
    *tm = mat_mul(&translate(advance, 0.0), tm);
    true
}

fn operand_of(op: &Operator, index: usize) -> Option<Operand> {
    if op.is("TJ") {
        // For a `TJ` operand index refers to an element of the array.
        if let Some(Operand::Array(items)) = op.operands.first() {
            return items.get(index).cloned();
        }
        None
    } else {
        op.operands.get(index).cloned()
    }
}

fn advance_width(font: &FontInfo, text: &str, size: f64, state: &TextState<'_>) -> f64 {
    let mut advance = 0.0;
    for ch in text.chars() {
        let width = font
            .text_width(&ch.to_string())
            .map(|width| width / 1000.0 * size)
            .unwrap_or(size * 0.5);
        advance += width + state.char_spacing;
        if ch == ' ' {
            advance += state.word_spacing;
        }
    }
    advance * state.scale / 100.0
}

/// Replace `find` in the document. Every replacement is an in-place byte edit.
pub(crate) fn replace_in_document(
    doc: &mut Document,
    find: &str,
    replacement: &str,
    all: bool,
    substitutes: &SubstituteFonts,
) -> Result<ReplaceOutcome, Error> {
    if doc.trailer.get(b"Encrypt").is_ok() {
        return Err(Error::invalid(MessageCode::PdfEncrypted, Params::new()));
    }
    crate::pdf::materialize_pages(doc)?;
    let signed = has_signature(doc);
    let page_ids: Vec<ObjectId> = doc.get_pages().into_values().collect();
    let mut replaced = 0usize;
    let mut present = 0usize;
    let mut warnings = Vec::new();
    let mut stop = false;
    let mut has_text = false;
    let mut split_glyphs = 0usize;

    for page_id in page_ids {
        if stop {
            break;
        }
        warnings.extend(form_warnings(doc, page_id)?);
        let mut page = load_page(doc, page_id)?;
        if page.lines.iter().any(|line| !line.text.trim().is_empty()) {
            has_text = true;
        }
        let matches = find_matches(&page.lines, find);
        present += matches.len();
        if matches.is_empty() {
            continue;
        }
        if page.streams.len() > 1 {
            let resources = page_resources(doc, page_id)?;
            forms::isolate(doc, page_id, page.streams[0].clone(), resources)?;
            page = load_page(doc, page_id)?;
        }
        let matches = find_matches(&page.lines, find);
        let mut selected = Vec::new();
        for candidate in matches {
            if !all && replaced + selected.len() >= 1 {
                break;
            }
            // A source glyph is indivisible. Do not delete its other letters
            // when a query matches only part of an expanded ligature.
            if !covers_whole_glyphs(&page.lines[candidate.line], &candidate) {
                split_glyphs += 1;
                continue;
            }
            selected.push(candidate);
        }
        apply_matches(
            doc,
            page_id,
            &mut page,
            find,
            replacement,
            &selected,
            substitutes,
            &mut warnings,
        )?;
        replaced += selected.len();
        if !all && replaced >= 1 {
            stop = true;
        }
    }

    if !has_text && warnings.is_empty() {
        return Err(Error::invalid(MessageCode::PdfNoText, Params::new()));
    }
    if signed {
        warnings.push(Msg::new(MessageCode::PdfSignatureInvalid));
    }
    if split_glyphs > 0 {
        warnings.push(Msg::with(
            MessageCode::PdfPartialGlyph,
            params(&[("skipped", split_glyphs.into())]),
        ));
    }
    Ok(ReplaceOutcome {
        replaced,
        present,
        partial_glyphs: split_glyphs,
        warnings,
    })
}

fn has_signature(doc: &Document) -> bool {
    doc.objects.values().any(|object| {
        object
            .as_dict()
            .ok()
            .and_then(|dict| dict.get(b"Type").ok())
            .and_then(|value| value.as_name().ok())
            .is_some_and(|name| name == b"Sig")
    }) || doc.objects.values().any(|object| {
        object
            .as_dict()
            .ok()
            .is_some_and(|dict| dict.get(b"ByteRange").is_ok())
    })
}

fn find_matches(lines: &[Line], find: &str) -> Vec<Match> {
    let find_chars = find.chars().count();
    if find_chars == 0 {
        return Vec::new();
    }
    let mut matches = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let mut start = 0usize;
        while let Some(offset) = line.text[start..].find(find) {
            let begin = start + offset;
            let end = begin + find.len();
            matches.push(Match {
                line: line_index,
                start: line.text[..begin].chars().count(),
                end: line.text[..end].chars().count(),
            });
            start = end;
        }
    }
    matches
}

fn covers_whole_glyphs(line: &Line, matched: &Match) -> bool {
    let ranges = char_range(line);
    affected_of(line, matched, &ranges)
        .iter()
        .all(|&(index, from, to)| {
            let chars = &line.shows[index].decoded.chars;
            (from == 0 || chars.get(from - 1) != chars.get(from))
                && (to == chars.len() || chars.get(to - 1) != chars.get(to))
        })
}

fn char_range(line: &Line) -> Vec<(usize, usize)> {
    let mut ranges = Vec::with_capacity(line.shows.len());
    let mut at = 0usize;
    for show in &line.shows {
        let count = show.decoded.chars.len();
        ranges.push((at, at + count));
        at += count;
    }
    ranges
}

/// Shows of a match: `(show index in its line, first char, end char)`.
fn affected_of(
    line: &Line,
    matched: &Match,
    ranges: &[(usize, usize)],
) -> Vec<(usize, usize, usize)> {
    let mut affected = Vec::new();
    for (show_index, (from, to)) in ranges.iter().enumerate() {
        let start = matched.start.max(*from);
        let end = matched.end.min(*to);
        if start < end {
            affected.push((show_index, start - from, end - from));
        }
    }
    let _ = line;
    affected
}

fn line_state(line: &Line) -> TextState<'_> {
    TextState {
        char_spacing: line.char_spacing,
        word_spacing: line.word_spacing,
        scale: line.scale,
        leading: line.leading,
        rise: line.rise,
        render_mode: line.render_mode,
        fill: &line.fill,
        bt: line.bt,
        ctm: IDENTITY,
        pos_op: None,
    }
}

/// Advance of characters `from..to` of one show, in text space.
fn segment_advance(show: &Show, from: usize, to: usize, state: &TextState<'_>) -> f64 {
    if from >= to {
        return 0.0;
    }
    // Measure the source glyph once even when its Unicode text expands to
    // multiple searchable letters. Partial ligature ranges share that glyph.
    let range = decoded_range(show, from, to);
    let mut at = 0;
    let mut text = String::new();
    for ch in show.decoded.glyph_text.chars() {
        let count = crate::fontdecode::expand_ligatures(&ch.to_string())
            .chars()
            .count();
        if show
            .decoded
            .chars
            .get(at)
            .is_some_and(|&(a, b)| a < range.end && b > range.start)
        {
            text.push(ch);
        }
        at += count;
    }
    advance_width(&show.font, &text, show.size, state)
}

/// A substitute font reduced to the glyphs actually used, ready to embed.
/// Glyph IDs in the subset program are also the CIDs written to the stream.
struct SubsetFont {
    program: Vec<u8>,
    glyphs: BTreeMap<char, u16>,
    widths: BTreeMap<u16, f64>,
}

impl SubsetFont {
    fn build(ttf: &[u8], chars: &std::collections::BTreeSet<char>) -> Result<Self, Error> {
        let face = ttf_parser::Face::parse(ttf, 0).map_err(|error| {
            Error::invalid(
                MessageCode::FontParseSubstitute,
                params(&[("detail", error.to_string().into())]),
            )
        })?;
        let upem = f64::from(face.units_per_em().max(1));
        let mut remapper = subsetter::GlyphRemapper::new();
        let mut glyphs = BTreeMap::new();
        let mut widths = BTreeMap::new();
        for ch in chars {
            if let Some(glyph) = face.glyph_index(*ch) {
                let new_gid = remapper.remap(glyph.0);
                glyphs.insert(*ch, new_gid);
                let width = face
                    .glyph_hor_advance(glyph)
                    .map(|advance| f64::from(advance) * 1000.0 / upem)
                    .unwrap_or(1000.0);
                widths.insert(new_gid, width);
            }
        }
        // `subsetter` drops the cmap and renumbers glyphs; the remapped ID is
        // the CID. If subsetting ever fails, keep the whole program and the
        // original glyph IDs so the text still renders.
        let program = match subsetter::subset(ttf, 0, &remapper) {
            Ok(program) => program,
            Err(_) => {
                glyphs.clear();
                widths.clear();
                for ch in chars {
                    if let Some(glyph) = face.glyph_index(*ch) {
                        glyphs.insert(*ch, glyph.0);
                        let width = face
                            .glyph_hor_advance(glyph)
                            .map(|advance| f64::from(advance) * 1000.0 / upem)
                            .unwrap_or(1000.0);
                        widths.insert(glyph.0, width);
                    }
                }
                ttf.to_vec()
            }
        };
        Ok(Self {
            program,
            glyphs,
            widths,
        })
    }

    /// Encode `text` as 2-byte CIDs of the subset program.
    fn encode(&self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() * 2);
        for ch in text.chars() {
            let gid = self.glyphs.get(&ch).copied().unwrap_or(0);
            out.push((gid >> 8) as u8);
            out.push((gid & 0xFF) as u8);
        }
        out
    }

    /// Advance of `text` in this font, in text space.
    fn advance(&self, text: &str, size: f64, state: &TextState<'_>) -> f64 {
        let mut advance = 0.0;
        for ch in text.chars() {
            let width = self
                .glyphs
                .get(&ch)
                .and_then(|gid| self.widths.get(gid))
                .copied()
                .unwrap_or(1000.0);
            advance += width / 1000.0 * size + state.char_spacing;
            if ch == ' ' {
                advance += state.word_spacing;
            }
        }
        advance * state.scale / 100.0
    }
}

/// Shows on one visual line: the same baseline, chained left to right without
/// a column-sized gap. Returns the flat indices of the chained shows (sorted
/// by x), the right edge of the chain and the right boundary the chain may
/// grow to (the column's right margin), all in page space.
fn visual_chain(
    page: &PageData,
    flat: &[(usize, usize)],
    affected_flat: &[usize],
) -> (Vec<usize>, f64, f64) {
    let show_at = |index: usize| -> &Show {
        let (line, show) = flat[index];
        &page.lines[line].shows[show]
    };
    let Some(first) = affected_flat.first().map(|index| show_at(*index)) else {
        return (Vec::new(), 0.0, f64::MAX);
    };
    let (_, base_y) = first.origin();
    let size_user = (first.size * first.scale_x()).max(1.0);
    let tolerance = (0.45 * size_user).max(2.0);

    let mut same_line: Vec<usize> = (0..flat.len())
        .filter(|index| (show_at(*index).origin().1 - base_y).abs() <= tolerance)
        .collect();
    same_line.sort_by(|a, b| {
        show_at(*a)
            .origin()
            .0
            .partial_cmp(&show_at(*b).origin().0)
            .unwrap_or(Ordering::Equal)
    });

    let match_left = affected_flat
        .iter()
        .map(|index| show_at(*index).origin().0)
        .fold(f64::MAX, f64::min);
    let match_right = affected_flat
        .iter()
        .map(|index| show_at(*index).end().0)
        .fold(f64::MIN, f64::max);

    // Extend from the match to the right while gaps stay word-sized.
    let gap_limit = (1.5 * size_user).max(6.0);
    let mut chain: Vec<usize> = Vec::new();
    let mut right = match_right;
    let mut next_column_x: Option<f64> = None;
    for index in &same_line {
        let show = show_at(*index);
        let (x, _) = show.origin();
        if affected_flat.contains(index) || (x >= match_left - 0.5 && x <= right + gap_limit) {
            chain.push(*index);
            right = right.max(show.end().0);
        } else if x > right && next_column_x.is_none() {
            next_column_x = Some(x);
        }
    }

    // The column's right margin: page edge minus the left margin, or the
    // start of the next column on this baseline, whichever comes first.
    let left_edge = flat
        .iter()
        .map(|(line, show)| page.lines[*line].shows[*show].origin().0)
        .fold(f64::MAX, f64::min);
    let left_margin = (left_edge - page.mediabox.0).max(0.0);
    let mut boundary = page.mediabox.2 - left_margin;
    if let Some(x) = next_column_x {
        boundary = boundary.min(x - 2.0);
    }
    if boundary < right {
        boundary = right;
    }
    (chain, right, boundary)
}

/// Reserialize `Tj`/`TJ` operator `(stream, operator)` with the matched bytes
/// removed and a `TJ` adjustment inserted where the matched text was, so any
/// suffix in the same operator keeps its distance from the new text. Used for
/// substitute-font replacements inside one operator. Returns the new operator
/// bytes, or `None` for operators that cannot be rewritten this way.
fn rewrite_operator_with_adjustment(
    page: &PageData,
    line: &Line,
    operator: usize,
    affected: &[(usize, usize, usize)],
    adj_advance: f64,
) -> Option<Vec<u8>> {
    let op = page.operators.get(line.stream)?.get(operator)?;
    if !(op.is("Tj") || op.is("TJ")) {
        return None;
    }
    let elements: Vec<Operand> = if op.is("TJ") {
        match op.operands.first() {
            Some(Operand::Array(items)) => items.clone(),
            _ => return None,
        }
    } else {
        vec![op.operands.first()?.clone()]
    };

    // Edited bytes and split point (in original bytes) per affected element.
    let mut edits: HashMap<usize, (Vec<u8>, usize)> = HashMap::new();
    let mut size = 0.0f64;
    for (show_index, from, to) in affected {
        let show = &line.shows[*show_index];
        if show.operator != operator {
            continue;
        }
        size = show.size;
        let Some(Operand::Str(strval)) = operand_of(op, show.operand) else {
            continue;
        };
        let range = decoded_range(show, *from, *to);
        let edited = content::splice(&strval.bytes, &[(range.clone(), Vec::new())]);
        edits.insert(show.operand, (edited, range.start));
    }
    if edits.is_empty() {
        return None;
    }
    let adj_number = if size > 0.0 && line.scale > 0.0 {
        -adj_advance * 1000.0 / (size * line.scale / 100.0)
    } else {
        0.0
    };

    let mut items: Vec<Operand> = Vec::new();
    let mut inserted = false;
    for (index, element) in elements.into_iter().enumerate() {
        if let Operand::Str(strval) = &element
            && let Some((edited, split)) = edits.get(&index)
        {
            let make_str = |bytes: &[u8]| {
                Operand::Str(StrVal {
                    bytes: bytes.to_vec(),
                    span: 0..0,
                    hex: strval.hex,
                })
            };
            if inserted {
                items.push(make_str(edited));
            } else {
                items.push(make_str(&edited[..(*split).min(edited.len())]));
                items.push(Operand::Number(adj_number));
                items.push(make_str(&edited[(*split).min(edited.len())..]));
                inserted = true;
            }
        } else {
            items.push(element);
        }
    }
    if !inserted {
        return None;
    }
    Some(serialize_operator(&Operator {
        keyword: b"TJ".to_vec(),
        operands: vec![Operand::Array(items)],
        start: 0,
        end: 0,
    }))
}

/// Reserialize a `Tm`/`Td`/`TD` operator shifted horizontally by `delta`.
fn adjust_position_operator(op: &Operator, delta: f64) -> Option<Vec<u8>> {
    let mut shifted = op.clone();
    if op.is("Tm") {
        let matrix = op.tail_numbers::<6>()?;
        let moved = mat_mul(&translate(delta, 0.0), &matrix);
        let len = shifted.operands.len();
        for (offset, value) in moved.iter().enumerate() {
            shifted.operands[len - 6 + offset] = Operand::Number(*value);
        }
    } else if op.is("Td") || op.is("TD") {
        let [tx, ty] = op.tail_numbers::<2>()?;
        let len = shifted.operands.len();
        shifted.operands[len - 2] = Operand::Number(tx + delta);
        shifted.operands[len - 1] = Operand::Number(ty);
    } else {
        return None;
    }
    Some(serialize_operator(&shifted))
}

#[allow(clippy::too_many_arguments)]
fn apply_matches(
    doc: &mut Document,
    page_id: ObjectId,
    page: &mut PageData,
    _find: &str,
    replacement: &str,
    matches: &[Match],
    substitutes: &SubstituteFonts,
    warnings: &mut Vec<Msg>,
) -> Result<(), Error> {
    let page_no = page_number(doc, page_id);

    // Every show of the page in stream order, plus a lookup by (line, show).
    let mut flat: Vec<(usize, usize)> = Vec::new();
    for (line_index, line) in page.lines.iter().enumerate() {
        for show_index in 0..line.shows.len() {
            flat.push((line_index, show_index));
        }
    }
    flat.sort_by_key(|(line, show)| {
        let item = &page.lines[*line].shows[*show];
        (item.stream, item.operator, item.operand)
    });
    let flat_index: HashMap<(usize, usize), usize> = flat
        .iter()
        .enumerate()
        .map(|(index, key)| (*key, index))
        .collect();

    // Pre-pass: which matches need a substitute font, in which style, with
    // which characters. Subsets are built once per style and page.
    let mut subset_chars: BTreeMap<(bool, bool), std::collections::BTreeSet<char>> =
        BTreeMap::new();
    for matched in matches {
        let line = &page.lines[matched.line];
        let ranges = char_range(line);
        let affected = affected_of(line, matched, &ranges);
        let Some(first) = affected.first() else {
            continue;
        };
        let first_show = &line.shows[first.0];
        if first_show.font.encode(replacement).is_none() {
            let style = (first_show.font.is_bold(), first_show.font.is_italic());
            subset_chars
                .entry(style)
                .or_default()
                .extend(replacement.chars());
        }
    }
    let mut subsets: HashMap<(bool, bool), SubsetFont> = HashMap::new();
    for (style, chars) in &subset_chars {
        let ttf = substitutes.for_style(style.0, style.1);
        if ttf.is_empty() {
            return Err(Error::FontNotFound);
        }
        subsets.insert(*style, SubsetFont::build(ttf, chars)?);
    }

    // A subset's glyph IDs are local to this edit. Never reuse a resource
    // installed by an earlier edit, even when the face/style is the same.
    let mut resources = HashMap::new();
    for style in subsets.keys() {
        let (base, _) = substitute_names(style.0, style.1);
        let base = String::from_utf8_lossy(base);
        let mut serial = 0u64;
        let resource = loop {
            let name = format!("{base}_{serial}");
            let form_uses_name = page.stream_ids.iter().skip(1).any(|id| {
                doc.get_object(*id)
                    .and_then(Object::as_stream)
                    .and_then(|stream| stream.dict.get_deref(b"Resources", doc))
                    .and_then(Object::as_dict)
                    .and_then(|resources| resources.get_deref(b"Font", doc))
                    .and_then(Object::as_dict)
                    .is_ok_and(|fonts| fonts.has(name.as_bytes()))
            });
            if !page_has_font(doc, page_id, name.as_bytes()) && !form_uses_name {
                break name;
            }
            serial += 1;
        };
        resources.insert(*style, resource);
    }

    // Per stream: byte edits to stream content, plus decoded-space edits per show.
    let mut show_edits: HashMap<(usize, usize, usize), Vec<Edit>> = HashMap::new();
    let mut stream_edits: HashMap<usize, Vec<Edit>> = HashMap::new();
    let mut insertions: BTreeMap<(usize, usize), Vec<u8>> = BTreeMap::new();
    // Whole-operator rewrites (substitute compensation) by (stream, operator).
    let mut operator_rewrites: HashMap<(usize, usize), Vec<u8>> = HashMap::new();
    // Horizontal shifts for positioning operators, by (stream, operator).
    let mut pos_adjust: HashMap<(usize, usize), f64> = HashMap::new();
    let mut substitute_needed = false;
    let mut overflow_warned = false;

    for matched in matches {
        let line = &page.lines[matched.line];
        let ranges = char_range(line);
        let affected = affected_of(line, matched, &ranges);
        if affected.is_empty() {
            continue;
        }
        let first_show_index = affected[0].0;
        let first_show = &line.shows[first_show_index];
        let encoded = first_show.font.encode(replacement);
        let state = line_state(line);

        let old_advance: f64 = affected
            .iter()
            .map(|(show_index, from, to)| {
                segment_advance(&line.shows[*show_index], *from, *to, &state)
            })
            .sum();

        let style = (first_show.font.is_bold(), first_show.font.is_italic());
        let new_advance_raw = match &encoded {
            Some(_) => advance_width(&first_show.font, replacement, first_show.size, &state),
            None => subsets
                .get(&style)
                .map(|subset| subset.advance(replacement, first_show.size, &state))
                .unwrap_or(0.0),
        };

        // Where this visual line ends and how far it may grow.
        let affected_flat: Vec<usize> = affected
            .iter()
            .filter_map(|(show_index, _, _)| flat_index.get(&(matched.line, *show_index)).copied())
            .collect();
        let (chain, line_right, boundary) = visual_chain(page, &flat, &affected_flat);
        let scale_x = first_show.scale_x();
        let scale_x = if scale_x > 0.0 { scale_x } else { 1.0 };

        // Compress horizontally (`Tz`, at most down to 80%) when the grown
        // line would cross the column's right margin.
        let line_scale = if line.scale > 0.0 { line.scale } else { 100.0 };
        let mut tz_percent = line_scale;
        let mut new_advance = new_advance_raw;
        let delta_raw = new_advance_raw - old_advance;
        if delta_raw > 0.0 && new_advance_raw > 0.0 {
            let overflow = line_right + delta_raw * scale_x - boundary;
            if overflow > 0.5 {
                let allowed_new = new_advance_raw - overflow / scale_x;
                let factor = (allowed_new / new_advance_raw).max(0.0);
                tz_percent = (line_scale * factor).clamp(80.0_f64.min(line_scale), line_scale);
                new_advance = new_advance_raw * tz_percent / line_scale;
                if line_right + (new_advance - old_advance) * scale_x > boundary + 0.5
                    && !overflow_warned
                {
                    overflow_warned = true;
                    warnings.push(Msg::with(
                        MessageCode::PdfLineOverflow,
                        params(&[("page", page_no.into())]),
                    ));
                }
            }
        }
        let delta = new_advance - old_advance;
        let compressed = tz_percent < line_scale - 0.01;

        // Operators holding the matched text, in order.
        let mut affected_ops: Vec<usize> = Vec::new();
        for (show_index, _, _) in &affected {
            let operator = line.shows[*show_index].operator;
            if !affected_ops.contains(&operator) {
                affected_ops.push(operator);
            }
        }

        let mut compensated_in_stream = false;
        let is_substitute = encoded.is_none();
        match encoded {
            Some(bytes) => {
                // Variant A: encode with the original font.
                for (order, (show_index, from, to)) in affected.iter().enumerate() {
                    let show = &line.shows[*show_index];
                    let range = decoded_range(show, *from, *to);
                    let payload = if order == 0 {
                        bytes.clone()
                    } else {
                        Vec::new()
                    };
                    show_edits
                        .entry((show.stream, show.operator, show.operand))
                        .or_default()
                        .push((range, payload));
                }
                if compressed
                    && let (Some(first_op), Some(last_op)) = (
                        affected_ops
                            .first()
                            .and_then(|op| page.operators[line.stream].get(*op)),
                        affected_ops
                            .last()
                            .and_then(|op| page.operators[line.stream].get(*op)),
                    )
                {
                    insertions
                        .entry((line.stream, first_op.start))
                        .or_default()
                        .extend(format!("\n{} Tz\n", fmt_num(tz_percent)).into_bytes());
                    insertions
                        .entry((line.stream, last_op.end))
                        .or_default()
                        .extend(format!("\n{} Tz\n", fmt_num(line_scale)).into_bytes());
                }
            }
            None => {
                // Variant B: the original font has no glyphs for the new text.
                let Some(subset) = subsets.get(&style) else {
                    return Err(Error::FontNotFound);
                };
                let position = if replacement.is_empty() {
                    Some(0)
                } else {
                    end_of_text(page, line)
                };
                let Some(position) = position else {
                    warnings.push(Msg::with(
                        MessageCode::PdfSubstituteUnplaced,
                        params(&[("page", page_no.into())]),
                    ));
                    continue;
                };
                substitute_needed = true;

                // Text after the match inside the same operator cannot move
                // on its own; rewrite that operator with a `TJ` adjustment.
                let (last_index, _, last_to) = affected[affected.len() - 1];
                let last_show = &line.shows[last_index];
                let suffix_in_last = last_to < last_show.decoded.chars.len();
                let later_in_affected_op = line.shows.iter().enumerate().any(|(index, show)| {
                    index > last_index && affected_ops.contains(&show.operator)
                });
                let mut rewritten: Vec<usize> = Vec::new();
                if suffix_in_last || later_in_affected_op {
                    if affected_ops.len() == 1 {
                        let operator = affected_ops[0];
                        if !operator_rewrites.contains_key(&(line.stream, operator))
                            && let Some(bytes) = rewrite_operator_with_adjustment(
                                page,
                                line,
                                operator,
                                &affected,
                                new_advance,
                            )
                        {
                            operator_rewrites.insert((line.stream, operator), bytes);
                            rewritten.push(operator);
                            compensated_in_stream = true;
                        }
                    } else if suffix_in_last {
                        // Rewrite only the last operator: its suffix must sit
                        // after the new text, so the adjustment covers the
                        // matched part inside it plus the width change.
                        let operator = last_show.operator;
                        let in_last_op: Vec<(usize, usize, usize)> = affected
                            .iter()
                            .copied()
                            .filter(|(show_index, _, _)| {
                                line.shows[*show_index].operator == operator
                            })
                            .collect();
                        let matched_in_last: f64 = in_last_op
                            .iter()
                            .map(|(show_index, from, to)| {
                                segment_advance(&line.shows[*show_index], *from, *to, &state)
                            })
                            .sum();
                        if !operator_rewrites.contains_key(&(line.stream, operator))
                            && let Some(bytes) = rewrite_operator_with_adjustment(
                                page,
                                line,
                                operator,
                                &in_last_op,
                                matched_in_last + delta,
                            )
                        {
                            operator_rewrites.insert((line.stream, operator), bytes);
                            rewritten.push(operator);
                            compensated_in_stream = true;
                        }
                    }
                }
                for (show_index, from, to) in &affected {
                    let show = &line.shows[*show_index];
                    if rewritten.contains(&show.operator) {
                        continue;
                    }
                    let range = decoded_range(show, *from, *to);
                    show_edits
                        .entry((show.stream, show.operator, show.operand))
                        .or_default()
                        .push((range, Vec::new()));
                }
                if !replacement.is_empty() {
                    let resource = resources.get(&style).ok_or(Error::FontNotFound)?;
                    let tm = tm_at(line, first_show_index, affected[0].1);
                    let insert = substitute_ops(
                        line,
                        &tm,
                        replacement,
                        subset,
                        first_show.size,
                        resource,
                        compressed.then_some(tz_percent),
                    );
                    insertions
                        .entry((line.stream, position))
                        .or_default()
                        .extend(insert);
                }
            }
        }

        // Alternate extraction text belongs to the original glyphs. Remove
        // it from enclosing inline property dictionaries after editing them;
        // consumers must derive text from the new glyphs/ToUnicode instead.
        for (show_index, _, _) in &affected {
            let show = &line.shows[*show_index];
            let mut scopes = Vec::new();
            for (index, op) in page.operators[show.stream]
                .iter()
                .enumerate()
                .take(show.operator)
            {
                if op.is("BDC") || op.is("BMC") {
                    scopes.push(index);
                }
                if op.is("EMC") {
                    scopes.pop();
                }
            }
            for index in scopes {
                let op = &page.operators[show.stream][index];
                if !op.is("BDC") {
                    continue;
                }
                let source = &page.streams[show.stream][op.start..op.end];
                let mut parsed = lopdf::content::Content::decode(source)?;
                let mut changed = false;
                for operation in &mut parsed.operations {
                    for operand in &mut operation.operands {
                        if let Object::Dictionary(dict) = operand
                            && dict.has(b"ActualText")
                        {
                            dict.remove(b"ActualText");
                            changed = true;
                        }
                    }
                }
                if changed {
                    operator_rewrites.insert((show.stream, index), parsed.encode()?);
                }
            }
        }

        // Shift the rest of this visual line by the width difference. Shows
        // drawn after the match in the same operator move by themselves
        // (variant A) or through the rewrite (variant B); the first show in a
        // later operator either carries a positioning operator we can shift
        // (`Td` chains and absolute `Tm`) or relies on natural advance.
        if delta.abs() > 1e-9 && !affected_flat.is_empty() {
            let last_affected_flat = affected_flat.iter().copied().max().unwrap_or(0);
            let mut chain_in_stream_order: Vec<usize> = chain
                .iter()
                .copied()
                .filter(|index| *index > last_affected_flat)
                .collect();
            chain_in_stream_order.sort_unstable();
            let mut adjusted: HashSet<(usize, usize)> = HashSet::new();
            let mut first_next_seen = false;
            for index in chain_in_stream_order {
                let (line_index, show_index) = flat[index];
                let show = &page.lines[line_index].shows[show_index];
                if show.stream == line.stream && affected_ops.contains(&show.operator) {
                    continue;
                }
                let pos_operator = show
                    .pos_op
                    .and_then(|pos| page.operators[show.stream].get(pos));
                if let Some(pos_op) = show.pos_op
                    && pos_operator.is_some_and(|op| op.is("Tm"))
                {
                    if adjusted.insert((show.stream, pos_op)) {
                        *pos_adjust.entry((show.stream, pos_op)).or_insert(0.0) += delta;
                    }
                    first_next_seen = true;
                    continue;
                }
                if first_next_seen {
                    continue;
                }
                first_next_seen = true;
                match (show.pos_op, pos_operator) {
                    (Some(pos_op), Some(op)) if op.is("Td") || op.is("TD") => {
                        let mostly_horizontal = op
                            .tail_numbers::<2>()
                            .map(|[_, ty]| ty.abs() <= 0.01)
                            .unwrap_or(false);
                        if mostly_horizontal && adjusted.insert((show.stream, pos_op)) {
                            *pos_adjust.entry((show.stream, pos_op)).or_insert(0.0) += delta;
                        }
                    }
                    _ => {
                        // Positioned only by the natural advance of the
                        // previous text: variant A and in-stream-compensated
                        // variant B already moved it. Otherwise pin it with
                        // an absolute `Tm` at its shifted position.
                        if is_substitute && !compensated_in_stream {
                            let moved = mat_mul(&translate(delta, 0.0), &show.tm);
                            let mut bytes = vec![b'\n'];
                            bytes.extend_from_slice(&serialize_operator(&Operator {
                                keyword: b"Tm".to_vec(),
                                operands: moved
                                    .iter()
                                    .map(|value| Operand::Number(*value))
                                    .collect(),
                                start: 0,
                                end: 0,
                            }));
                            if let Some(op) = page.operators[show.stream].get(show.operator) {
                                insertions
                                    .entry((show.stream, op.start))
                                    .or_default()
                                    .extend(bytes);
                            }
                        }
                    }
                }
            }
        }
    }

    // Materialise decoded-space edits per show.
    for ((stream_index, operator, operand), edits) in show_edits {
        if operator_rewrites.contains_key(&(stream_index, operator)) {
            continue;
        }
        let Some(op) = page
            .operators
            .get(stream_index)
            .and_then(|ops| ops.get(operator))
        else {
            continue;
        };
        let Some(Operand::Str(strval)) = operand_of(op, operand) else {
            continue;
        };
        let mut sorted = edits;
        sorted.sort_by_key(|(range, _)| range.start);
        let mut combined: Vec<(Range<usize>, Vec<u8>)> = Vec::new();
        for (range, payload) in sorted {
            if let Some(last) = combined.last_mut()
                && range.start < last.0.end
            {
                continue;
            }
            combined.push((range, payload));
        }
        let bytes = content::splice(&strval.bytes, &combined);
        // Replace only the content between the delimiters, not the delimiters.
        let new_content = if strval.hex {
            content::hex_content(&bytes)
        } else {
            content::escape_literal(&bytes)
        };
        stream_edits
            .entry(stream_index)
            .or_default()
            .push((strval.span.clone(), new_content));
    }

    // Whole-operator rewrites and positioning-operator shifts.
    for ((stream_index, operator), bytes) in &operator_rewrites {
        if let Some(op) = page
            .operators
            .get(*stream_index)
            .and_then(|ops| ops.get(*operator))
        {
            stream_edits
                .entry(*stream_index)
                .or_default()
                .push((op.start..op.end, bytes.clone()));
        }
    }
    for ((stream_index, operator), delta) in &pos_adjust {
        if delta.abs() <= 1e-9 {
            continue;
        }
        if let Some(op) = page
            .operators
            .get(*stream_index)
            .and_then(|ops| ops.get(*operator))
            && let Some(bytes) = adjust_position_operator(op, *delta)
        {
            stream_edits
                .entry(*stream_index)
                .or_default()
                .push((op.start..op.end, bytes));
        }
    }

    // Inserted operators and substitute drawings.
    for ((stream_index, position), bytes) in insertions {
        stream_edits
            .entry(stream_index)
            .or_default()
            .push((position..position, bytes));
    }

    if substitute_needed {
        warnings.push(Msg::with(
            MessageCode::PdfSubstituteFont,
            params(&[("page", page_no.into())]),
        ));
    }

    if stream_edits.is_empty() {
        return Ok(());
    }

    for (stream_index, mut edits) in stream_edits {
        edits.sort_by_key(|(range, _)| range.start);
        let Some(source) = page.streams.get(stream_index) else {
            continue;
        };
        if stream_index == 0 {
            // Normally an edit stays inside one original Contents stream. Keep
            // every other stream byte-for-byte; a token spanning a boundary
            // instead requires a joined replacement stream on this page only.
            let local = edits.iter().all(|(edit, _)| {
                page.contents_origins
                    .iter()
                    .any(|(_, range)| edit.start >= range.start && edit.end <= range.end)
            });
            if local {
                let mut ids = Vec::new();
                for (id, range) in &page.contents_origins {
                    let changes: Vec<Edit> = edits
                        .iter()
                        .filter(|(edit, _)| edit.start >= range.start && edit.end <= range.end)
                        .map(|(edit, bytes)| {
                            (
                                (edit.start - range.start)..(edit.end - range.start),
                                bytes.clone(),
                            )
                        })
                        .collect();
                    if changes.is_empty() {
                        ids.push(Object::Reference(*id));
                    } else {
                        let bytes = content::splice(&source[range.clone()], &changes);
                        ids.push(Object::Reference(
                            doc.add_object(Stream::new(Dictionary::new(), bytes)),
                        ));
                    }
                }
                doc.get_dictionary_mut(page_id)?.set("Contents", ids);
            } else {
                let bytes = content::splice(source, &edits);
                let id = doc.add_object(Stream::new(Dictionary::new(), bytes));
                doc.get_dictionary_mut(page_id)?.set("Contents", id);
            }
        } else {
            let new_bytes = content::splice(source, &edits);
            let Some(id) = page.stream_ids.get(stream_index).copied() else {
                continue;
            };
            let mut stream = doc.get_object(id)?.as_stream()?.clone();
            stream.dict.remove(b"Filter");
            stream.dict.remove(b"DecodeParms");
            stream.set_content(new_bytes);
            doc.set_object(id, Object::Stream(stream));
        }
    }

    if substitute_needed {
        for (style, subset) in &subsets {
            let ttf = substitutes.for_style(style.0, style.1);
            let resource = resources.get(style).ok_or(Error::FontNotFound)?;
            embed_substitute(doc, page_id, *style, resource.as_bytes(), subset, ttf)?;
        }
        // Form-local Resources shadow the page dictionary. Install the new
        // subset names in each private invocation without replacing its fonts.
        let subset_resources: Vec<_> = resources.values().cloned().collect();
        let resources = page_resources(doc, page_id)?;
        let page_fonts = resources.get_deref(b"Font", doc)?.as_dict()?.clone();
        for id in page
            .stream_ids
            .iter()
            .skip(1)
            .copied()
            .collect::<HashSet<_>>()
        {
            let mut form = doc.get_object(id)?.as_stream()?.clone();
            let mut resources = form.dict.get_deref(b"Resources", doc)?.as_dict()?.clone();
            let mut fonts = match resources.get(b"Font") {
                Ok(value) => doc.dereference(value)?.1.as_dict()?.clone(),
                Err(_) => Dictionary::new(),
            };
            for name in &subset_resources {
                fonts.set(name.as_bytes(), page_fonts.get(name.as_bytes())?.clone());
            }
            resources.set("Font", fonts);
            form.dict.set("Resources", resources);
            doc.set_object(id, form);
        }
    }
    Ok(())
}

fn decoded_range(show: &Show, from: usize, to: usize) -> Range<usize> {
    let start = show
        .decoded
        .chars
        .get(from)
        .map(|(start, _)| *start)
        .unwrap_or(0);
    let end = show
        .decoded
        .chars
        .get(to.saturating_sub(1))
        .map(|(_, end)| *end)
        .unwrap_or(start);
    start..end
}

fn end_of_text(page: &PageData, line: &Line) -> Option<usize> {
    page.operators
        .get(line.stream)?
        .iter()
        .enumerate()
        .skip(line.bt + 1)
        .find_map(|(index, op)| op.is("ET").then_some(index))
        .and_then(|index| page.operators.get(line.stream)?.get(index))
        .map(|op| op.end)
}

/// Text matrix at `char_index` inside `show_index` of a line.
fn tm_at(line: &Line, show_index: usize, char_index: usize) -> Mat {
    let Some(show) = line.shows.get(show_index) else {
        return IDENTITY;
    };
    let state = line_state(line);
    let advance = segment_advance(show, 0, char_index, &state);
    mat_mul(&translate(advance, 0.0), &show.tm)
}

/// Build the `BT … ET` block that draws `text` in the substitute font. The
/// block starts and ends with a newline so it can be spliced anywhere without
/// ever gluing two operators together.
#[allow(clippy::too_many_arguments)]
fn substitute_ops(
    line: &Line,
    tm: &Mat,
    text: &str,
    subset: &SubsetFont,
    size: f64,
    resource: &str,
    tz_percent: Option<f64>,
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\nBT\n");
    for fill in &line.fill {
        out.extend_from_slice(&serialize_operator(fill));
    }
    out.extend_from_slice(format!("/{resource} {} Tf\n", fmt_num(size)).as_bytes());
    if let Some(tz) = tz_percent {
        out.extend_from_slice(format!("{} Tz\n", fmt_num(tz)).as_bytes());
    }
    out.extend_from_slice(&serialize_operator(&Operator {
        keyword: b"Tm".to_vec(),
        operands: tm.iter().map(|value| Operand::Number(*value)).collect(),
        start: 0,
        end: 0,
    }));
    out.extend_from_slice(&serialize_operator(&Operator {
        keyword: b"Tj".to_vec(),
        operands: vec![Operand::Str(StrVal {
            bytes: subset.encode(text),
            span: 0..0,
            hex: false,
        })],
        start: 0,
        end: 0,
    }));
    // `Tz` is part of the text state and survives `ET`; restore the line's
    // own horizontal scaling so later text on the page is not squeezed.
    if tz_percent.is_some() {
        let restore = if line.scale > 0.0 { line.scale } else { 100.0 };
        out.extend_from_slice(format!("{} Tz\n", fmt_num(restore)).as_bytes());
    }
    out.extend_from_slice(b"ET\n");
    out
}

fn page_number(doc: &Document, page_id: ObjectId) -> u32 {
    doc.get_pages()
        .into_iter()
        .find_map(|(number, id)| (id == page_id).then_some(number))
        .unwrap_or(0)
}

/// Embed a subset substitute Type0 font into the page's resources (once per
/// style). Only the characters actually drawn are in the program, the
/// `ToUnicode` map and the width table, so the file grows by a few kilobytes.
fn embed_substitute(
    doc: &mut Document,
    page_id: ObjectId,
    style: (bool, bool),
    resource: &[u8],
    subset: &SubsetFont,
    original_ttf: &[u8],
) -> Result<(), Error> {
    let (_, base_name) = substitute_names(style.0, style.1);
    let face = ttf_parser::Face::parse(original_ttf, 0).map_err(|error| {
        Error::invalid(
            MessageCode::FontParseSubstitute,
            params(&[("detail", error.to_string().into())]),
        )
    })?;
    let upem = f64::from(face.units_per_em().max(1));
    let units = |value: i16| f64::from(value) * 1000.0 / upem;
    let bbox = face.global_bounding_box();
    let mut program_stream = font_file_stream(&subset.program);
    let _ = program_stream.compress();
    let font_file_id = doc.add_object(Object::Stream(program_stream));

    // FontDescriptor flags: bit 6 (32) = nonsymbolic, bit 7 (64) = italic,
    // bit 19 (0x40000) = force bold.
    let mut flags: i64 = 32;
    if style.1 {
        flags |= 64;
    }
    if style.0 {
        flags |= 0x40000;
    }
    let descriptor_id = doc.add_object(Object::Dictionary(dictionary! {
        "Type" => "FontDescriptor",
        "FontName" => Object::Name(base_name.as_bytes().to_vec()),
        "Flags" => flags,
        "FontBBox" => vec![
            Object::Integer(units(bbox.x_min) as i64),
            Object::Integer(units(bbox.y_min) as i64),
            Object::Integer(units(bbox.x_max) as i64),
            Object::Integer(units(bbox.y_max) as i64),
        ],
        "ItalicAngle" => if style.1 { -12 } else { 0 },
        "Ascent" => units(face.ascender()) as i64,
        "Descent" => units(face.descender()) as i64,
        "CapHeight" => units(face.capital_height().unwrap_or(face.ascender())) as i64,
        "StemV" => if style.0 { 120 } else { 80 },
        "FontFile2" => font_file_id,
    }));

    let mut mappings: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    let mut widths: Vec<Object> = Vec::new();
    for (ch, gid) in &subset.glyphs {
        mappings
            .entry(u32::from(*gid))
            .or_insert_with(|| vec![*ch as u32]);
        let width = subset.widths.get(gid).copied().unwrap_or(1000.0);
        widths.push(Object::Integer(i64::from(*gid)));
        widths.push(Object::Array(vec![Object::Real(width as f32)]));
    }
    let cmap = ToUnicodeCMap { mappings };
    let cmap_text = cmap.to_cmap_string(base_name);
    let to_unicode_id = doc.add_object(Object::Stream(Stream::new(
        Dictionary::new(),
        cmap_text.into_bytes(),
    )));

    let cid_id = doc.add_object(Object::Dictionary(dictionary! {
        "Type" => "Font",
        "Subtype" => "CIDFontType2",
        "BaseFont" => Object::Name(base_name.as_bytes().to_vec()),
        "CIDSystemInfo" => dictionary! {
            "Registry" => Object::string_literal("Adobe"),
            "Ordering" => Object::string_literal("Identity"),
            "Supplement" => 0,
        },
        "FontDescriptor" => descriptor_id,
        "DW" => 1000,
        "W" => widths,
        "CIDToGIDMap" => "Identity",
    }));

    let font_id = doc.add_object(Object::Dictionary(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type0",
        "BaseFont" => Object::Name(base_name.as_bytes().to_vec()),
        "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Reference(cid_id)],
        "ToUnicode" => to_unicode_id,
    }));

    add_page_font(doc, page_id, resource.to_vec(), font_id)?;
    Ok(())
}

fn font_file_stream(ttf: &[u8]) -> Stream {
    let mut dict = Dictionary::new();
    dict.set("Length1", ttf.len() as i64);
    Stream::new(dict, ttf.to_vec())
}

fn page_has_font(doc: &Document, page_id: ObjectId, name: &[u8]) -> bool {
    doc.get_page_fonts(page_id)
        .map(|fonts| fonts.contains_key(name))
        .unwrap_or(false)
}

fn add_page_font(
    doc: &mut Document,
    page_id: ObjectId,
    name: Vec<u8>,
    font_id: ObjectId,
) -> Result<(), Error> {
    let resources_id = ensure_indirect_dict(doc, page_id, b"Resources")?;
    let fonts_id = match doc
        .get_dictionary(resources_id)
        .ok()
        .and_then(|resources| resources.get(b"Font").ok())
        .and_then(|value| value.as_reference().ok())
        .filter(|id| doc.get_dictionary(*id).is_ok())
    {
        Some(id) => id,
        None => {
            let existing = doc
                .get_dictionary(resources_id)
                .ok()
                .and_then(|resources| resources.get(b"Font").ok())
                .and_then(|value| value.as_dict().ok())
                .cloned()
                .unwrap_or_default();
            let id = doc.add_object(Object::Dictionary(existing));
            let resources = doc.get_dictionary_mut(resources_id).map_err(Error::Pdf)?;
            resources.set("Font", Object::Reference(id));
            id
        }
    };
    let fonts = doc.get_dictionary_mut(fonts_id).map_err(Error::Pdf)?;
    fonts.set(name, Object::Reference(font_id));
    Ok(())
}

/// Make sure `key` on the page refers to an indirect dictionary, and return it.
fn ensure_indirect_dict(
    doc: &mut Document,
    page_id: ObjectId,
    key: &[u8],
) -> Result<ObjectId, Error> {
    enum Found {
        Reference(ObjectId),
        Direct,
        Missing,
    }
    let found = {
        let page = doc.get_dictionary(page_id).map_err(Error::Pdf)?;
        match page.get(key) {
            Ok(Object::Reference(id)) => Found::Reference(*id),
            Ok(Object::Dictionary(_)) => Found::Direct,
            _ => Found::Missing,
        }
    };
    match found {
        Found::Reference(id) => Ok(id),
        Found::Direct => {
            let dict = doc
                .get_dictionary(page_id)
                .map_err(Error::Pdf)?
                .get(key)
                .ok()
                .and_then(|value| value.as_dict().ok())
                .cloned()
                .unwrap_or_default();
            let id = doc.add_object(Object::Dictionary(dict));
            let page = doc.get_dictionary_mut(page_id).map_err(Error::Pdf)?;
            page.set(key, Object::Reference(id));
            Ok(id)
        }
        Found::Missing => {
            let id = doc.add_object(Object::Dictionary(Dictionary::new()));
            let page = doc.get_dictionary_mut(page_id).map_err(Error::Pdf)?;
            page.set(key, Object::Reference(id));
            Ok(id)
        }
    }
}

/// Resolve the device colour spaces used by ordinary PDF text.
fn fill_color(ops: &[Operator]) -> String {
    let mut rgb = [0.0; 3];
    for op in ops {
        match op.keyword.as_slice() {
            b"rg" => rgb = op.tail_numbers::<3>().unwrap_or(rgb),
            b"g" => rgb = [op.number(0).unwrap_or(0.0); 3],
            b"k" => {
                if let Some([c, m, y, k]) = op.tail_numbers::<4>() {
                    rgb = [c, m, y].map(|v| 1.0 - (v + k).min(1.0));
                }
            }
            b"sc" | b"scn" => {
                if let Some(v) = op.tail_numbers::<3>() {
                    rgb = v;
                } else if let Some(v) = op.number(0) {
                    rgb = [v; 3];
                }
            }
            _ => {}
        }
    }
    let [r, g, b] = rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    format!("{r:02X}{g:02X}{b:02X}")
}

/// All text with positions, for `read` and PDF → DOCX (kept small and internal).
pub(crate) fn positioned_lines(
    doc: &Document,
    page_id: ObjectId,
) -> Result<Vec<PositionedLine>, Error> {
    let page = load_page(doc, page_id)?;
    let mut out = Vec::new();
    for line in &page.lines {
        // Font switches inside a text object are run boundaries, not whole
        // paragraph properties. Keep those fragments separate for DOCX export.
        let mut fragments: Vec<PositionedLine> = Vec::new();
        for show in &line.shows {
            let (x, y) = show.origin();
            let size = show.size_user();
            let size = if size > 0.0 { size } else { 12.0 };
            let font = show.font.base_font();
            if let Some(last) = fragments.last_mut()
                && last.font == font
                && last.color == show.color
                && (last.size - size).abs() < 0.01
                && (last.y - y).abs() < 0.5
                && (x - last.x - last.width).abs() < size * 0.5
            {
                last.text.push_str(&show.decoded.text);
                last.width = show.end().0 - last.x;
            } else {
                fragments.push(PositionedLine {
                    text: show.decoded.text.clone(),
                    x,
                    y,
                    size,
                    font: font.to_string(),
                    color: show.color.clone(),
                    width: (show.end().0 - x).max(0.0),
                });
            }
        }
        out.extend(fragments);
    }
    Ok(out)
}

/// All text of a document, top-to-bottom and left-to-right.
pub(crate) fn checked_document_text(doc: &Document) -> Result<String, Error> {
    let mut pages = Vec::new();
    for page_id in doc.get_pages().into_values() {
        pages.push(render_page_lines(&positioned_lines(doc, page_id)?));
    }
    Ok(pages.join("\n"))
}

#[cfg(test)]
pub(crate) fn document_text(doc: &Document) -> String {
    checked_document_text(doc).unwrap_or_default()
}

fn render_page_lines(lines: &[PositionedLine]) -> String {
    let mut items: Vec<&PositionedLine> =
        lines.iter().filter(|line| !line.text.is_empty()).collect();
    items.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal))
    });
    let mut rows: Vec<(f64, Vec<(f64, String)>)> = Vec::new();
    for line in items {
        match rows.last_mut() {
            Some((row_y, fragments)) if (line.y - *row_y).abs() <= 1.5 => {
                fragments.push((line.x, line.text.clone()));
            }
            _ => rows.push((line.y, vec![(line.x, line.text.clone())])),
        }
    }
    let mut out = String::new();
    for (_, mut fragments) in rows {
        fragments.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
        for (_, text) in fragments {
            out.push_str(&text);
        }
        out.push('\n');
    }
    out
}

/// One positioned line of a PDF page.
#[derive(Debug, Clone)]
pub(crate) struct PositionedLine {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub size: f64,
    pub font: String,
    pub color: String,
    /// Width of the line in page-space points.
    pub width: f64,
}

/// One axis-aligned path segment in page space.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Segment {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

/// Every axis-aligned line segment a page actually paints, for grid
/// (table) detection. Paths that only clip (`re W* n`, as producers emit
/// for the page or a table cell) are not visible lines and are skipped:
/// segments count once a painting operator (`f`, `S`, …) consumes the path.
pub(crate) fn page_segments(doc: &Document, page_id: ObjectId) -> Result<Vec<Segment>, Error> {
    let mut out = Vec::new();
    for id in doc.get_page_contents(page_id) {
        let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
            continue;
        };
        let data = crate::pdfbudget::stream(stream)?;
        let operators = content::parse(&data)?;
        let mut ctm = IDENTITY;
        let mut stack: Vec<Mat> = Vec::new();
        let mut current: Option<(f64, f64)> = None;
        let mut pending: Vec<Segment> = Vec::new();
        for op in &operators {
            match op.keyword.as_slice() {
                b"q" => stack.push(ctm),
                b"Q" => {
                    if let Some(previous) = stack.pop() {
                        ctm = previous;
                    }
                }
                b"cm" => {
                    if let Some(values) = op.tail_numbers::<6>() {
                        ctm = mat_mul(&values, &ctm);
                    }
                }
                b"m" => {
                    if let Some([x, y]) = op.tail_numbers::<2>() {
                        current = Some(apply(&ctm, x, y));
                    }
                }
                b"l" => {
                    if let (Some(from), Some([x, y])) = (current, op.tail_numbers::<2>()) {
                        let to = apply(&ctm, x, y);
                        pending.push(Segment {
                            x0: from.0,
                            y0: from.1,
                            x1: to.0,
                            y1: to.1,
                        });
                        current = Some(to);
                    }
                }
                b"re" => {
                    if let Some([x, y, w, h]) = op.tail_numbers::<4>() {
                        let corners = [
                            apply(&ctm, x, y),
                            apply(&ctm, x + w, y),
                            apply(&ctm, x + w, y + h),
                            apply(&ctm, x, y + h),
                        ];
                        for index in 0..4 {
                            let a = corners[index];
                            let b = corners[(index + 1) % 4];
                            pending.push(Segment {
                                x0: a.0,
                                y0: a.1,
                                x1: b.0,
                                y1: b.1,
                            });
                        }
                    }
                }
                b"f" | b"F" | b"f*" | b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" => {
                    out.append(&mut pending);
                    current = None;
                }
                b"n" => {
                    pending.clear();
                    current = None;
                }
                _ => {}
            }
        }
    }
    Ok(out)
}

/// Image XObject streams referenced by a page.
pub(crate) fn page_images(doc: &Document, page_id: ObjectId) -> Result<Vec<Stream>, Error> {
    Ok(walk_page_xobjects(doc, page_id, true)?.0)
}

/// One diagnostic per skipped depth, using the source page number even for a selection.
pub(crate) fn form_warnings(doc: &Document, page_id: ObjectId) -> Result<Vec<Msg>, Error> {
    let page = doc.get_pages().into_iter().find(|(_, id)| *id == page_id)
        .map(|(number, _)| number).unwrap_or(1);
    Ok(walk_page_xobjects(doc, page_id, false)?.1.into_iter().map(|depth| {
        Msg::with(MessageCode::PdfFormSkipped,
            params(&[("page", page.into()), ("depth", depth.into())]))
    }).collect())
}

fn walk_page_xobjects(
    doc: &Document,
    page_id: ObjectId,
    collect_images: bool,
) -> Result<(Vec<Stream>, std::collections::BTreeSet<usize>), Error> {
    let resources = page_resources(doc, page_id)?;
    let mut data = Vec::new();
    for id in doc.get_page_contents(page_id) {
        data.extend(crate::pdfbudget::stream(doc.get_object(id)?.as_stream()?)?);
        data.push(b'\n');
        if data.len() > crate::pdfbudget::DOCUMENT_BYTES {
            return Err(crate::pdfbudget::limit(
                "page_bytes",
                crate::pdfbudget::DOCUMENT_BYTES,
            ));
        }
    }
    let mut bytes = data.len();
    let mut pending = vec![(data, resources, 0, HashSet::new())];
    let mut count = 0usize;
    let mut skipped = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    while let Some((data, resources, depth, active)) = pending.pop() {
        for op in content::parse(&data)? {
            if !op.is("Do") {
                continue;
            }
            let Some(name) = op.name(0) else {
                continue;
            };
            // Missing painting resources are already diagnosed by page_bounds
            // as an operator-specific warning; there is no image to preserve.
            let Ok(value) = resources.get(b"XObject") else {
                continue;
            };
            let objects = doc.dereference(value)?.1.as_dict()?;
            let Ok(value) = objects.get(name) else {
                continue;
            };
            let (id, object) = doc.dereference(value)?;
            count += 1;
            if count > crate::pdfbudget::OBJECTS {
                return Err(Error::invalid(
                    MessageCode::PdfContentLimit,
                    params(&[("kind", "xobjects".into()), ("limit", 100_000.into())]),
                ));
            }
            let stream = object.as_stream()?;
            match stream.dict.get(b"Subtype").and_then(Object::as_name)? {
                b"Image" if collect_images => {
                    bytes = bytes.saturating_add(stream.content.len());
                    if bytes > crate::pdfbudget::DOCUMENT_BYTES {
                        return Err(crate::pdfbudget::limit("expanded_page_bytes", crate::pdfbudget::DOCUMENT_BYTES));
                    }
                    out.push(stream.clone());
                }
                b"Form" => {
                    if depth >= crate::pdfbudget::DEPTH || id.is_some_and(|id| active.contains(&id)) {
                        skipped.insert(depth + 1);
                        continue;
                    }
                    let inherited = match stream.dict.get(b"Resources") {
                        Ok(value) => doc.dereference(value)?.1.as_dict()?.clone(),
                        Err(_) => resources.clone(),
                    };
                    let mut active = active.clone();
                    if let Some(id) = id { active.insert(id); }
                    let data = crate::pdfbudget::stream(stream)?;
                    bytes = bytes.saturating_add(data.len());
                    if bytes > crate::pdfbudget::DOCUMENT_BYTES {
                        return Err(crate::pdfbudget::limit("expanded_page_bytes", crate::pdfbudget::DOCUMENT_BYTES));
                    }
                    pending.push((data, inherited, depth + 1, active));
                }
                _ => {}
            }
        }
    }
    Ok((out, skipped))
}

/// Count image XObjects and line-drawing operators (used by tests).
#[cfg(test)]
pub(crate) fn page_graphics(doc: &Document, page_id: ObjectId) -> (usize, usize) {
    let images = count_image_xobjects(doc, page_id);
    let lines = doc
        .get_page_contents(page_id)
        .iter()
        .filter_map(|id| doc.get_object(*id).and_then(Object::as_stream).ok())
        .filter_map(|stream| stream.decompressed_content().ok())
        .map(|data| {
            content::parse(&data)
                .unwrap_or_default()
                .iter()
                .filter(|op| {
                    matches!(
                        op.keyword.as_slice(),
                        b"l" | b"m" | b"re" | b"c" | b"v" | b"y"
                    )
                })
                .count()
        })
        .sum();
    (images, lines)
}

#[cfg(test)]
fn count_image_xobjects(doc: &Document, _page_id: ObjectId) -> usize {
    doc.objects
        .values()
        .filter(|object| {
            object
                .as_stream()
                .ok()
                .and_then(|stream| stream.dict.get(b"Subtype").ok())
                .and_then(|value| value.as_name().ok())
                .is_some_and(|name| name == b"Image")
        })
        .count()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::pdf::{PageSetup, html_to_pdf_with_setup};

    fn png() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(8, 8, image::Rgb([10, 200, 30]));
        let mut out = std::io::Cursor::new(Vec::new());
        let _ = image::DynamicImage::ImageRgb8(image).write_to(&mut out, image::ImageFormat::Png);
        out.into_inner()
    }

    fn rich_pdf() -> Vec<u8> {
        let font = crate::pdf::load_font(None).unwrap_or_default();
        let mut images: BTreeMap<String, printpdf::Base64OrRaw> = BTreeMap::new();
        images.insert("pic".to_string(), printpdf::Base64OrRaw::Raw(png()));
        let body = "<h1>Title Line</h1>\
            <p>Zażółć gęślą jaźń.</p>\
            <table><tr><td>A</td><td>B</td></tr><tr><td>1</td><td>2</td></tr></table>\
            <div style=\"width:8px;height:8px\"><img src=\"pic\" style=\"width:8px;height:8px\" /></div>";
        let html = crate::markdown::full_html(body);
        html_to_pdf_with_setup(&html, &font, &images, &PageSetup::default()).unwrap_or_default()
    }

    fn line_key(line: &PositionedLine) -> (String, i64, i64, i64) {
        (
            line.text.clone(),
            (line.x * 10.0).round() as i64,
            (line.y * 10.0).round() as i64,
            (line.size * 10.0).round() as i64,
        )
    }

    #[test]
    fn replace_in_place_keeps_everything_else() -> Result<(), Box<dyn std::error::Error>> {
        let bytes = rich_pdf();
        let font = crate::pdf::load_font(None)?;
        let mut doc = Document::load_mem(&bytes)?;
        let page = doc.get_pages().into_values().next().ok_or("missing page")?;
        let before_graphics = page_graphics(&doc, page);
        let before_lines: Vec<PositionedLine> = positioned_lines(&doc, page).unwrap_or_default();
        assert!(
            before_graphics.1 > 4,
            "the table should draw grid lines, got {before_graphics:?}"
        );
        assert!(before_graphics.0 >= 1, "the image should be an XObject");

        let outcome = replace_in_document(
            &mut doc,
            "Zażółć",
            "żółć",
            true,
            &SubstituteFonts::all(&font),
        );
        let outcome = outcome?;
        assert_eq!(outcome.replaced, 1);
        assert_eq!(outcome.present, 1);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);

        let text = document_text(&doc);
        assert!(text.contains("żółć gęślą jaźń."), "{text:?}");
        assert!(!text.contains("Zażółć"), "{text:?}");

        let after_graphics = page_graphics(&doc, page);
        assert_eq!(after_graphics, before_graphics);
        let after_lines: Vec<PositionedLine> = positioned_lines(&doc, page).unwrap_or_default();
        for line in &before_lines {
            if line.text.contains("Zażółć") {
                continue;
            }
            assert!(
                after_lines
                    .iter()
                    .any(|other| line_key(other) == line_key(line)),
                "line {:?} moved or vanished",
                line.text
            );
        }
        Ok(())
    }

    #[test]
    fn variant_b_substitutes_font_and_warns() -> Result<(), Box<dyn std::error::Error>> {
        let bytes = rich_pdf();
        let font = crate::pdf::load_font(None)?;
        let mut doc = Document::load_mem(&bytes)?;
        let outcome = replace_in_document(
            &mut doc,
            "Zażółć",
            "Zamiast",
            true,
            &SubstituteFonts::all(&font),
        );
        let outcome = outcome?;
        assert_eq!(outcome.replaced, 1);
        assert!(
            outcome
                .warnings
                .iter()
                .any(|warning| warning.code == MessageCode::PdfSubstituteFont),
            "{:?}",
            outcome.warnings
        );
        let text = document_text(&doc);
        assert!(text.contains("Zamiast gęślą jaźń."), "{text:?}");
        assert!(!text.contains("Zażółć"), "{text:?}");
        Ok(())
    }

    #[test]
    fn replace_across_tj_elements() -> Result<(), Box<dyn std::error::Error>> {
        let bytes = rich_pdf();
        let font = crate::pdf::load_font(None)?;
        let mut doc = Document::load_mem(&bytes)?;
        // "żółć" is made of four separate TJ elements, so a match here crosses them.
        let outcome =
            replace_in_document(&mut doc, "żółć", "óć", true, &SubstituteFonts::all(&font));
        let outcome = outcome?;
        assert_eq!(outcome.replaced, 1);
        let text = document_text(&doc);
        assert!(text.contains("Zaóć"), "{text:?}");
        Ok(())
    }

    #[test]
    fn scan_without_text_is_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let font = crate::pdf::load_font(None)?;
        let mut images: BTreeMap<String, printpdf::Base64OrRaw> = BTreeMap::new();
        images.insert("pic".to_string(), printpdf::Base64OrRaw::Raw(png()));
        let body = "<div style=\"width:8px;height:8px\"><img src=\"pic\" style=\"width:8px;height:8px\" /></div>";
        let html = crate::markdown::full_html(body);
        let bytes = html_to_pdf_with_setup(&html, &font, &images, &PageSetup::default())?;
        let mut doc = Document::load_mem(&bytes)?;
        let result = replace_in_document(
            &mut doc,
            "anything",
            "else",
            true,
            &SubstituteFonts::all(&font),
        );
        assert!(matches!(result, Err(Error::Invalid(_))), "{result:?}");
        Ok(())
    }
    /// A small hand-built PDF: one Type1 font (optionally named bold) and one
    /// content stream given by the caller. Widths are absent, so advances
    /// fall back to `size * 0.5` per character (6 pt at 12 pt).
    fn synthetic_pdf(content: &str, base_font: &str) -> Vec<u8> {
        use lopdf::{Object, Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let font_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => Object::Name(base_font.as_bytes().to_vec()),
            "Encoding" => "WinAnsiEncoding",
        }));
        let content_id = doc.add_object(Object::Stream(Stream::new(
            dictionary! {},
            content.as_bytes().to_vec(),
        )));
        let resources_id = doc.add_object(Object::Dictionary(dictionary! {
            "Font" => dictionary! { "F1" => Object::Reference(font_id) },
        }));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Resources" => Object::Reference(resources_id),
            "Contents" => Object::Reference(content_id),
        }));
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![Object::Reference(page_id)],
                "Count" => 1,
            }),
        );
        let catalog_id = doc.add_object(Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        }));
        doc.trailer.set("Root", catalog_id);
        let mut out = Vec::new();
        let _ = doc.save_to(&mut out);
        out
    }

    /// `(text, x, end_x)` of every shown string on the first page, in order.
    fn fragments(doc: &Document) -> Vec<(String, f64, f64)> {
        let Some(page) = doc.get_pages().into_values().next() else {
            return Vec::new();
        };
        let Ok(data) = load_page(doc, page) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for line in &data.lines {
            for show in &line.shows {
                out.push((show.decoded.text.clone(), show.origin().0, show.end().0));
            }
        }
        out
    }

    /// D2: every content stream decodes with lopdf and holds only real PDF
    /// operators (a glued `ETBT` would show up as an unknown operator).
    fn assert_streams_valid(doc: &Document) {
        const OPERATORS: &[&str] = &[
            "b", "B", "b*", "B*", "BDC", "BI", "BMC", "BT", "BX", "c", "cm", "CS", "cs", "d", "d0",
            "d1", "Do", "DP", "EI", "EMC", "ET", "EX", "f", "F", "f*", "G", "g", "gs", "h", "i",
            "ID", "J", "j", "K", "k", "l", "m", "M", "MP", "n", "q", "Q", "re", "RG", "rg", "ri",
            "s", "S", "SC", "sc", "SCN", "scn", "sh", "T*", "Tc", "Td", "TD", "Tf", "Tj", "TJ",
            "TL", "Tm", "Tr", "Ts", "Tw", "Tz", "v", "w", "W", "W*", "y", "'", "\"",
        ];
        for page in doc.get_pages().into_values() {
            for id in doc.get_page_contents(page) {
                let Ok(stream) = doc.get_object(id).and_then(Object::as_stream) else {
                    continue;
                };
                let Ok(bytes) = stream.decompressed_content() else {
                    continue;
                };
                let decoded = lopdf::content::Content::decode(&bytes);
                assert!(decoded.is_ok(), "content stream does not decode");
                let Ok(content) = decoded else { continue };
                for operation in &content.operations {
                    assert!(
                        OPERATORS.contains(&operation.operator.as_str()),
                        "unknown operator {:?}",
                        operation.operator
                    );
                }
            }
        }
    }

    fn gap_after(frags: &[(String, f64, f64)], first: &str, second: &str) -> Option<f64> {
        let first_frag = frags.iter().find(|frag| frag.0 == first)?;
        let second_frag = frags.iter().find(|frag| frag.0.contains(second))?;
        Some(second_frag.1 - first_frag.2)
    }

    #[test]
    fn shorter_and_longer_replacement_shifts_following_fragment()
    -> Result<(), Box<dyn std::error::Error>> {
        // "Kowalski" ends at 50 + 8*6 = 98; the next fragment starts at 102,
        // so the original gap is 4 pt.
        let content = "BT /F1 12 Tf 1 0 0 1 50 700 Tm (Kowalski) Tj ET\n\
                       BT /F1 12 Tf 1 0 0 1 102 700 Tm ( Sp. z o.o.) Tj ET\n";
        for replacement in ["Nowak", "Kowalskiewicz"] {
            let bytes = synthetic_pdf(content, "Helvetica");
            let mut doc = Document::load_mem(&bytes)?;
            let before = fragments(&doc);
            let original_gap = gap_after(&before, "Kowalski", "Sp.").unwrap_or(0.0);
            assert!(
                (original_gap - 4.0).abs() <= 1.0,
                "setup gap {original_gap}"
            );

            let outcome = replace_in_document(
                &mut doc,
                "Kowalski",
                replacement,
                true,
                &SubstituteFonts::default(),
            );
            assert!(
                matches!(outcome, Ok(ref o) if o.replaced == 1),
                "{outcome:?}"
            );
            assert_streams_valid(&doc);

            let after = fragments(&doc);
            let gap = gap_after(&after, replacement, "Sp.").unwrap_or(-999.0);
            assert!(
                (gap - original_gap).abs() <= 1.0,
                "{replacement}: gap {gap} vs original {original_gap}; fragments {after:?}"
            );
            assert!(gap >= -0.5, "{replacement}: fragments overlap, gap {gap}");
        }
        Ok(())
    }

    #[test]
    fn td_chain_shifts_following_glyphs() -> Result<(), Box<dyn std::error::Error>> {
        // Skia style: every fragment is positioned by a `Td` relative to the
        // line matrix. "AB" at 50, Td 12 -> "CD" at 62, Td 12 -> "EF" at 74.
        let content = "BT /F1 12 Tf 1 0 0 1 50 700 Tm (AB) Tj 12 0 Td (CD) Tj 12 0 Td (EF) Tj ET\n";
        let bytes = synthetic_pdf(content, "Helvetica");
        let mut doc = Document::load_mem(&bytes)?;
        let outcome =
            replace_in_document(&mut doc, "AB", "ABCD", true, &SubstituteFonts::default());
        assert!(
            matches!(outcome, Ok(ref o) if o.replaced == 1),
            "{outcome:?}"
        );
        assert_streams_valid(&doc);
        let after = fragments(&doc);
        // New text is 2 chars * 6 pt wider, so "CD" moves 62 -> 74, "EF" 74 -> 86.
        let cd_x = after
            .iter()
            .find(|frag| frag.0 == "CD")
            .map(|frag| frag.1)
            .unwrap_or(0.0);
        let ef_x = after
            .iter()
            .find(|frag| frag.0 == "EF")
            .map(|frag| frag.1)
            .unwrap_or(0.0);
        assert!(
            (cd_x - 74.0).abs() <= 1.0,
            "CD at {cd_x}, fragments {after:?}"
        );
        assert!(
            (ef_x - 86.0).abs() <= 1.0,
            "EF at {ef_x}, fragments {after:?}"
        );
        Ok(())
    }

    #[test]
    fn substitute_replacement_keeps_following_text_in_place()
    -> Result<(), Box<dyn std::error::Error>> {
        let font = crate::pdf::load_font(None)?;
        let substitutes = SubstituteFonts::all(&font);

        // Two operators: the following one relies on natural advance.
        let two_ops = "BT /F1 12 Tf 1 0 0 1 50 700 Tm (Kowalski) Tj ( Sp.) Tj ET\n";
        let bytes = synthetic_pdf(two_ops, "Helvetica");
        let mut doc = Document::load_mem(&bytes)?;
        let outcome = replace_in_document(&mut doc, "Kowalski", "Żaneta", true, &substitutes);
        assert!(
            matches!(outcome, Ok(ref o) if o.replaced == 1),
            "{outcome:?}"
        );
        assert_streams_valid(&doc);
        let after = fragments(&doc);
        let gap = gap_after(&after, "Żaneta", "Sp.").unwrap_or(-999.0);
        assert!(
            (-0.5..=1.5).contains(&gap),
            "two-op gap {gap}; fragments {after:?}"
        );

        // One operator: the suffix shares the string with the replaced text.
        let one_op = "BT /F1 12 Tf 1 0 0 1 50 700 Tm (Kowalski Sp.) Tj ET\n";
        let bytes = synthetic_pdf(one_op, "Helvetica");
        let mut doc = Document::load_mem(&bytes)?;
        let outcome = replace_in_document(&mut doc, "Kowalski", "Żaneta", true, &substitutes);
        assert!(
            matches!(outcome, Ok(ref o) if o.replaced == 1),
            "{outcome:?}"
        );
        assert_streams_valid(&doc);
        let after = fragments(&doc);
        let gap = gap_after(&after, "Żaneta", "Sp.").unwrap_or(-999.0);
        assert!(
            (-0.5..=1.5).contains(&gap),
            "one-op gap {gap}; fragments {after:?}"
        );
        Ok(())
    }

    #[test]
    fn substitute_font_is_a_small_bold_subset() -> Result<(), Box<dyn std::error::Error>> {
        let content = "BT /F1 12 Tf 1 0 0 1 50 700 Tm (Kowalski) Tj ET\n";
        let bytes = synthetic_pdf(content, "Helvetica-Bold");
        let mut doc = Document::load_mem(&bytes)?;
        let mut before = Vec::new();
        let _ = doc.save_to(&mut before);

        let substitutes = crate::pdf::load_substitute_fonts(None);
        assert!(!substitutes.regular.is_empty(), "missing substitute font");
        let outcome = replace_in_document(&mut doc, "Kowalski", "Żaneta", true, &substitutes);
        let outcome = outcome?;
        assert_eq!(outcome.replaced, 1);
        assert!(
            outcome
                .warnings
                .iter()
                .any(|warning| warning.code == MessageCode::PdfSubstituteFont)
        );
        assert_streams_valid(&doc);

        let mut after = Vec::new();
        let _ = doc.save_to(&mut after);
        let growth = after.len() as i64 - before.len() as i64;
        assert!(growth < 60_000, "file grew by {growth} bytes");

        // The bold original must get the bold substitute face.
        let bold_named = doc.objects.values().any(|object| {
            object
                .as_dict()
                .ok()
                .and_then(|dict| dict.get(b"FontName").ok())
                .and_then(|value| value.as_name().ok())
                .is_some_and(|name| String::from_utf8_lossy(name).contains("Bold"))
        });
        assert!(bold_named, "no bold substitute font descriptor");
        let text = document_text(&doc);
        assert!(text.contains("Żaneta"), "{text:?}");
        Ok(())
    }
}
