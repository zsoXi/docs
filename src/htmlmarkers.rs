//! HTML-only list adaptation. Zero-advance layout anchors identify generated
//! markers, so literal bullet characters in document text are never repainted.
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

#[cfg(test)]
#[path = "htmlmarkers_tests.rs"]
mod tests;

use printpdf::{
    Color, DictItem, LinePoint, Op, PaintMode, PdfDocument, Point, Polygon, PolygonRing, Pt,
    TextItem, TextMatrix, TextRenderingMode, WindingOrder,
};

pub(crate) struct Tag<'a> {
    pub range: Range<usize>,
    pub name: String,
    pub closing: bool,
    attrs: Vec<(&'a str, Option<&'a str>, Range<usize>)>,
}

impl Tag<'_> {
    pub(crate) fn attribute_range(&self, name: &str) -> Option<Range<usize>> {
        self.attrs.iter().find(|(n, _, _)| n.eq_ignore_ascii_case(name)).map(|(_, _, range)| range.clone())
    }

    pub(crate) fn attr(&self, name: &str) -> Option<Option<&str>> {
        self.attrs
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v, _)| *v)
    }
}

// Only tag boundaries and attributes are needed; preserve all other bytes,
// including raw-text elements and comments. Quoted '>' is not a tag boundary.
pub(crate) fn tags(html: &str) -> Vec<Tag<'_>> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(relative) = html[at..].find('<') {
        let start = at + relative;
        if html[start..].starts_with("<!--") {
            at = html[start..]
                .find("-->")
                .map_or(html.len(), |n| start + n + 3);
            continue;
        }
        let mut quote = None;
        let Some(end) = html[start..].char_indices().find_map(|(n, ch)| {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
            } else if matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else if ch == '>' {
                return Some(start + n + 1);
            }
            None
        }) else {
            break;
        };
        at = end;
        let mut pos = start + 1;
        let closing = html.as_bytes().get(pos) == Some(&b'/');
        if closing {
            pos += 1;
        }
        let name_start = pos;
        while pos < end && (html.as_bytes()[pos].is_ascii_alphanumeric() || html.as_bytes()[pos] == b':') {
            pos += 1;
        }
        if pos == name_start {
            continue;
        }
        let name = lower[name_start..pos].to_string();
        let mut attrs = Vec::new();
        while pos < end - 1 {
            while pos < end - 1 && html.as_bytes()[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos >= end - 1 || html.as_bytes()[pos] == b'/' {
                break;
            }
            let attr_start = pos;
            while pos < end - 1
                && !html.as_bytes()[pos].is_ascii_whitespace()
                && !matches!(html.as_bytes()[pos], b'=' | b'>')
            {
                pos += 1;
            }
            let attr_name = &html[attr_start..pos];
            while pos < end - 1 && html.as_bytes()[pos].is_ascii_whitespace() {
                pos += 1;
            }
            let mut value = None;
            if html.as_bytes().get(pos) == Some(&b'=') {
                pos += 1;
                while pos < end - 1 && html.as_bytes()[pos].is_ascii_whitespace() {
                    pos += 1;
                }
                let q = html
                    .as_bytes()
                    .get(pos)
                    .copied()
                    .filter(|c| matches!(c, b'\'' | b'"'));
                if q.is_some() {
                    pos += 1;
                }
                let v = pos;
                while pos < end - 1
                    && q.map_or_else(
                        || !html.as_bytes()[pos].is_ascii_whitespace(),
                        |q| html.as_bytes()[pos] != q,
                    )
                {
                    pos += 1;
                }
                value = Some(&html[v..pos]);
                if q.is_some() && pos < end - 1 {
                    pos += 1;
                }
            }
            attrs.push((attr_name, value, attr_start..pos));
        }
        if !closing && matches!(name.as_str(), "script" | "style" | "textarea" | "title") {
            at = lower[end..]
                .find(&format!("</{name}"))
                .map_or(html.len(), |n| end + n);
        }
        out.push(Tag {
            range: start..end,
            name,
            closing,
            attrs,
        });
    }
    out
}

fn integer(value: Option<Option<&str>>) -> Option<i64> {
    value.flatten()?.trim().parse::<i32>().ok().map(i64::from)
}

