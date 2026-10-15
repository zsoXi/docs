//! PDF → DOCX.
//!
//! Text is grouped into rows and paragraphs from its positions. Reading order
//! is built per page band: full-width lines (title, intro) come first in top
//! to bottom order, and between them multi-column blocks are read column by
//! column, with columns detected from a vertical gutter inside the band.
//! Grid lines delimit tables, which stay where they are on the page; image
//! XObjects are re-encoded as PNG and embedded in `word/media`.

use std::cmp::Ordering;
use std::io::Cursor;

use docx_rs::{Document, DocumentChild, Paragraph, Pic, Run, Table, TableCell, TableRow};
use lopdf::{Document as PdfDocument, ObjectId, Stream};

use crate::error::Error;
use crate::messages::{MessageCode, Msg, params};
use crate::pdfedit::{self, PositionedLine, Segment};

#[path = "pdfdocx_assets.rs"]
mod assets;

/// The DOCX bytes plus anything that did not survive the conversion.
pub(crate) struct BuildResult {
    pub data: Vec<u8>,
    pub warnings: Vec<Msg>,
}

/// Build a DOCX from every page of a PDF.
pub(crate) fn build(pdf: &PdfDocument, page_ids: &[ObjectId]) -> Result<BuildResult, Error> {
    build_with_ocr(pdf, page_ids, &std::collections::BTreeMap::new())
}

/// Native pages retain their layout; recognized scan pages become editable blocks.
pub(crate) fn build_with_ocr(pdf: &PdfDocument, page_ids: &[ObjectId], recognized: &std::collections::BTreeMap<usize, String>) -> Result<BuildResult, Error> {
    let mut children: Vec<DocumentChild> = Vec::new();
    let mut warnings = Vec::new();
    let mut page_geometry = None;
    let word_ascent = crate::pdf::family_metrics("Noto Sans")?
        .map(|v| v.0)
        .unwrap_or(1.0);

    let original_pages = pdf.get_pages();
    for (selection_index, page_id) in page_ids.iter().enumerate() {
        // Callers can also supply newly constructed pages not yet attached to
        // the tree. Rooted pages always retain their original document number.
        let page = original_pages.iter().find_map(|(number, id)| {
            (id == page_id).then_some(*number as usize)
        }).unwrap_or(selection_index + 1);
        let index = page - 1;
        if let Some(previous) = page_geometry.take() {
            // Each PDF page owns its geometry; the paragraph terminates the
            // preceding section, rather than adding a second page break.
            children.push(DocumentChild::Paragraph(Box::new(
                Paragraph::new().section_property(section_geometry(&previous)),
            )));
        }
        warnings.extend(pdfedit::form_warnings(pdf, *page_id)?);
        let geometry = pdfedit::page_bounds(pdf, *page_id)?;
        if geometry.approximate {
            for operator in &geometry.operators {
                warnings.push(Msg::with(
                    MessageCode::PdfDocxBoundsApproximate,
                    params(&[
                        ("page", (index + 1).into()),
                        ("operator", operator.clone().into()),
                    ]),
                ));
            }
        }
        page_geometry = Some(geometry);
        let lines = pdfedit::positioned_lines(pdf, *page_id)?;
        if let Some(text) = recognized.get(&page).filter(|s| !s.trim().is_empty()) {
            children.extend(ocr_children(text));
            continue;
        }
        if lines.is_empty() {
            warnings.push(Msg::with(
                MessageCode::PdfNoTextPage,
                params(&[("page", (index + 1).into())]),
            ));
        }
        let segments = pdfedit::page_segments(pdf, *page_id)?;
        let (mut table, mut kept_lines) = detect_table(&lines, &segments, word_ascent);
        if table.is_none()
            && let Some((inferred, kept)) = detect_aligned_table(&lines, word_ascent)
        {
            table = Some(inferred);
            kept_lines = kept;
            warnings.push(Msg::with(
                MessageCode::PdfDocxTableInferred,
                params(&[("page", (index + 1).into())]),
            ));
        }
        if let (Some(table), Some(geometry)) = (&mut table, &page_geometry) {
            // Chromium paints grid borders as thin filled rectangles. Preserve
            // a uniform grid's measured width and colour instead of a default hairline.
            let borders: Vec<_> = geometry
                .fills
                .iter()
                .filter_map(|(rect, color)| {
                    let [x0, y0, x1, y1] = rect.0;
                    let width = x1 - x0;
                    let height = y1 - y0;
                    let thin = width.min(height);
                    (x0 >= table.min_x - 1.0
                        && x1 <= table.max_x + 1.0
                        && y0 >= table.min_y - 1.0
                        && y1 <= table.max_y + 1.0
                        && (0.1..=3.0).contains(&thin)
                        && width.max(height) > 4.0)
                        .then_some((thin, color))
                })
                .collect();
            if let Some((width, color)) = borders.first()
                && borders.len() >= 3
                && borders
                    .iter()
                    .all(|(w, c)| (w - width).abs() < 0.01 && c == color)
            {
                table.border = Some(((width * 8.0).round().max(1.0) as usize, (*color).clone()));
            }
            for row in &mut table.rows {
                for cell in row {
                    cell.fill = geometry.fills.iter().rev().find_map(|(rect, color)| {
                        let [x0, y0, x1, y1] = rect.0;
                        (cell.x >= x0 && cell.x <= x1 && cell.y >= y0 && cell.y <= y1)
                            .then(|| color.clone())
                    });
                }
            }
        }
        let mut frags: Vec<Frag> = kept_lines.iter().map(|line| Frag {
            page,
            ..Frag::from(line)
        }).collect();
        assets::link_runs(
            pdf,
            *page_id,
            &segments,
            &mut frags,
            index + 1,
            &mut warnings,
        )?;
        assets::resolve_fonts(&mut frags, index + 1, &mut warnings)?;
        if let Some(table) = &mut table {
            for cell in table.rows.iter_mut().flatten() {
                for frag in &mut cell.runs {
                    frag.page = page;
                }
                assets::link_runs(
                    pdf,
                    *page_id,
                    &segments,
                    &mut cell.runs,
                    index + 1,
                    &mut warnings,
                )?;
                assets::resolve_fonts(&mut cell.runs, index + 1, &mut warnings)?;
            }
        }
        let mut blocks = layout_page(&frags, table.as_ref(), page, &mut warnings);
        if let Some(geometry) = &page_geometry {
            position_blocks(&mut blocks, table.as_ref(), geometry, word_ascent, page)?;
            assets::paragraph_backgrounds(&mut blocks, geometry);
            assets::insert_vectors(
                pdf,
                (*page_id, index + 1),
                &mut blocks,
                geometry,
                table.as_ref(),
                &lines,
                &mut warnings,
            )?;
        }
        children.extend(blocks.into_iter().map(|block| block.child));

        for stream in pdfedit::page_images(pdf, *page_id)? {
            if let Some((png, width, height)) = image_png(&stream).or_else(|| jpeg_png(&stream)) {
                let run = Run::new().add_image(Pic::new_with_dimensions(png, width, height));
                children.push(DocumentChild::Paragraph(Box::new(
                    Paragraph::new().add_run(run),
                )));
                warnings.push(Msg::with(
                    MessageCode::PdfDocxImageAtEnd,
                    params(&[("page", (index + 1).into())]),
                ));
            } else {
                warnings.push(Msg::with(
                    MessageCode::PdfDocxImageDecodeFailed,
                    params(&[("page", (index + 1).into())]),
                ));
            }
        }
    }

    let mut document = Document::new();
    document.children = children;
    let mut docx = docx_rs::Docx::new()
        .add_style(
            docx_rs::Style::new("PdfHeading", docx_rs::StyleType::Paragraph).name("PDF heading"),
        )
        .default_fonts(
            docx_rs::RunFonts::new()
                .ascii("Noto Sans")
                .hi_ansi("Noto Sans"),
        )
        .default_size(22)
        .default_line_spacing(docx_rs::LineSpacing::new().line(240).after(120));
    assets::number_lists(&mut docx, &mut document);
    docx.document = document;
    if let Some(geometry) = page_geometry {
        docx.document.section_property = section_geometry(&geometry);
    }
    let mut built = docx.build();
    // docx-rs enables Word's legacy single-/double-byte width balancing.
    // Writer applies it across face changes, adding about a space at each
    // bold boundary. PDF text already has its intended glyph advances.
    let settings =
        String::from_utf8(built.settings).map_err(|err| Error::DocxBuild(Box::new(err)))?;
    built.settings = settings
        .replace("<w:balanceSingleByteDoubleByteWidth />", "")
        .into_bytes();
    let data = crate::docx::pack_built(built)?;
    let mut unique = Vec::new();
    for warning in warnings {
        if !unique.contains(&warning) {
            unique.push(warning);
        }
    }
    Ok(BuildResult {
        data,
        warnings: unique,
    })
}

