//! Carry link identity through layout using removable zero-advance anchors.
//! Text matching is deliberately avoided: repeated labels can have different URLs.

use crate::Error;
use printpdf::{
    Actions, BorderArray, ColorArray, CurTransMat, DictItem, LinkAnnotation, Op, PdfDocument,
    PdfFontHandle, Pt, Rect, TextItem, TextMatrix,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct Links {
    prefix: String,
    urls: Vec<String>,
    images: BTreeMap<printpdf::XObjectId, usize>,
}

pub(crate) fn prepare(html: &str) -> Result<(String, Links), Error> {
    let decoded = crate::read::html::markdown(html)?;
    let mut prefix = "\u{200b}\u{200b}".to_string();
    while decoded.contains(&prefix) {
        prefix.push('\u{200b}');
    }
    let mut links = Links {
        prefix,
        urls: Vec::new(),
        images: BTreeMap::new(),
    };
    let mut edits = Vec::new();
    let mut opened = None;
    let tags = crate::htmlpdf::markers::tags(html);
    for (position, tag) in tags.iter().enumerate() {
        if tag.name == "img"
            && !tag.closing
            && let Some((index, _)) = opened
            && let Some(Some(src)) = tag.attr("src")
        {
            links
                .images
                .insert(printpdf::html::bridge::image_xobject_id(src), index);
        }
        if tag.name != "a" {
            continue;
        }
        if tag.closing {
            if let Some((_, image_only)) = opened.take() {
                let range = if image_only {
                    tag.range.clone()
                } else {
                    tag.range.start..tag.range.start
                };
                edits.push((range, links.prefix.clone()));
            }
        } else if let Some(Some(_)) = tag.attr("href") {
            let raw = format!("{}label</a>", &html[tag.range.clone()]);
            let md = crate::read::html::markdown(&raw)?;
            let url = pulldown_cmark::Parser::new(&md).find_map(|event| match event {
                pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { dest_url, .. }) => {
                    Some(dest_url.to_string())
                }
                _ => None,
            });
            if let Some(url) = url {
                links.urls.push(url);
                // The layout backend treats an image inside an anchor as text
                // and drops it. Unwrap image-only anchors before layout; retain
                // their target by the deterministic image-resource identity.
                let mut end = tag.range.end;
                let mut image_only = true;
                let mut has_image = false;
                for child in &tags[position + 1..] {
                    image_only &= html[end..child.range.start].trim().is_empty();
                    if child.name == "a" && child.closing {
                        break;
                    }
                    has_image |= child.name == "img";
                    end = child.range.end;
                }
                image_only &= has_image;
                let range = if image_only {
                    tag.range.clone()
                } else {
                    tag.range.end..tag.range.end
                };
                edits.push((range, links.prefix.clone()));
                opened = Some((links.urls.len() - 1, image_only));
            }
        }
    }
    let mut out = html.to_string();
    for (range, token) in edits.into_iter().rev() {
        out.replace_range(range, &token);
    }
    Ok((out, links))
}
impl Links {
    pub fn annotate(&self, document: &mut PdfDocument) -> Result<(), Error> {
        if self.urls.is_empty() {
            return Ok(());
        }
        // First recover the anchor stream across runs, styles, lines and pages.
        let mut text = String::new();
        let mut glyphs = Vec::new();
        for (p, page) in document.pages.iter().enumerate() {
            for (o, op) in page.ops.iter().enumerate() {
                if let Op::ShowText { items } = op {
                    for (i, item) in items.iter().enumerate() {
                        if let TextItem::GlyphIds(gs) = item {
                            for (g, glyph) in gs.iter().enumerate() {
                                let start = text.len();
                                text.push_str(glyph.cid.as_deref().unwrap_or_default());
                                glyphs.push(((p, o, i, g), start..text.len()));
                            }
                        }
                    }
                }
            }
        }
        let mut tokens = Vec::new();
        let mut at = 0;
        while let Some(relative) = text[at..].find(&self.prefix) {
            let start = at + relative;
            let count = text[start..]
                .chars()
                .take_while(|c| *c == '\u{200b}')
                .count();
            at = start + count * '\u{200b}'.len_utf8();
            let base = self.prefix.chars().count();
            for n in 0..count / base {
                let id = if tokens.len() % 2 == 0 {
                    tokens.len() / 2 + 1
                } else {
                    0
                };
                let from = start + n * self.prefix.len();
                tokens.push((from..from + self.prefix.len(), id));
            }
        }
        if tokens.len() != self.urls.len() * 2 {
            return Err(Error::invalid(
                crate::messages::MessageCode::PdfLinkLayout,
                crate::messages::Params::new(),
            ));
        }
        let mut remove = BTreeSet::new();
        let mut targets = BTreeMap::new();
        let mut active = 0;
        let mut token = 0;
        for (key, range) in glyphs {
            while token < tokens.len() && tokens[token].0.end <= range.start {
                active = tokens[token].1;
                token += 1;
            }
            if !range.is_empty()
                && token < tokens.len()
                && range.start >= tokens[token].0.start
                && range.end <= tokens[token].0.end
            {
                remove.insert(key);
            } else if active > 0 {
                targets.insert(key, active - 1);
            }
        }
        let mut annotated = BTreeSet::new();
        for (p, page) in document.pages.iter_mut().enumerate() {
            let mut font = None;
            let mut size = 12.0;
            let mut x = 0.0;
            let mut y = 0.0;
            let mut rects: Vec<(usize, Rect)> = Vec::new();
            let mut ctm = CurTransMat::Identity.as_array();
            let mut stack = Vec::new();
            for (o, op) in page.ops.iter_mut().enumerate() {
                match op {
                    Op::SaveGraphicsState => stack.push(ctm),
                    Op::RestoreGraphicsState => {
                        if let Some(saved) = stack.pop() {
                            ctm = saved;
                        }
                    }
                    Op::SetTransformationMatrix { matrix } => {
                        ctm = CurTransMat::combine_matrix(matrix.as_array(), ctm)
                    }
                    Op::UseXobject { id, transform } => {
                        if let Some(&url) = self.images.get(id) {
                            let wh = if transform.no_auto_scale {
                                None
                            } else {
                                document
                                    .resources
                                    .xobjects
                                    .map
                                    .get(id)
                                    .and_then(|v| v.get_width_height())
                            };
                            let mut placement = CurTransMat::Identity.as_array();
                            for matrix in transform.get_ctms(wh) {
                                placement =
                                    CurTransMat::combine_matrix(placement, matrix.as_array());
                            }
                            placement = CurTransMat::combine_matrix(placement, ctm);
                            let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                            for (x, y) in [(0.0, 0.0), (0.0, 1.0), (1.0, 0.0), (1.0, 1.0)] {
                                let px = placement[0] * x + placement[2] * y + placement[4];
                                let py = placement[1] * x + placement[3] * y + placement[5];
                                bounds = [
                                    bounds[0].min(px),
                                    bounds[1].min(py),
                                    bounds[2].max(px),
                                    bounds[3].max(py),
                                ];
                            }
                            rects.push((
                                url,
                                Rect {
                                    x: Pt(bounds[0]),
                                    y: Pt(bounds[1]),
                                    width: Pt(bounds[2] - bounds[0]),
                                    height: Pt(bounds[3] - bounds[1]),
                                    mode: None,
                                    winding_order: None,
                                },
                            ));
                            annotated.insert(url);
                        }
                    }
                    Op::SetFont {
                        font: handle,
                        size: pt,
                    } => {
                        font = Some(handle.clone());
                        size = pt.0;
                    }
                    Op::SetTextMatrix {
                        matrix: TextMatrix::Raw(m),
                    } => {
                        x = m[4];
                        y = m[5];
                    }
                    Op::BeginMarkedContentWithProperties {
                        properties: DictItem::Dict { map },
                        ..
                    } => {
                        if let Some(DictItem::String { data, .. }) = map.get_mut("ActualText") {
                            let units: Vec<_> = data
                                .as_chunks::<2>()
                                .0
                                .iter()
                                .map(|b| u16::from_be_bytes(*b))
                                .collect();
                            if let Ok(mut value) = String::from_utf16(&units) {
                                value = value.replace(&self.prefix, "");
                                *data = value.encode_utf16().flat_map(u16::to_be_bytes).collect();
                            }
                        }
                    }
                    Op::ShowText { items } => {
                        for (i, item) in items.iter_mut().enumerate() {
                            match item {
                                TextItem::Offset(offset) => x -= *offset * size / 1000.0,
                                TextItem::GlyphIds(gs) => {
                                    let mut keep = Vec::new();
                                    for (g, glyph) in std::mem::take(gs).into_iter().enumerate() {
                                        let width = match &font {
                                            Some(PdfFontHandle::External(id)) => document
                                                .resources
                                                .fonts
                                                .map
                                                .get(id)
                                                .map(|f| {
                                                    f.parsed_font.get_horizontal_advance(glyph.gid)
                                                        as f32
                                                        / f.parsed_font.font_metrics.units_per_em
                                                            as f32
                                                        * size
                                                })
                                                .unwrap_or(size * 0.5),
                                            _ => size * 0.5,
                                        };
                                        if let Some(&url) = targets.get(&(p, o, i, g)) {
                                            annotated.insert(url);
                                            let rect = Rect {
                                                x: Pt(x),
                                                y: Pt(y - size * 0.25),
                                                width: Pt(width.max(0.0)),
                                                height: Pt(size),
                                                mode: None,
                                                winding_order: None,
                                            };
                                            if let Some((previous, last)) =
                                                rects.last_mut().filter(|(id, last)| {
                                                    *id == url
                                                        && (last.y.0 - rect.y.0).abs() < 0.1
                                                        && (last.x.0 + last.width.0 - x).abs() < 1.0
                                                })
                                            {
                                                let _ = previous;
                                                last.width.0 =
                                                    (x + width - last.x.0).max(last.width.0);
                                            } else {
                                                rects.push((url, rect));
                                            }
                                        }
                                        x += width - glyph.offset * size / 1000.0;
                                        if !remove.contains(&(p, o, i, g)) {
                                            keep.push(glyph);
                                        }
                                    }
                                    *gs = keep;
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            page.ops
                .extend(
                    rects
                        .into_iter()
                        .filter(|(_, r)| r.width.0 > 0.0)
                        .map(|(id, rect)| Op::LinkAnnotation {
                            link: LinkAnnotation::new(
                                rect,
                                Actions::uri(self.urls[id].clone()),
                                Some(BorderArray::Solid([0.0; 3])),
                                Some(ColorArray::Transparent),
                                None,
                            ),
                        }),
                );
        }
        if annotated.len() != self.urls.len() {
            return Err(Error::invalid(
                crate::messages::MessageCode::PdfLinkLayout,
                crate::messages::Params::new(),
            ));
        }
        Ok(())
    }
}