pub(super) fn has_author_list_style(html: &str) -> bool {
    let declarations = |css: &str| {
        super::without_comments(css)
            .split(['{', '}', ';'])
            .any(|declaration| {
                declaration.split_once(':').is_some_and(|(key, _)| {
                    matches!(
                        key.trim().to_ascii_lowercase().as_str(),
                        "list-style" | "list-style-type" | "list-style-position"
                    )
                })
            })
    };
    let tags = tags(html);
    tags.iter().enumerate().any(|(index, tag)| {
        if let Some(Some(style)) = tag.attr("style")
            && declarations(style)
        {
            return true;
        }
        tag.name == "style"
            && !tag.closing
            && tags
                .get(index + 1)
                .filter(|end| end.closing && end.name == "style")
                .is_some_and(|end| declarations(&html[tag.range.end..end.range.start]))
    })
}

pub(super) fn prepare(html: &str, author_list_style: bool) -> (String, String) {
    // The anchor is zero-advance in the serif face, unlike U+2060. Avoid a
    // collision with literal or entity-encoded zero-width spaces in input.
    let mut decoded = String::new();
    let mut rest = html;
    while let Some(at) = rest.find('&') {
        decoded.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let numeric = entity.strip_prefix('#').and_then(|n| {
            if let Some(hex) = n.strip_prefix('x').or_else(|| n.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                n.parse::<u32>().ok()
            }
        });
        if numeric == Some(0x200b)
            || matches!(
                entity,
                "ZeroWidthSpace"
                    | "NegativeVeryThinSpace"
                    | "NegativeThinSpace"
                    | "NegativeMediumSpace"
                    | "NegativeThickSpace"
            )
        {
            decoded.push('\u{200b}');
        } else {
            decoded.push_str(&rest[..=end]);
        }
        rest = &rest[end + 1..];
    }
    decoded.push_str(rest);
    let longest = decoded
        .split(|c| c != '\u{200b}')
        .map(str::chars)
        .map(Iterator::count)
        .max()
        .unwrap_or(0);
    let anchor = "\u{200b}".repeat(longest.max(1) + 1);
    let tags = tags(html);
    let mut lists: Vec<(usize, Vec<usize>)> = Vec::new();
    let mut completed = Vec::new();
    for (index, tag) in tags.iter().enumerate() {
        if matches!(tag.name.as_str(), "ul" | "ol") {
            if tag.closing {
                if let Some(list) = lists.pop() {
                    completed.push(list);
                }
            } else {
                lists.push((index, Vec::new()));
            }
        } else if tag.name == "li"
            && !tag.closing
            && let Some((_, items)) = lists.last_mut()
        {
            items.push(index);
        }
    }
    completed.extend(lists);
    let mut replacements = BTreeMap::new();
    // Normalize mixed inline/list children into sibling layout blocks. The
    // backend otherwise emits the parent's inline text again before its nested
    // list. Keep the ul/ol nesting (and thus indentation and marker styles).
    let mut open_items = Vec::new();
    let mut split = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    for (index, tag) in tags.iter().enumerate() {
        if tag.name == "li" {
            if tag.closing {
                if let Some(item) = open_items.pop()
                    && split.contains(&item)
                {
                    replacements.insert(index, String::new());
                }
            } else {
                open_items.push(index);
            }
        } else if matches!(tag.name.as_str(), "ul" | "ol")
            && !tag.closing
            && let Some(&item) = open_items.last()
            && split.insert(item)
        {
            prefixes.insert(index);
        }
    }
    for (list_index, items) in completed {
        let list = &tags[list_index];
        let ordered = list.name == "ol";
        let reversed = ordered && list.attr("reversed").is_some();
        let mut next =
            integer(list.attr("start")).unwrap_or(if reversed { items.len() as i64 } else { 1 });
        // The layout parser is XML-like: normalize the HTML boolean attribute.
        let mut opening = html[list.range.clone()].to_string();
        let mut changes = Vec::new();
        if let Some((_, _, range)) = list
            .attrs
            .iter()
            .find(|(n, _, _)| n.eq_ignore_ascii_case("reversed"))
        {
            changes.push((
                range.start - list.range.start..range.end - list.range.start,
                "reversed=\"reversed\" ".to_string(),
            ));
        }
        if let Some(kind) = list.attr("type").flatten().and_then(|v| match v {
            "a" => Some("lower-alpha"),
            "A" => Some("upper-alpha"),
            "i" => Some("lower-roman"),
            "I" => Some("upper-roman"),
            _ => None,
        }) {
            // Type is only a presentation hint, never override author list CSS.
            if !author_list_style {
                if let Some((_, value, range)) = list
                    .attrs
                    .iter()
                    .find(|(n, _, _)| n.eq_ignore_ascii_case("style"))
                {
                    changes.push((
                        range.start - list.range.start..range.end - list.range.start,
                        format!(
                            "style=\"list-style-type:{kind};{}\"",
                            value.unwrap_or("").replace('"', "&quot;")
                        ),
                    ));
                } else {
                    changes.push((
                        opening.len() - 1..opening.len() - 1,
                        format!(" style=\"list-style-type:{kind}\""),
                    ));
                }
            }
        }
        changes.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, text) in changes {
            opening.replace_range(range, &text);
        }
        replacements.insert(list_index, opening);
        for index in items {
            let item = &tags[index];
            let mut opening = html[item.range.clone()].to_string();
            if ordered {
                next = integer(item.attr("value"))
                    .unwrap_or(next)
                    .clamp(-2_147_483_647, 2_147_483_647);
                let reset = format!("counter-reset:list-item {}", next - 1);
                if let Some((_, value, range)) = item
                    .attrs
                    .iter()
                    .find(|(n, _, _)| n.eq_ignore_ascii_case("style"))
                {
                    let style = format!(
                        "style=\"{};{reset}\"",
                        value.unwrap_or("").replace('"', "&quot;")
                    );
                    opening.replace_range(
                        range.start - item.range.start..range.end - item.range.start,
                        &style,
                    );
                } else {
                    opening.insert_str(opening.len() - 1, &format!(" style=\"{reset}\""));
                }
                next += if reversed { -1 } else { 1 };
            }
            opening.push_str(&anchor);
            replacements.insert(index, opening);
        }
    }
    let mut out = String::new();
    let mut at = 0;
    for (index, tag) in tags.iter().enumerate() {
        if replacements.contains_key(&index) || prefixes.contains(&index) {
            out.push_str(&html[at..tag.range.start]);
            if prefixes.contains(&index) {
                out.push_str("</li>");
            }
            out.push_str(
                replacements
                    .get(&index)
                    .map_or(&html[tag.range.clone()], String::as_str),
            );
            at = tag.range.end;
        }
    }
    out.push_str(&html[at..]);
    (out, anchor)
}