fn ocr_children(markdown: &str) -> Vec<DocumentChild> {
    use crate::markdown::Block;
    let text = |runs: Vec<crate::markdown::Inline>| runs.into_iter().map(|r| r.text).collect::<String>();
    let paragraph = |value: String| DocumentChild::Paragraph(Box::new(Paragraph::new().add_run(Run::new().add_text(value))));
    crate::markdown::to_blocks(markdown).into_iter().map(|block| match block {
        Block::Table { rows, .. } => DocumentChild::Table(Box::new(Table::new(rows.into_iter().map(|row| {
            TableRow::new(row.into_iter().map(|cell| TableCell::new().add_paragraph(Paragraph::new().add_run(Run::new().add_text(text(cell))))).collect())
        }).collect()))),
        Block::ListItem { ordered, number, content, .. } => paragraph(format!("{} {}", if ordered { format!("{number}.") } else { "•".into() }, text(content))),
        Block::Heading(_, runs) | Block::Paragraph(runs) | Block::Quote(runs) => paragraph(text(runs)),
        Block::Code(value) => paragraph(value),
        Block::Rule => paragraph(String::new()),
    }).collect()
}

/// Frames keep editable paragraphs at PDF coordinates without rewrapping
/// them to the much tighter ink-derived section margins.
fn position_blocks(
    blocks: &mut [LayoutBlock],
    table: Option<&DetectedTable>,
    geometry: &pdfedit::PageBounds,
    word_ascent: f64,
    page: usize,
) -> Result<(), Error> {
    let [x0, _, x1, y1] = geometry.media;
    for block in blocks {
        match &mut block.child {
            DocumentChild::Paragraph(p) => {
                if let Some(line) = block.source.first() {
                    if line.page != page {
                        return Err(Error::invalid(MessageCode::ReadInvalid, Default::default()));
                    }
                    // Word's line box starts one font ascent above the
                    // baseline, independently of the glyph's ink box.
                    p.property.frame_property = Some(
                        docx_rs::FrameProperty::new()
                            .h_anchor("page")
                            .v_anchor("page")
                            .wrap("none")
                            .x(((line.x - x0) * 20.0).round() as i32)
                            .y(
                                ((y1 - line.y - ((line.size * 2.0).round() / 2.0) * word_ascent)
                                    * 20.0)
                                    .round() as i32,
                            )
                            .width(((x1 - line.x) * 20.0).round() as u32),
                    );
                    p.property.line_spacing = Some(docx_rs::LineSpacing::new().before(0).after(0));
                }
            }
            DocumentChild::Table(t) => {
                if let Some(table) = table {
                    t.property = t.property.clone().position(
                        docx_rs::TablePositionProperty::new()
                            .horizontal_anchor("page")
                            .vertical_anchor("page")
                            .position_x(((table.min_x - x0) * 20.0).round() as i32)
                            .position_y(((y1 - table.max_y) * 20.0).round() as i32),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn section_geometry(geometry: &pdfedit::PageBounds) -> docx_rs::SectionProperty {
    let [x0, y0, x1, y1] = geometry.media;
    let [left, top, right, bottom] = geometry.margins();
    let mut section = docx_rs::SectionProperty::new()
        .page_size(docx_rs::PageSize::new().size(
            ((x1 - x0) * 20.0).round() as u32,
            ((y1 - y0) * 20.0).round() as u32,
        ))
        .page_margin(
            docx_rs::PageMargin::new()
                .left((left * 20.0).round() as i32)
                .right((right * 20.0).round() as i32)
                .top((top * 20.0).round() as i32)
                .bottom((bottom * 20.0).round() as i32),
        );
    section.section_type = Some(docx_rs::SectionType::NextPage);
    section
}

/// One positioned text fragment of a page.
#[derive(Debug, Clone)]
struct Frag {
    page: usize,
    text: String,
    x: f64,
    x_end: f64,
    y: f64,
    size: f64,
    bold: bool,
    italic: bool,
    font: String,
    color: String,
    source_font: String,
    href: Option<String>,
    underline: bool,
}

impl From<&PositionedLine> for Frag {
    fn from(line: &PositionedLine) -> Self {
        Self {
            page: 0,
            text: line.text.clone(),
            x: line.x,
            x_end: line.x + line.width,
            y: line.y,
            size: line.size,
            bold: is_bold(&line.font),
            italic: is_italic(&line.font),
            font: font_family(&line.font),
            color: line.color.clone(),
            source_font: line.font.clone(),
            href: None,
            underline: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Full,
    Left,
    Right,
}

/// A table found on a page, with its bounding box in page space.
struct DetectedTable {
    rows: Vec<Vec<Cell>>,
    widths: Vec<usize>,
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    heights: Vec<f64>,
    border: Option<(usize, String)>,
}

#[derive(Debug, Clone)]
struct Cell {
    text: String,
    runs: Vec<Frag>,
    bold: bool,
    italic: bool,
    size: f64,
    x: f64,
    y: f64,
    left: f64,
    top: f64,
    fill: Option<String>,
}

fn table_child(table: &DetectedTable) -> DocumentChild {
    let rows = table
        .rows
        .iter()
        .enumerate()
        .map(|(row, cells)| {
            TableRow::new(
                cells
                    .iter()
                    .map(|cell| {
                        let mut paragraph = run_paragraph(&cell.runs);
                        paragraph.property.line_spacing =
                            Some(docx_rs::LineSpacing::new().before(0).after(0));
                        let mut out = TableCell::new().add_paragraph(paragraph);
                        out.property = out.property.margins(
                            docx_rs::CellMargins::new()
                                .margin_top(
                                    // Word includes the top border in the cell's inset.
                                    ((cell.top
                                        - table
                                            .border
                                            .as_ref()
                                            .map_or(0.25, |(size, _)| *size as f64 / 8.0))
                                    .max(0.0)
                                        * 20.0)
                                        .round() as usize,
                                    docx_rs::WidthType::Dxa,
                                )
                                .margin_left(
                                    (cell.left * 20.0).round() as usize,
                                    docx_rs::WidthType::Dxa,
                                )
                                .margin_bottom(0, docx_rs::WidthType::Dxa)
                                .margin_right(0, docx_rs::WidthType::Dxa),
                        );
                        if let Some(fill) = &cell.fill {
                            out = out.shading(docx_rs::Shading::new().fill(fill));
                        }
                        out
                    })
                    .collect::<Vec<_>>(),
            )
            // OOXML row heights are integral twips. Truncating a value such
            // as 479.99997 in Writer loses a twip on every successive row.
            .row_height((table.heights[row] * 20.0).round() as f32)
            .height_rule(docx_rs::HeightRule::Exact)
        })
        .collect::<Vec<_>>();
    let mut out = Table::new(rows)
        .set_grid(table.widths.clone())
        .width(table.widths.iter().sum(), docx_rs::WidthType::Dxa);
    if let Some((size, color)) = &table.border {
        let mut borders = docx_rs::TableBorders::new();
        for position in [
            docx_rs::TableBorderPosition::Top,
            docx_rs::TableBorderPosition::Bottom,
            docx_rs::TableBorderPosition::Left,
            docx_rs::TableBorderPosition::Right,
            docx_rs::TableBorderPosition::InsideH,
            docx_rs::TableBorderPosition::InsideV,
        ] {
            let border = docx_rs::TableBorder::new(position).size(*size).color(color);
            borders = borders.set(if *size == 0 {
                border.border_type(docx_rs::BorderType::Nil)
            } else {
                border
            });
        }
        out = out.set_borders(borders);
    }
    DocumentChild::Table(Box::new(out))
}

/// A paragraph retaining each source face, colour and link boundary.
fn run_paragraph(runs: &[Frag]) -> Paragraph {
    let mut paragraph = Paragraph::new();
    if let Some(first) = runs.first() {
        let size = runs.iter().map(|f| f.size).fold(0.0, f64::max);
        paragraph = paragraph.size((size * 2.0).round() as usize).fonts(
            docx_rs::RunFonts::new()
                .ascii(&first.font)
                .hi_ansi(&first.font),
        );
    }
    if list_marker(&runs.iter().map(|f| f.text.as_str()).collect::<String>()).is_some()
        && let Some(first) = runs.first()
        && let Some(content) = runs
            .iter()
            .skip(1)
            .find(|f| f.x > first.x && !f.text.trim().is_empty())
    {
        let offset = ((content.x - first.x) * 20.0).round() as i32;
        paragraph = paragraph.indent(
            Some(offset),
            Some(docx_rs::SpecialIndentType::Hanging(offset)),
            None,
            None,
        );
    }
    for frag in runs {
        if frag.text.is_empty() {
            continue;
        }
        let mut run = Run::new()
            .size((frag.size * 2.0).round() as usize)
            .fonts(
                docx_rs::RunFonts::new()
                    .ascii(&frag.font)
                    .hi_ansi(&frag.font),
            )
            .color(&frag.color);
        for (index, line) in frag.text.split('\n').enumerate() {
            if index > 0 {
                run = run.add_break(docx_rs::BreakType::TextWrapping);
            }
            run = run.add_text(line);
        }
        if frag.bold {
            run = run.bold();
        }
        if frag.italic {
            run = run.italic();
        }
        if frag.underline {
            run = run.underline("single");
        }
        paragraph = if let Some(href) = &frag.href {
            paragraph.add_hyperlink(
                docx_rs::Hyperlink::new(href, docx_rs::HyperlinkType::External).add_run(run),
            )
        } else {
            paragraph.add_run(run)
        };
    }
    paragraph
}

/// Keep every source fragment beside its paragraph through layout and graphics.
struct LayoutBlock {
    child: DocumentChild,
    source: Vec<Frag>,
}

impl From<DocumentChild> for LayoutBlock {
    fn from(child: DocumentChild) -> Self {
        Self { child, source: Vec::new() }
    }
}

/// Lay out one page: full-width bands in order, columns column by column,
/// the table at its own place.
fn layout_page(
    frags: &[Frag],
    table: Option<&DetectedTable>,
    page: usize,
    warnings: &mut Vec<Msg>,
) -> Vec<LayoutBlock> {
    if frags.is_empty() {
        return table
            .map(|table| vec![table_child(table).into()])
            .unwrap_or_default();
    }
    let median = median_size(frags);
    // A font change is not a column boundary. Detect gutters from whole
    // visual rows, joining adjacent fragments before classifying columns.
    let mut visual = frags.to_vec();
    visual.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal))
    });
    let mut joined: Vec<Frag> = Vec::new();
    for frag in visual {
        if let Some(last) = joined.last_mut()
            && (last.y - frag.y).abs() < 2.0
            && frag.x - last.x_end < frag.size * 2.0
        {
            last.x_end = last.x_end.max(frag.x_end);
            last.text.push_str(&frag.text);
        } else {
            joined.push(frag);
        }
    }
    let gutter = find_gutter(&joined);
    if gutter.is_none() {
        let mut out = Vec::new();
        for above in [true, false] {
            let selected: Vec<_> = frags
                .iter()
                .filter(|f| table.is_none_or(|t| (f.y > t.max_y) == above))
                .collect();
            if above || table.is_some() {
                out.extend(
                    column_blocks(&selected, median)
                        .into_iter()
                        .map(|(_, child)| child),
                );
            }
            if above && let Some(table) = table {
                out.push(table_child(table).into());
            }
        }
        return out;
    }
    if let Some(gutter) = gutter {
        let _ = gutter;
        warnings.push(Msg::with(
            MessageCode::PdfDocxColumns,
            params(&[("page", page.into()), ("count", 2.into())]),
        ));
    }
    let classify = |frag: &Frag| -> Column {
        let frag = joined
            .iter()
            .find(|row| {
                (row.y - frag.y).abs() < 2.0
                    && row.x <= frag.x + 0.5
                    && row.x_end >= frag.x_end - 0.5
            })
            .unwrap_or(frag);
        let Some((gx0, gx1)) = gutter else {
            return Column::Full;
        };
        if frag.x < gx0 && frag.x_end > gx1 {
            return Column::Full;
        }
        let mid = (gx0 + gx1) / 2.0;
        if (frag.x + frag.x_end) / 2.0 < mid {
            Column::Left
        } else {
            Column::Right
        }
    };

    // Separators split the page into horizontal bands. A table that crosses
    // the gutter (or any table on a single-column page) is a separator; a
    // table inside one column stays with that column.
    enum Sep<'a> {
        Line(Vec<&'a Frag>),
        Table(&'a DetectedTable),
    }
    let table_is_separator = match (table, gutter) {
        (Some(table), Some((gx0, gx1))) => table.min_x < gx0 && table.max_x > gx1,
        (Some(_), None) => true,
        (None, _) => false,
    };
    let mut separators: Vec<(f64, Sep<'_>)> = frags
        .iter()
        .filter(|frag| classify(frag) == Column::Full)
        .map(|frag| (frag.y, Sep::Line(vec![frag])))
        .collect();
    if table_is_separator && let Some(table) = table {
        separators.push((table.max_y, Sep::Table(table)));
    }
    separators.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));

    let mut grouped: Vec<(f64, Sep<'_>)> = Vec::new();
    for (y, sep) in separators {
        if let Sep::Line(ref row) = sep
            && let Some((previous_y, Sep::Line(previous))) = grouped.last_mut()
            && (*previous_y - y).abs() < 2.0
        {
            previous.extend(row);
            continue;
        }
        grouped.push((y, sep));
    }
    let separators = grouped;
    let band_of = |y: f64| {
        separators
            .iter()
            .take_while(|(sep_y, _)| *sep_y > y + 1.0)
            .count()
    };
    let column_table = if table_is_separator { None } else { table };
    let column_table_band = column_table.map(|table| band_of((table.min_y + table.max_y) / 2.0));
    let column_table_side = match (column_table, gutter) {
        (Some(table), Some((gx0, gx1))) => {
            if (table.min_x + table.max_x) / 2.0 < (gx0 + gx1) / 2.0 {
                Column::Left
            } else {
                Column::Right
            }
        }
        _ => Column::Left,
    };

    let mut out: Vec<LayoutBlock> = Vec::new();
    for band in 0..=separators.len() {
        for side in [Column::Left, Column::Right] {
            let column_frags: Vec<&Frag> = frags
                .iter()
                .filter(|frag| classify(frag) == side && band_of(frag.y) == band)
                .collect();
            let mut blocks = column_blocks(&column_frags, median);
            if column_table_side == side
                && column_table_band == Some(band)
                && let Some(table) = column_table
            {
                // Keep the table at its height inside the column.
                let position = blocks
                    .iter()
                    .position(|(top_y, _)| *top_y < table.max_y)
                    .unwrap_or(blocks.len());
                blocks.insert(position, (table.max_y, table_child(table).into()));
            }
            out.extend(blocks.into_iter().map(|(_, child)| child));
        }
        if let Some((_, separator)) = separators.get(band) {
            match separator {
                Sep::Line(frag) => {
                    out.extend(
                        column_blocks(frag, median)
                            .into_iter()
                            .map(|(_, child)| child),
                    );
                }
                Sep::Table(table) => out.push(table_child(table).into()),
            }
        }
    }
    // A column table whose band held no fragments is still emitted.
    if let Some(table) = column_table {
        let placed = out
            .iter()
            .any(|block| matches!(block.child, DocumentChild::Table(_)));
        if !placed {
            out.push(table_child(table).into());
        }
    }
    out
}

fn median_size(frags: &[Frag]) -> f64 {
    let mut sizes: Vec<f64> = frags.iter().map(|frag| frag.size).collect();
    sizes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    sizes.get(sizes.len() / 2).copied().unwrap_or(12.0)
}

/// Paragraphs of one column band, each with the y of its first row. Fragments
/// on one baseline become runs of one row, so a bold fragment inside a line
/// stays a bold run; rows close together merge into one paragraph.
fn column_blocks(frags: &[&Frag], median: f64) -> Vec<(f64, LayoutBlock)> {
    if frags.is_empty() {
        return Vec::new();
    }
    let mut sorted: Vec<&Frag> = frags.to_vec();
    sorted.sort_by(|a, b| {
        b.y.partial_cmp(&a.y)
            .unwrap_or(Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal))
    });

    // Group fragments into rows by baseline.
    let mut rows: Vec<Vec<&Frag>> = Vec::new();
    for frag in sorted {
        match rows.last_mut() {
            Some(row) if (frag.y - row[0].y).abs() <= (0.4 * frag.size).max(2.0) => row.push(frag),
            _ => rows.push(vec![frag]),
        }
    }
    for row in &mut rows {
        row.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(Ordering::Equal));
    }

    let mut out = Vec::new();
    let mut index = 0usize;
    while index < rows.len() {
        let first_row = &rows[index];
        let top_y = first_row[0].y;
        let size = first_row.iter().map(|frag| frag.size).fold(0.0, f64::max);
        let heading = size > median * 1.35;
        let mut runs: Vec<Frag> = Vec::new();
        append_row(&mut runs, first_row, heading, true);
        index += 1;
        while index < rows.len() {
            let previous = &rows[index - 1];
            let next = &rows[index];
            let gap = previous[0].y - next[0].y;
            let next_size = next.iter().map(|frag| frag.size).fold(0.0, f64::max);
            let next_list = list_marker(&row_text(next)).is_some();
            let first_list = list_marker(&row_text(first_row)).is_some();
            let same_style = previous[0].font == next[0].font
                && previous[0].bold == next[0].bold
                && previous[0].italic == next[0].italic;
            if gap > 0.0
                && gap < size * 1.7
                && (next_size - size).abs() <= 1.5
                && same_style
                && !heading
                && !next_list
                && !first_list
            {
                append_row(&mut runs, next, heading, false);
                index += 1;
            } else {
                break;
            }
        }
        out.push((
            top_y,
            LayoutBlock {
                source: runs.clone(),
                child: DocumentChild::Paragraph(Box::new(if heading {
                    run_paragraph(&runs)
                        .style("PdfHeading")
                        .outline_lvl(0)
                        .keep_next(false)
                } else {
                    run_paragraph(&runs)
                })),
            },
        ));
    }
    out
}

fn append_row(runs: &mut Vec<Frag>, row: &[&Frag], _heading: bool, first: bool) {
    // Positioned paragraphs preserve physical rows with explicit breaks;
    // ink-derived section margins must not decide their wrapping again.
    if !first && let Some(last) = runs.last_mut() {
        last.text.push('\n');
    }
    let mut previous_end: Option<f64> = None;
    for frag in row {
        let mut text = frag.text.clone();
        // Adjacent PDF fragments can both carry the same boundary space.
        // Keep the first run's space, not a second copy in the next face.
        if previous_end.is_some() && runs.last().is_some_and(|last| last.text.ends_with(' ')) {
            text = text.trim_start_matches(' ').to_string();
        }
        if let Some(end) = previous_end
            && frag.x - end > frag.size * 0.3
            && !text.starts_with(' ')
            && runs.last().is_some_and(|last| !last.text.ends_with(' '))
        {
            text.insert(0, ' ');
        }
        previous_end = Some(frag.x_end);
        let mut run = (*frag).clone();
        run.text = text;
        runs.push(run);
    }
}

/// Find a vertical gutter: an empty strip that separates the fragments into
/// a left and a right group, wide enough to be a column gap. Fragments that
/// cross the strip count against it, so the strip is chosen where the page
/// really is two-column, not from start positions alone.
fn find_gutter(frags: &[Frag]) -> Option<(f64, f64)> {
    if frags.len() < 4 {
        return None;
    }
    let left = frags.iter().map(|frag| frag.x).fold(f64::MAX, f64::min);
    let right = frags.iter().map(|frag| frag.x_end).fold(f64::MIN, f64::max);
    let width = right - left;
    if width < 120.0 {
        return None;
    }
    let mut starts: Vec<f64> = frags.iter().map(|frag| frag.x_end).collect();
    let mut ends: Vec<f64> = frags.iter().map(|frag| frag.x).collect();
    starts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    ends.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));

    let mut best: Option<(i64, f64, f64, f64)> = None; // (score, strip width, gx0, gx1)
    for gx0 in &starts {
        if *gx0 < left + width * 0.15 {
            continue;
        }
        for gx1 in &ends {
            if *gx1 > right - width * 0.15 || gx1 - gx0 < 14.0 {
                continue;
            }
            let mut left_count = 0i64;
            let mut right_count = 0i64;
            let mut crossing = 0i64;
            for frag in frags {
                if frag.x_end <= *gx0 + 0.5 {
                    left_count += 1;
                } else if frag.x >= *gx1 - 0.5 {
                    right_count += 1;
                } else {
                    crossing += 1;
                }
            }
            if left_count < 2 || right_count < 2 || crossing * 2 > left_count + right_count {
                continue;
            }
            let score = left_count + right_count - 3 * crossing;
            let candidate = (score, gx1 - gx0, *gx0, *gx1);
            if best
                .map(|(best_score, best_width, _, _)| {
                    score > best_score || (score == best_score && gx1 - gx0 > best_width)
                })
                .unwrap_or(true)
            {
                best = Some(candidate);
            }
        }
    }
    best.map(|(_, _, gx0, gx1)| (gx0, gx1))
}

/// Find a table from grid lines: return it (rows top first) and the lines
/// that are not part of it.
fn detect_table(
    lines: &[PositionedLine],
    segments: &[Segment],
    word_ascent: f64,
) -> (Option<DetectedTable>, Vec<PositionedLine>) {
    let kept_all = || (None, lines.to_vec());
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let mut min_x = f64::MAX;
    let mut max_x = f64::MIN;
    let mut min_y = f64::MAX;
    let mut max_y = f64::MIN;
    // Select one connected grid by repeated horizontal extents, rather than
    // mixing table borders with signature rules elsewhere on the page.
    let mut horizontal: Vec<_> = segments
        .iter()
        .filter(|s| (s.y1 - s.y0).abs() < 0.5 && (s.x1 - s.x0).abs() > 4.0)
        .copied()
        .collect();
    horizontal.sort_by(|a, b| {
        a.y0.total_cmp(&b.y0)
            .then(a.x0.min(a.x1).total_cmp(&b.x0.min(b.x1)))
    });
    // Browsers paint each cell border as its own thin rectangle. Join
    // touching horizontal pieces before selecting the repeated table extent.
    let mut merged: Vec<Segment> = Vec::new();
    for s in horizontal {
        let (left, right) = (s.x0.min(s.x1), s.x0.max(s.x1));
        if let Some(last) = merged.last_mut()
            && (last.y0 - s.y0).abs() < 1.5
            && left <= last.x1 + 2.0
        {
            last.x0 = last.x0.min(left);
            last.x1 = last.x1.max(right);
        } else {
            merged.push(Segment {
                x0: left,
                x1: right,
                ..s
            });
        }
    }
    let horizontal = merged;
    let extent = horizontal
        .iter()
        .map(|s| {
            let (left, right) = (s.x0.min(s.x1), s.x0.max(s.x1));
            let matching: Vec<_> = horizontal
                .iter()
                .filter(|t| {
                    (t.x0.min(t.x1) - left).abs() < 2.0 && (t.x0.max(t.x1) - right).abs() < 2.0
                })
                .collect();
            // Equal-width rules elsewhere (a code background, for example)
            // are not grid rows. Only continue across a band whose vertical
            // borders actually connect both horizontal boundaries.
            let ys = cluster(matching.iter().map(|t| t.y0).collect(), 1.5);
            let mut start = 0;
            let mut best = (0, left, right, 0.0, 0.0);
            for i in 1..ys.len() {
                let connected = segments
                    .iter()
                    .filter(|v| {
                        (v.x1 - v.x0).abs() < 0.5
                            && v.x0 >= left - 2.0
                            && v.x0 <= right + 2.0
                            && v.y0.min(v.y1) <= ys[i - 1] + 1.5
                            && v.y0.max(v.y1) >= ys[i] - 1.5
                    })
                    .count()
                    >= 2;
                if !connected {
                    start = i;
                }
                if i + 1 - start > best.0 {
                    best = (i + 1 - start, left, right, ys[start], ys[i]);
                }
            }
            best
        })
        .filter(|(count, _, _, low, high)| *count >= 3 && high - low > 4.0)
        .max_by_key(|v| v.0);
    let Some((_, left, right, low, high)) = extent else {
        return kept_all();
    };
    for segment in segments {
        if segment.x0.min(segment.x1) < left - 2.0
            || segment.x0.max(segment.x1) > right + 2.0
            || segment.y0.min(segment.y1) < low - 2.0
            || segment.y0.max(segment.y1) > high + 2.0
        {
            continue;
        }
        let dx = (segment.x1 - segment.x0).abs();
        let dy = (segment.y1 - segment.y0).abs();
        if dy < 0.5 && dx > 4.0 {
            ys.push(segment.y0);
            min_x = min_x.min(segment.x0.min(segment.x1));
            max_x = max_x.max(segment.x0.max(segment.x1));
        } else if dx < 0.5 && dy > 4.0 {
            xs.push(segment.x0);
            min_y = min_y.min(segment.y0.min(segment.y1));
            max_y = max_y.max(segment.y0.max(segment.y1));
        }
    }
    let xs = cluster(xs, 1.5);
    // For thin filled borders, row origins are their upper edges. Keeping
    // the lower edge adds the first border's width to every later row.
    ys.sort_by(|a, b| b.total_cmp(a));
    ys.dedup_by(|a, b| (*a - *b).abs() <= 1.5);
    if xs.len() < 2 || ys.len() < 2 || min_x >= max_x || min_y >= max_y {
        return kept_all();
    }

    // The selected connected grid, never unrelated vertical paths, owns
    // the table's bottom and top edges.
    min_y = low;
    max_y = high;
    let inside = |line: &PositionedLine| {
        line.x >= min_x - 1.0 && line.x <= max_x + 1.0 && line.y >= min_y && line.y <= max_y
    };
    let used: Vec<&PositionedLine> = lines.iter().filter(|line| inside(line)).collect();
    let kept: Vec<PositionedLine> = lines.iter().filter(|line| !inside(line)).cloned().collect();
    if used.len() < 2 {
        return kept_all();
    }

    // Row positions: the grid's horizontal lines, descending (top row first).
    let mut row_lines = ys.clone();
    row_lines.push(max_y);
    row_lines.push(min_y);
    row_lines.sort_by(|a, b| b.partial_cmp(a).unwrap_or(Ordering::Equal));
    row_lines.dedup_by(|a, b| (*a - *b).abs() < 1.5);

    let mut table: Vec<Vec<Cell>> = Vec::new();
    let mut heights = Vec::new();
    for window in row_lines.windows(2) {
        let (high, low) = (window[0], window[1]);
        if high - low < 1.0 {
            continue;
        }
        let mut cells = vec![
            Cell {
                text: String::new(),
                runs: Vec::new(),
                bold: false,
                italic: false,
                size: 0.0,
                x: 0.0,
                y: 0.0,
                left: 0.0,
                top: 0.0,
                fill: None,
            };
            xs.len().saturating_sub(1).max(1)
        ];
        for line in &used {
            if line.y < low - 1.0 || line.y > high + 1.0 {
                continue;
            }
            let column = column_for_x(line.x, &xs);
            let cell = &mut cells[column];
            if !cell.text.is_empty() && !cell.text.ends_with(' ') && !line.text.starts_with(' ') {
                cell.text.push(' ');
            }
            if cell.text.is_empty() {
                cell.x = line.x;
                cell.y = line.y;
                cell.left = (line.x - xs[column]).max(0.0);
                let word_size = (line.size * 2.0).round() / 2.0;
                cell.top = (high - line.y - word_size * word_ascent).max(0.0);
            }
            cell.text.push_str(if cell.text.ends_with(' ') {
                line.text.trim_start_matches(' ')
            } else {
                &line.text
            });
            let frag = Frag::from(*line);
            let refs = [&frag];
            let same_row = cell
                .runs
                .last()
                .is_none_or(|last| (last.y - line.y).abs() < 2.0);
            append_row(&mut cell.runs, &refs, false, same_row);
            cell.bold |= is_bold(&line.font);
            cell.italic |= is_italic(&line.font);
            cell.size = cell.size.max(line.size);
        }
        if cells.iter().any(|cell| !cell.text.trim().is_empty()) {
            table.push(cells);
            heights.push(high - low);
        }
    }
    if table.len() < 2 {
        return kept_all();
    }
    (
        Some(DetectedTable {
            rows: table,
            heights,
            border: None,
            widths: xs
                .windows(2)
                .map(|v| ((v[1] - v[0]) * 20.0).round() as usize)
                .collect(),
            min_x,
            max_x,
            min_y,
            max_y,
        }),
        kept,
    )
}

/// Infer a simple unruled table only from three or more consecutive rows
/// whose separated cells share left or right column edges. A prose row ends
/// the table immediately; its baseline cannot become a new table row.
fn detect_aligned_table(
    lines: &[PositionedLine],
    word_ascent: f64,
) -> Option<(DetectedTable, Vec<PositionedLine>)> {
    let mut sorted: Vec<_> = lines.iter().collect();
    sorted.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));
    let mut rows: Vec<Vec<&PositionedLine>> = Vec::new();
    for line in sorted {
        if let Some(row) = rows.last_mut()
            && (row[0].y - line.y).abs() < 2.0
        {
            row.push(line);
        } else {
            rows.push(vec![line]);
        }
    }
    let separate = |row: &[&PositionedLine]| {
        row.len() >= 2 && row.windows(2).all(|v| v[1].x - v[0].x - v[0].width >= 18.0)
    };
    for start in 0..rows.len() {
        let first = &rows[start];
        if !separate(first) {
            continue;
        }
        let mut end = start + 1;
        while let Some(next) = rows.get(end) {
            let previous = &rows[end - 1];
            if !separate(next)
                || next.len() != first.len()
                || previous[0].y - next[0].y > first[0].size * 3.0
                || !next.iter().zip(first).all(|(a, b)| {
                    (a.x - b.x).abs() < 3.0 || (a.x + a.width - b.x - b.width).abs() < 3.0
                })
            {
                break;
            }
            end += 1;
        }
        if end - start < 3 {
            continue;
        }
        let selected = &rows[start..end];
        // Without rules, repeated prose columns are ambiguous. Require a
        // consistently numeric value column rather than turning an article
        // into a table merely because its paragraphs share column starts.
        if !(1..first.len()).any(|col| {
            selected.iter().skip(1).all(|row| {
                let value = row[col].text.trim();
                !value.is_empty()
                    && value.chars().any(|c| c.is_ascii_digit())
                    && value
                        .chars()
                        .all(|c| c.is_ascii_digit() || ".,%-+$ €£".contains(c))
            })
        }) {
            continue;
        }
        let min_x = selected.iter().map(|r| r[0].x).fold(f64::MAX, f64::min);
        let max_x = selected
            .iter()
            .filter_map(|r| r.last())
            .map(|l| l.x + l.width)
            .fold(0.0, f64::max);
        let mut xs = vec![min_x];
        for col in 1..first.len() {
            let left = selected
                .iter()
                .map(|r| r[col - 1].x + r[col - 1].width)
                .fold(0.0, f64::max);
            let right = selected.iter().map(|r| r[col].x).fold(f64::MAX, f64::min);
            xs.push((left + right) / 2.0);
        }
        xs.push(max_x + 1.0);
        let top = first[0].y + first[0].size * 1.1;
        let last = rows[end - 1][0];
        let bottom = last.y - last.size * 0.35;
        let mut ys = vec![top];
        ys.extend(selected.windows(2).map(|r| (r[0][0].y + r[1][0].y) / 2.0));
        ys.push(bottom);
        let segments: Vec<_> = xs
            .iter()
            .map(|x| Segment {
                x0: *x,
                x1: *x,
                y0: bottom,
                y1: top,
            })
            .chain(ys.iter().map(|y| Segment {
                x0: min_x,
                x1: max_x + 1.0,
                y0: *y,
                y1: *y,
            }))
            .collect();
        let (table, kept) = detect_table(lines, &segments, word_ascent);
        if let Some(mut table) = table {
            table.border = Some((0, "auto".into()));
            return Some((table, kept));
        }
    }
    None
}

