//! PDF links, paragraph decoration and high-resolution vector illustrations.
use super::*;
use docx_rs::*;

pub(super) fn resolve_fonts(
    frags: &mut [Frag],
    page: usize,
    warnings: &mut Vec<Msg>,
) -> Result<(), Error> {
    fn normalized(name: &str) -> String {
        name.split_once('+')
            .map_or(name, |(_, name)| name)
            .split('-')
            .next()
            .unwrap_or(name)
            .replace(' ', "")
            .to_ascii_lowercase()
    }
    for frag in frags {
        if crate::pdf::family_metrics(&frag.font)?.is_none() {
            frag.font = "Noto Sans".into();
        }
        if normalized(&frag.source_font) != normalized(&frag.font) {
            warnings.push(Msg::with(
                MessageCode::PdfDocxFontSubstituted,
                params(&[
                    ("page", page.into()),
                    ("font", frag.source_font.clone().into()),
                    ("substitute", frag.font.clone().into()),
                ]),
            ));
        }
    }
    Ok(())
}

fn contains(r: [f64; 4], line: &PositionedLine) -> bool {
    line.x >= r[0] - 0.5 && line.x + line.width <= r[2] + 0.5 && line.y >= r[1] && line.y <= r[3]
}

pub(super) fn link_runs(
    pdf: &PdfDocument,
    page_id: ObjectId,
    segments: &[Segment],
    frags: &mut [Frag],
    page: usize,
    warnings: &mut Vec<Msg>,
) -> Result<(), Error> {
    for annotation in pdf.get_page_annotations(page_id)? {
        let uri = annotation
            .get(b"A")
            .ok()
            .and_then(|v| pdf.dereference(v).ok())
            .and_then(|(_, v)| v.as_dict().ok())
            .and_then(|a| a.get(b"URI").ok())
            .and_then(|v| lopdf::decode_text_string(v).ok());
        let rect = annotation
            .get(b"Rect")
            .ok()
            .and_then(|v| pdf.dereference(v).ok())
            .and_then(|(_, v)| v.as_array().ok())
            .filter(|a| a.len() == 4)
            .and_then(|a| {
                a.iter()
                    .map(|v| v.as_float().ok().map(f64::from))
                    .collect::<Option<Vec<_>>>()
            });
        let (Some(uri), Some(r)) = (uri, rect) else {
            warnings.push(Msg::with(
                MessageCode::PdfDocxAnnotationUnsupported,
                params(&[("page", page.into())]),
            ));
            continue;
        };
        for frag in frags
            .iter_mut()
            .filter(|f| f.x < r[2] && f.x_end > r[0] && f.y >= r[1] - 1.0 && f.y <= r[3] + 1.0)
        {
            if frag.x < r[0] - 1.0 || frag.x_end > r[2] + 1.0 {
                warnings.push(Msg::with(
                    MessageCode::PdfDocxLinkExtent,
                    params(&[("page", page.into())]),
                ));
            }
            frag.href = Some(uri.clone());
            // A PDF link does not imply a blue underline. Preserve the painted
            // text colour and only recreate an underline actually present.
            frag.underline = segments.iter().any(|s| {
                (s.y1 - s.y0).abs() < 1.0
                    && s.y0 < frag.y
                    && s.y0 > frag.y - frag.size * 0.35
                    && s.x0.min(s.x1) <= frag.x + 1.0
                    && s.x0.max(s.x1) >= frag.x_end - 1.0
            });
        }
    }
    Ok(())
}

pub(super) fn paragraph_backgrounds(
    blocks: &mut [LayoutBlock],
    geometry: &pdfedit::PageBounds,
) {
    for block in blocks {
        let DocumentChild::Paragraph(p) = &mut block.child else {
            continue;
        };
        let Some(line) = block.source.first() else {
            continue;
        };
        if let Some((rect, color)) = geometry
            .fills
            .iter()
            .rev()
            .find(|(r, _)| {
                let [x0, y0, x1, y1] = r.0;
                line.x >= x0 - 0.5 && line.x_end <= x1 + 0.5 && line.y >= y0 && line.y <= y1
            })
        {
            p.property.shading = Some(Shading::new().fill(color));
            if let Some(frame) = p.property.frame_property.as_mut() {
                frame.w = Some(((rect.0[2] - line.x) * 20.0).round() as u32);
            }
        }
    }
}