struct Glyph {
    op: usize,
    matrix: usize,
    text: String,
    x: f32,
    y: f32,
    size: f32,
    color: Option<Color>,
}

pub(super) fn paint(document: &mut PdfDocument, anchor: &str) {
    for page in &mut document.pages {
        let mut glyphs = Vec::new();
        let mut size = 12.0;
        let mut color = None;
        let mut matrix = (0, 0.0, 0.0);
        for (i, op) in page.ops.iter_mut().enumerate() {
            match op {
                Op::SetFont { size: pt, .. } => size = pt.0,
                Op::SetFillColor { col } => color = Some(col.clone()),
                Op::SetTextMatrix {
                    matrix: TextMatrix::Raw(m),
                } => matrix = (i, m[4], m[5]),
                Op::BeginMarkedContentWithProperties {
                    properties: DictItem::Dict { map: properties },
                    ..
                } => {
                    if let Some(DictItem::String { data, .. }) = properties.get_mut("ActualText") {
                        let units: Vec<_> = data
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|c| u16::from_be_bytes(*c))
                            .collect();
                        if let Ok(text) = String::from_utf16(&units) {
                            *data = text
                                .replace(anchor, "")
                                .encode_utf16()
                                .flat_map(u16::to_be_bytes)
                                .collect();
                        }
                    }
                }
                Op::ShowText { items } => {
                    let text: String = items
                        .iter()
                        .filter_map(|item| {
                            if let TextItem::GlyphIds(gs) = item {
                                Some(
                                    gs.iter()
                                        .filter_map(|g| g.cid.as_deref())
                                        .collect::<String>(),
                                )
                            } else {
                                None
                            }
                        })
                        .collect();
                    glyphs.push(Glyph {
                        op: i,
                        matrix: matrix.0,
                        text,
                        x: matrix.1,
                        y: matrix.2,
                        size,
                        color: color.clone(),
                    });
                }
                _ => {}
            }
        }
        let mut remove = BTreeSet::new();
        let mut invisible = BTreeSet::new();
        let mut drawings: BTreeMap<usize, Vec<Op>> = BTreeMap::new();
        let mut i = 0;
        while i < glyphs.len() {
            let first = i;
            let mut text = String::new();
            while i < glyphs.len()
                && !glyphs[i].text.is_empty()
                && glyphs[i].text.chars().all(|c| c == '\u{200b}')
            {
                text.push_str(&glyphs[i].text);
                i += 1;
            }
            if !text.contains(anchor) {
                i = first + 1;
                continue;
            }
            let mut remaining = anchor.chars().count();
            for g in &glyphs[first..i] {
                let count = g.text.chars().count();
                if remaining >= count {
                    remove.insert(g.op);
                    remaining -= count;
                } else if remaining > 0 {
                    if let Op::ShowText { items } = &mut page.ops[g.op] {
                        for item in items {
                            if let TextItem::GlyphIds(gs) = item {
                                for glyph in gs {
                                    if let Some(cid) = &mut glyph.cid {
                                        *cid = cid.chars().skip(remaining).collect();
                                    }
                                }
                            }
                        }
                    }
                    remaining = 0;
                }
            }
            let origin = &glyphs[first];
            let mut start = first;
            while start > 0 {
                let g = &glyphs[start - 1];
                if (g.y - origin.y).abs() > 0.01
                    || g.x >= origin.x
                    || !g.text.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ' ' | '•' | '◦' | '▪')
                    })
                {
                    break;
                }
                start -= 1;
            }
            let marker: String = glyphs[start..first]
                .iter()
                .map(|g| g.text.as_str())
                .collect();
            let symbol = marker.trim();
            if matches!(symbol, "•" | "◦" | "▪") {
                for g in &glyphs[start..first] {
                    invisible.insert(g.op);
                }
                if let Some(end) = page.ops[origin.op..]
                    .iter()
                    .position(|o| matches!(o, Op::EndTextSection))
                {
                    drawings
                        .entry(origin.op + end)
                        .or_default()
                        .extend(symbol_ops(symbol, origin));
                }
            } else if marker.ends_with(". ") {
                // The backend adds an extra quarter-em outside the marker's
                // trailing space. Chromium uses just that trailing space.
                for g in &glyphs[start..first] {
                    if let Op::SetTextMatrix {
                        matrix: TextMatrix::Raw(m),
                    } = &mut page.ops[g.matrix]
                    {
                        m[4] += origin.size * 0.25;
                    }
                }
            }
        }
        let mut ops = Vec::new();
        for (i, op) in std::mem::take(&mut page.ops).into_iter().enumerate() {
            if !remove.contains(&i) {
                if invisible.contains(&i) {
                    ops.push(Op::SetTextRenderingMode {
                        mode: TextRenderingMode::Invisible,
                    });
                }
                ops.push(op);
                if invisible.contains(&i) {
                    ops.push(Op::SetTextRenderingMode {
                        mode: TextRenderingMode::Fill,
                    });
                }
            }
            if let Some(draw) = drawings.remove(&i) {
                ops.extend(draw);
            }
        }
        page.ops = ops;
    }
}