fn column_for_x(x: f64, xs: &[f64]) -> usize {
    let mut column = 0usize;
    for (index, boundary) in xs.iter().enumerate() {
        if x >= *boundary {
            column = index;
        }
    }
    column.min(xs.len().saturating_sub(2))
}

fn cluster(mut values: Vec<f64>, tolerance: f64) -> Vec<f64> {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let mut out: Vec<f64> = Vec::new();
    for value in values {
        match out.last_mut() {
            Some(last) if (value - *last).abs() <= tolerance => {}
            _ => out.push(value),
        }
    }
    out
}

fn font_family(font: &str) -> String {
    let name = font.split_once('+').map_or(font, |(_, name)| name);
    let lower = name.to_ascii_lowercase();
    if lower.contains("mono") || lower.contains("courier") {
        "Noto Sans Mono".into()
    } else if lower.contains("serif") || lower.contains("times") {
        "Noto Serif".into()
    } else if lower.contains("notosans") || lower.contains("helvetica") {
        "Noto Sans".into()
    } else {
        name.split('-').next().unwrap_or(name).to_string()
    }
}

fn paragraph_text(p: &Paragraph) -> String {
    fn children(items: &[docx_rs::ParagraphChild], out: &mut String) {
        for item in items {
            match item {
                docx_rs::ParagraphChild::Run(run) => {
                    for child in &run.children {
                        if let docx_rs::RunChild::Text(t) = child {
                            out.push_str(&t.text);
                        }
                    }
                }
                docx_rs::ParagraphChild::Hyperlink(link) => children(&link.children, out),
                _ => {}
            }
        }
    }
    let mut out = String::new();
    children(&p.children, &mut out);
    out
}