/// List markers are printed text in PDF. Convert those prefixes to real Word
/// numbering only after coordinate matching, so removing a marker cannot move
/// the paragraph or cause its next occurrence to consume the wrong source row.
pub(super) fn number_lists(docx: &mut Docx, document: &mut Document) {
    let mut id = 1000;
    let mut base_x: Option<i32> = None;
    for child in &mut document.children {
        let DocumentChild::Paragraph(p) = child else {
            continue;
        };
        let text = paragraph_text(p);
        let Some((ordered, number, prefix)) = list_marker(&text) else {
            continue;
        };
        let x = p
            .property
            .frame_property
            .as_ref()
            .and_then(|f| f.x)
            .unwrap_or(0);
        let left = *base_x.get_or_insert(x);
        let level = ((x - left).max(0) / 360).min(8) as usize;
        let mut remaining = prefix;
        for child in &mut p.children {
            if let ParagraphChild::Run(run) = child {
                for child in &mut run.children {
                    if let RunChild::Text(t) = child {
                        let cut = remaining.min(t.text.len());
                        if t.text.is_char_boundary(cut) {
                            t.text.drain(..cut);
                            remaining -= cut;
                        }
                    }
                }
            }
        }
        let marker_style = p
            .children
            .iter()
            .find_map(|c| {
                if let ParagraphChild::Run(r) = c {
                    Some(r.run_property.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let format = if ordered { "decimal" } else { "bullet" };
        let marker = if ordered {
            format!("%{}.", level + 1)
        } else {
            "•".into()
        };
        // Each explicit PDF item owns a start value; this also preserves lists
        // that restart, without colliding with the builder's reserved ID 1.
        let offset = p
            .property
            .indent
            .as_ref()
            .and_then(|i| i.start)
            .unwrap_or(if ordered { 400 } else { 180 });
        let mut definition = Level::new(
            level,
            Start::new(number),
            NumberFormat::new(format),
            LevelText::new(marker),
            LevelJc::new("left"),
        )
        .indent(
            Some(offset),
            Some(SpecialIndentType::Hanging(offset)),
            None,
            None,
        );
        definition.run_property = marker_style;
        docx.numberings = docx
            .numberings
            .clone()
            .add_abstract_numbering(AbstractNumbering::new(id).add_level(definition))
            .add_numbering(Numbering::new(id, id));
        p.property = p
            .property
            .clone()
            .numbering(NumberingId::new(id), IndentLevel::new(level));
        p.has_numbering = true;
        docx.document_rels.has_numberings = true;
        id += 1;
    }
}

fn intersects(a: [f64; 4], b: [f64; 4], gap: f64) -> bool {
    a[0] <= b[2] + gap && b[0] <= a[2] + gap && a[1] <= b[3] + gap && b[1] <= a[3] + gap
}

pub(super) fn vector_regions(
    geometry: &pdfedit::PageBounds,
    table: Option<&DetectedTable>,
    lines: &[PositionedLine],
) -> Vec<[f64; 4]> {
    let mut regions: Vec<[f64; 4]> = Vec::new();
    for path in &geometry.paths {
        let r = path.0;
        if table.is_some_and(|t| {
            r[0] >= t.min_x - 2.0
                && r[2] <= t.max_x + 2.0
                && r[1] >= t.min_y - 2.0
                && r[3] <= t.max_y + 2.0
        }) {
            continue;
        }
        // Filled text backgrounds are reproduced as paragraph shading, not
        // chart images. Underlines are run properties, not illustrations.
        if geometry.fills.iter().any(|(f, color)| {
            f.0 == r
                && (color == "FFFFFF"
                    || lines
                        .iter()
                        .any(|l| contains(r, l) && font_family(&l.font) == "Noto Sans Mono"))
        }) {
            continue;
        }
        if r[3] - r[1] <= 1.5
            && lines.iter().any(|l| {
                r[0] >= l.x - 1.0
                    && r[2] <= l.x + l.width + 1.0
                    && r[1] < l.y
                    && r[3] > l.y - l.size * 0.35
            })
        {
            continue;
        }
        let mut combined = r;
        while let Some(i) = regions
            .iter()
            .position(|other| intersects(combined, *other, 2.0))
        {
            let other = regions.remove(i);
            combined = [
                combined[0].min(other[0]),
                combined[1].min(other[1]),
                combined[2].max(other[2]),
                combined[3].max(other[3]),
            ];
        }
        regions.push(combined);
    }
    regions.sort_by(|a, b| b[3].total_cmp(&a[3]));
    regions
}

pub(super) fn insert_vectors(
    pdf: &PdfDocument,
    page_info: (ObjectId, usize),
    blocks: &mut Vec<LayoutBlock>,
    geometry: &pdfedit::PageBounds,
    table: Option<&DetectedTable>,
    lines: &[PositionedLine],
    warnings: &mut Vec<Msg>,
) -> Result<(), Error> {
    let (page_id, page) = page_info;
    let regions = vector_regions(geometry, table, lines);
    if regions.is_empty() {
        return Ok(());
    }
    let [x0, y0, x1, y1] = geometry.media;
    let feature = ["Rotate", "UserUnit", "CropBox"]
        .into_iter()
        .find(|key| geometry.operators.contains(*key));
    let number = pdf.get_pages().into_values().position(|id| id == page_id);
    if let Some(feature) = feature.or(if number.is_none() {
        Some("detached_page")
    } else {
        None
    }) {
        warnings.push(Msg::with(
            MessageCode::PdfDocxVectorGeometry,
            params(&[("page", page.into()), ("feature", feature.into())]),
        ));
        return Ok(());
    }
    // A fixed 216 DPI (3 px/pt), with a hard raster budget before spawning.
    if (x1 - x0) * (y1 - y0) * 9.0 > 40_000_000.0 {
        return Err(Error::invalid(
            MessageCode::PdfDocxRasterLimit,
            params(&[("page", page.into())]),
        ));
    }
    let scratch = crate::read::raster::Scratch::new()?;
    let mut bytes = Vec::new();
    pdf.clone()
        .save_to(&mut bytes)
        .map_err(|e| Error::reader(MessageCode::PdfDocxRasterFailed, e))?;
    scratch.write("in.pdf", &bytes)?;
    let number = number
        .map(|i| i + 1)
        .ok_or_else(|| {
            Error::invalid(
                MessageCode::PdfDocxRasterFailed,
                params(&[("page", page.into())]),
            )
        })?
        .to_string();
    crate::read::raster::decode(
        &scratch,
        "pdftoppm",
        &[
            "-f",
            &number,
            "-l",
            &number,
            "-singlefile",
            "-r",
            "216",
            "-png",
            "/work/in.pdf",
            "/work/page",
        ],
    )
    .map_err(|e| Error::reader(MessageCode::PdfDocxRasterFailed, e))?;
    let image = image::load_from_memory(&scratch.read("page.png", 160 * 1024 * 1024)?)
        .map_err(|e| Error::reader(MessageCode::PdfDocxRasterFailed, e))?;
    for r in regions {
        let left = ((r[0] - x0) * 3.0).floor().max(0.0) as u32;
        let top = ((y1 - r[3]) * 3.0).floor().max(0.0) as u32;
        let right = ((r[2] - x0) * 3.0).ceil().min(f64::from(image.width())) as u32;
        let bottom = ((y1 - r[1]) * 3.0).ceil().min(f64::from(image.height())) as u32;
        if right <= left || bottom <= top {
            continue;
        }
        let crop = image.crop_imm(left, top, right - left, bottom - top);
        let mut png = Cursor::new(Vec::new());
        crop.write_to(&mut png, image::ImageFormat::Png)
            .map_err(|e| Error::reader(MessageCode::PdfDocxRasterFailed, e))?;
        let pic = Pic::new_with_dimensions(png.into_inner(), right - left, bottom - top)
            .size(
                ((r[2] - r[0]) * 12700.0).round() as u32,
                ((r[3] - r[1]) * 12700.0).round() as u32,
            )
            .floating()
            .overlapping()
            .relative_from_h(RelativeFromHType::Page)
            .relative_from_v(RelativeFromVType::Page)
            .offset_x(((r[0] - x0) * 12700.0).round() as i32)
            .offset_y(((y1 - r[3]) * 12700.0).round() as i32);
        let picture = Paragraph::new().add_run(Run::new().add_image(pic));
        // Do not draw editable text a second time over the same text already
        // rendered by Poppler inside the illustration crop.
        blocks.retain(|block| {
            let sources: Vec<_> = block.source.iter()
                .filter(|source| !source.text.trim().is_empty()).collect();
            sources.is_empty() || !sources.iter().all(|source| {
                source.x >= r[0] - 0.5 && source.x_end <= r[2] + 0.5
                    && source.y >= r[1] && source.y <= r[3]
            })
        });
        let at = blocks
            .iter()
            .position(|b| match &b.child {
                DocumentChild::Table(_) => table.is_some_and(|t| t.max_y < r[3]),
                DocumentChild::Paragraph(p) => p
                    .property
                    .frame_property
                    .as_ref()
                    .and_then(|f| f.y)
                    .is_some_and(|y| f64::from(y) / 20.0 > y1 - r[3]),
                _ => false,
            })
            .unwrap_or(blocks.len());
        if lines.iter().any(|l| contains(r, l)) {
            warnings.push(Msg::with(
                MessageCode::PdfDocxVectorText,
                params(&[("page", page.into())]),
            ));
        }
        blocks.insert(at, DocumentChild::Paragraph(Box::new(picture)).into());
    }
    Ok(())
}