fn symbol_ops(symbol: &str, origin: &Glyph) -> Vec<Op> {
    // Chromium uses an integral CSS-pixel symbol box and a 6px hanging gap
    // beyond twice the symbol width. For 16px serif: 5px disc, 1px circle stroke,
    // top 7px above the baseline (not the bullet glyph's font bounding box).
    let diameter = (origin.size / 0.75 / 3.0).floor().max(1.0) * 0.75;
    let x = origin.x - 2.0 * diameter - 4.5;
    let top = origin.y + origin.size * 7.0 / 16.0;
    let r = diameter / 2.0;
    let (cx, cy) = (x + r, top - r);
    let point = |x, y, bezier| LinePoint {
        p: Point { x: Pt(x), y: Pt(y) },
        bezier,
    };
    let points = if symbol == "▪" {
        vec![
            point(x, top, false),
            point(x + diameter, top, false),
            point(x + diameter, top - diameter, false),
            point(x, top - diameter, false),
        ]
    } else {
        let k = r * 0.552_284_8;
        vec![
            point(cx + r, cy, false),
            point(cx + r, cy + k, true),
            point(cx + k, cy + r, true),
            point(cx, cy + r, false),
            point(cx - k, cy + r, true),
            point(cx - r, cy + k, true),
            point(cx - r, cy, false),
            point(cx - r, cy - k, true),
            point(cx - k, cy - r, true),
            point(cx, cy - r, false),
            point(cx + k, cy - r, true),
            point(cx + r, cy - k, true),
            point(cx + r, cy, false),
        ]
    };
    let mut ops = vec![Op::SaveGraphicsState];
    if symbol == "◦" {
        if let Some(col) = &origin.color {
            ops.push(Op::SetOutlineColor { col: col.clone() });
        }
        ops.push(Op::SetOutlineThickness { pt: Pt(0.75) });
    }
    ops.push(Op::DrawPolygon {
        polygon: Polygon {
            rings: vec![PolygonRing { points }],
            mode: if symbol == "◦" {
                PaintMode::Stroke
            } else {
                PaintMode::Fill
            },
            winding_order: WindingOrder::NonZero,
        },
    });
    ops.push(Op::RestoreGraphicsState);
    ops
}