fn row_text(row: &[&Frag]) -> String {
    row.iter()
        .map(|f| f.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The marker and byte length of an explicitly printed PDF list prefix.
fn list_marker(text: &str) -> Option<(bool, usize, usize)> {
    let text = text.trim_start();
    for marker in ["•", "◦", "▪", "●"] {
        if let Some(rest) = text.strip_prefix(marker) {
            let spaces = rest.len() - rest.trim_start().len();
            return Some((false, 1, marker.len() + spaces));
        }
    }
    let end = text.find(['.', ')'])?;
    let number = text.get(..end)?.parse::<usize>().ok()?;
    let rest = text.get(end + 1..)?;
    let spaces = rest.len() - rest.trim_start().len();
    (spaces > 0).then_some((true, number, end + 1 + spaces))
}

fn is_bold(font: &str) -> bool {
    let lower = font.to_ascii_lowercase();
    lower.contains("bold") || lower.contains("black")
}

fn is_italic(font: &str) -> bool {
    let lower = font.to_ascii_lowercase();
    lower.contains("italic") || lower.contains("oblique")
}

/// Decode a raster image XObject into PNG bytes.
fn image_png(stream: &Stream) -> Option<(Vec<u8>, u32, u32)> {
    let width = stream.dict.get(b"Width").ok()?.as_i64().ok()? as u32;
    let height = stream.dict.get(b"Height").ok()?.as_i64().ok()? as u32;
    let bits = stream
        .dict
        .get(b"BitsPerComponent")
        .ok()
        .and_then(|value| value.as_i64().ok())
        .unwrap_or(8);
    if bits != 8 || width == 0 || height == 0 {
        return None;
    }
    let colorspace = stream
        .dict
        .get(b"ColorSpace")
        .ok()
        .and_then(|value| value.as_name().ok())
        .map(|name| name.to_vec())?;
    let data = crate::pdfbudget::stream(stream).ok()?;
    let png = match colorspace.as_slice() {
        b"DeviceRGB" | b"RGB" => {
            let image = image::RgbImage::from_raw(width, height, data)?;
            encode_png(image::DynamicImage::ImageRgb8(image))
        }
        b"DeviceGray" | b"G" => {
            let image = image::GrayImage::from_raw(width, height, data)?;
            encode_png(image::DynamicImage::ImageLuma8(image))
        }
        _ => None,
    }?;
    Some((png, width, height))
}

fn jpeg_png(stream: &Stream) -> Option<(Vec<u8>, u32, u32)> {
    let is_jpeg = stream
        .dict
        .get(b"Filter")
        .ok()
        .and_then(|value| value.as_name().ok())
        .is_some_and(|name| name == b"DCTDecode");
    if !is_jpeg {
        return None;
    }
    let image = image::load_from_memory(&stream.content).ok()?;
    let width = image.width();
    let height = image.height();
    let png = encode_png(image)?;
    Some((png, width, height))
}

fn encode_png(image: image::DynamicImage) -> Option<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod round10 {
    use super::*;

    #[test]
    fn filled_pdf_grid_retains_border_rows_and_final_paragraph_coordinates()
    -> Result<(), Box<dyn std::error::Error>> {
        use lopdf::{Object, dictionary};
        let mut pdf = PdfDocument::with_version("1.5");
        let pages = pdf.new_object_id();
        let font = pdf.add_object(
            dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
        );
        // Chromium-style filled 0.75pt border rectangles, not stroked centre lines.
        let stream = ".2 .2 .2 rg\n57 697.25 330 .75 re f\n57 673.25 330 .75 re f\n57 650 330 .75 re f\n57 626 330 .75 re f\n57 626 .75 72 re f\n216 626 .75 72 re f\n386.25 626 .75 72 re f\n0 0 0 rg\nBT /F1 11 Tf 1 0 0 1 61.75 681.5 Tm (Header) Tj ET\nBT /F1 11 Tf 1 0 0 1 61.75 657.5 Tm (North) Tj ET\nBT /F1 11 Tf 1 0 0 1 61.75 633.5 Tm (South) Tj ET\nBT /F1 11 Tf 1 0 0 1 57 614 Tm (Final paragraph.) Tj ET";
        let content = pdf.add_object(Stream::new(dictionary! {}, stream.as_bytes().to_vec()));
        let page = pdf.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            "Contents" => content, "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } }
        });
        pdf.objects.insert(
            pages,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 },
            ),
        );
        let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        pdf.trailer.set("Root", catalog);
        let built = build(&pdf, &[page])?;
        let xml = String::from_utf8(
            crate::docxxml::entry(&built.data, "word/document.xml")?.ok_or("document")?,
        )?;
        assert_eq!(
            xml.matches("<w:trHeight w:val=\"480\" w:hRule=\"exact\"")
                .count(),
            2,
            "{xml}"
        );
        assert!(
            xml.contains("<w:trHeight w:val=\"465\" w:hRule=\"exact\""),
            "{xml}"
        );
        assert!(
            xml.contains("<w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"0\" w:color=\"333333\""),
            "{xml}"
        );
        let ascent = crate::pdf::family_metrics("Noto Sans")?
            .ok_or("font metrics")?
            .0;
        let y = ((228.0 - 11.0 * ascent) * 20.0).round() as i32;
        assert!(xml.contains(&format!("w:y=\"{y}\"")), "{xml}");
        assert!(xml.contains("Final paragraph."));
        Ok(())
    }

    #[test]
    fn table_row_heights_round_to_integral_twips_without_cumulative_loss()
    -> Result<(), Box<dyn std::error::Error>> {
        let table = DetectedTable {
            rows: vec![vec![], vec![], vec![]],
            heights: vec![24.749998, 23.249998, 23.999998],
            border: None,
            widths: vec![2000],
            min_x: 0.0,
            max_x: 100.0,
            min_y: 0.0,
            max_y: 72.0,
        };
        let mut docx = docx_rs::Docx::new();
        docx.document.children.push(table_child(&table));
        let bytes = crate::docx::pack_built(docx.build())?;
        let xml = String::from_utf8(
            crate::docxxml::entry(&bytes, "word/document.xml")?.ok_or("document")?,
        )?;
        for height in [495, 465, 480] {
            assert!(
                xml.contains(&format!("<w:trHeight w:val=\"{height}\" w:hRule=\"exact\"")),
                "{xml}"
            );
        }
        Ok(())
    }
}
