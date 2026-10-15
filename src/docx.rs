//! Reading, writing and editing DOCX files.

use docx_rs::{
    BreakType, Document, DocumentChild, Docx, Hyperlink, HyperlinkType, Paragraph, ParagraphChild,
    Run, RunChild, RunFonts, Table, TableCell, TableCellContent, TableChild, TableRow,
    TableRowChild, read_docx,
};

use crate::error::Error;
use crate::markdown::{Block, Inline};

/// Extract the plain text of a DOCX file, one paragraph per line.
pub fn read_text(bytes: &[u8]) -> Result<String, Error> {
    let docx = parse(bytes)?;
    Ok(document_text(&docx.document))
}

/// Replace `find` in a DOCX and return the new bytes together with how many
/// occurrences were replaced and how many are present in the flattened text.
pub fn replace_text(
    bytes: &[u8],
    find: &str,
    replacement: &str,
    all: bool,
) -> Result<(Vec<u8>, usize, usize), Error> {
    crate::docxedit::replace(bytes, find, replacement, all)
}

/// Bound drawing extents without rebuilding unrelated document parts.
pub(crate) fn bound_images(bytes: &[u8], setup: &crate::pdf::PageSetup) -> Result<Vec<u8>, Error> {
    let xml = crate::docxxml::entry(bytes, "word/document.xml")?.ok_or_else(||
        Error::invalid(crate::messages::MessageCode::DocxPackageInvalid, crate::messages::Params::new()))?;
    let text = std::str::from_utf8(&xml).map_err(|source|
        Error::reader(crate::messages::MessageCode::DocxPackageInvalid, source))?;
    let mut edits = Vec::new();
    for tag in crate::htmlpdf::markers::tags(text) {
        if tag.closing || !matches!(tag.name.rsplit(':').next(), Some("extent" | "ext")) { continue; }
        let dimension = |name| tag.attr(name).flatten().and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0);
        let (Some(w), Some(h)) = (dimension("cx"), dimension("cy")) else { continue };
        let (width, height) = crate::document::fit(w / 9525.0, h / 9525.0, setup);
        for (name, before, after) in [("cx", w, width * 9525.0), ("cy", h, height * 9525.0)] {
            if (before - after).abs() >= 1.0 && let Some(range) = tag.attribute_range(name) {
                edits.push((range, format!("{name}=\"{:.0}\"", after).into_bytes()));
            }
        }
    }
    if edits.is_empty() { return Ok(bytes.to_vec()); }
    edits.sort_by_key(|(range, _)| range.start);
    crate::docxedit::replace_entry(bytes, "word/document.xml", &crate::content::splice(&xml, &edits))
}

/// Build a DOCX document from parsed markdown blocks.
pub fn blocks_to_docx(blocks: &[Block]) -> Result<Vec<u8>, Error> {
    use docx_rs::{
        AbstractNumbering, Level, LevelJc, LevelText, LineSpacing, NumberFormat, Numbering,
        SpecialIndentType, Start, Style, StyleType,
    };
    let spacing = || LineSpacing::new().line(240).after(120);
    let mut doc = Docx::new()
        .page_margin(
            docx_rs::PageMargin::new()
                .top(1134)
                .bottom(1134)
                .left(1134)
                .right(1134),
        )
        .default_fonts(RunFonts::new().ascii("Noto Sans").hi_ansi("Noto Sans"))
        .default_size(22)
        .default_line_spacing(spacing())
        .add_style(
            Style::new("Quote", StyleType::Paragraph)
                .name("Quote")
                .italic()
                .color("666666")
                .indent(Some(360), None, None, None),
        )
        .add_style(
            Style::new("Code", StyleType::Paragraph).name("Code").fonts(
                RunFonts::new()
                    .ascii("Noto Sans Mono")
                    .hi_ansi("Noto Sans Mono"),
            ),
        );
    let mut defined = std::collections::BTreeSet::new();
    for block in blocks {
        if let Block::ListItem {
            ordered,
            number,
            level,
            list,
            ..
        } = block
            && defined.insert(*list)
        {
            let format = if *ordered { "decimal" } else { "bullet" };
            let text = if *ordered {
                format!("%{}.", level + 1)
            } else {
                "•".into()
            };
            doc = doc
                .add_abstract_numbering(
                    AbstractNumbering::new(*list).add_level(
                        Level::new(
                            *level,
                            Start::new(*number as usize),
                            NumberFormat::new(format),
                            LevelText::new(text),
                            LevelJc::new("left"),
                        )
                        .indent(
                            Some(((*level + 1) * 360) as i32),
                            Some(SpecialIndentType::Hanging(180)),
                            None,
                            None,
                        )
                        .fonts(RunFonts::new().ascii("Noto Sans").hi_ansi("Noto Sans")),
                    ),
                )
                .add_numbering(Numbering::new(*list, *list));
        }
    }
    for block in blocks {
        doc = match block {
            Block::Heading(level, inlines) => doc.add_paragraph(heading(*level, inlines)),
            Block::Paragraph(inlines) => doc.add_paragraph(paragraph(inlines, None)),
            Block::ListItem {
                level,
                list,
                content,
                ..
            } => doc.add_paragraph(paragraph(content, None).numbering(
                docx_rs::NumberingId::new(*list),
                docx_rs::IndentLevel::new(*level),
            )),
            Block::Table { rows, alignments } => doc.add_table(table(rows, alignments)),
            Block::Quote(inlines) => doc.add_paragraph(paragraph(inlines, None).style("Quote")),
            Block::Code(code) => doc.add_paragraph(code_paragraph(code)),
            Block::Rule => doc.add_paragraph(rule_paragraph()),
        };
    }
    // docx-rs does not expose repeating table headers. Add the OOXML flag
    // to the first row of each generated Markdown table before ZIP packing.
    let mut built = doc.build();
    built.document = String::from_utf8_lossy(&built.document)
        .split_inclusive("</w:tbl>")
        .map(|part| part.replacen("<w:trPr />", "<w:trPr><w:tblHeader/></w:trPr>", 1))
        .collect::<String>()
        .into_bytes();
    pack_built(built)
}

/// Build a DOCX document from plain text, one paragraph per line.
pub fn text_to_docx(text: &str) -> Result<Vec<u8>, Error> {
    let mut doc = Docx::new();
    if text.is_empty() {
        doc = doc.add_paragraph(Paragraph::new());
    } else {
        for line in text.lines() {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(line)));
        }
    }
    pack(doc)
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Docx, Error> {
    read_docx(bytes).map_err(|err| Error::Docx(Box::new(err)))
}

pub(crate) fn pack(docx: Docx) -> Result<Vec<u8>, Error> {
    pack_built(docx.build())
}

pub(crate) fn pack_built(mut docx: docx_rs::XMLDocx) -> Result<Vec<u8>, Error> {
    // Builder counters are process-global for pictures and hyperlinks. Normalize
    // only generated package identifiers, never visible document text or URLs.
    let source = std::str::from_utf8(&docx.document)
        .map_err(|e| Error::DocxBuild(Box::new(e)))?;
    let mut ids = std::collections::BTreeMap::new();
    for tag in crate::htmlpdf::markers::tags(source) {
        for attr in ["r:id", "r:embed"] {
            if let Some(Some(id)) = tag.attr(attr) {
                let next = ids.len() + 1;
                ids.entry(id.to_string()).or_insert_with(|| format!("rIdGenerated{next}"));
            }
        }
    }
    let rewrite = |bytes: &mut Vec<u8>| -> Result<(), Error> {
        let text = std::str::from_utf8(bytes).map_err(|e| Error::DocxBuild(Box::new(e)))?;
        let mut edits = Vec::new();
        for tag in crate::htmlpdf::markers::tags(text) {
            for attr in ["r:id", "r:embed", "Id", "Target"] {
                if let Some(Some(value)) = tag.attr(attr) {
                    let media = value.strip_prefix("media/").and_then(|v| v.strip_suffix(".png"));
                    let replacement = ids.get(value).cloned().or_else(|| media.and_then(|v| ids.get(v)).map(|id| format!("media/{id}.png")));
                    if let (Some(value), Some(range)) = (replacement, tag.attribute_range(attr)) {
                        edits.push((range, format!("{attr}=\"{value}\"").into_bytes()));
                    }
                }
            }
        }
        *bytes = crate::content::splice(bytes, &edits);
        Ok(())
    };
    rewrite(&mut docx.document)?;
    rewrite(&mut docx.document_rels)?;
    // Hyperlink relationships originate in a HashMap. XML order is not semantic,
    // but must be stable before ZIP serialization, including many-link documents.
    let text=std::str::from_utf8(&docx.document_rels).map_err(|e|Error::DocxBuild(Box::new(e)))?;
    let ranges:Vec<_>=crate::htmlpdf::markers::tags(text).into_iter()
        .filter(|tag|tag.name=="relationship" && !tag.closing).map(|tag|tag.range).collect();
    let mut entries:Vec<_>=ranges.iter().map(|range|text[range.clone()].as_bytes().to_vec()).collect();
    entries.sort();
    let edits:Vec<_>=ranges.into_iter().zip(entries).collect();
    docx.document_rels=crate::content::splice(&docx.document_rels,&edits);
    for (id, _)  in &mut docx.media {
        if let Some(new) = ids.get(id) { *id = new.clone(); }
    }
    docx.media.sort_by(|a,b|a.0.cmp(&b.0));
    let mut cursor = std::io::Cursor::new(Vec::new());
    docx.pack(&mut cursor)
        .map_err(|err| Error::DocxBuild(Box::new(err)))?;
    Ok(cursor.into_inner())
}

fn heading(level: u8, inlines: &[Inline]) -> Paragraph {
    let half_points = match level {
        1 => 40usize,
        2 => 32,
        3 => 26,
        _ => 24,
    };
    let mut para = Paragraph::new().style(&format!("Heading{level}"));
    for inline in inlines {
        let mut inline = inline.clone();
        inline.bold = true;
        let mut run = styled_run(&inline).size(half_points);
        if inline.image.is_some() { run = styled_run(&inline); }
        para = append_inline(para, &inline, run);
    }
    para
}

fn paragraph(inlines: &[Inline], prefix: Option<&str>) -> Paragraph {
    let mut para = Paragraph::new();
    if let Some(prefix) = prefix {
        para = para.add_run(Run::new().add_text(prefix));
    }
    for inline in inlines {
        para = append_inline(para, inline, styled_run(inline));
    }
    para
}

fn append_inline(para: Paragraph, inline: &Inline, run: Run) -> Paragraph {
    match &inline.link {
        Some(url) => para.add_hyperlink(
            Hyperlink::new(url.clone(), HyperlinkType::External)
                .add_run(run.color("0645AD").underline("single")),
        ),
        None => para.add_run(run),
    }
}

fn styled_run(inline: &Inline) -> Run {
    let mut run = if let Some(image) = &inline.image {
        Run::new().add_image(docx_rs::Pic::new_with_dimensions(image.bytes.clone(), 1, 1).size(
            (image.width * 9525.0).round() as u32,
            (image.height * 9525.0).round() as u32,
        ))
    } else {
        Run::new().add_text(inline.text.as_str())
    };
    if inline.bold {
        run = run.bold();
    }
    if inline.italic {
        run = run.italic();
    }
    if inline.code {
        run = run.fonts(
            RunFonts::new()
                .ascii("Noto Sans Mono")
                .hi_ansi("Noto Sans Mono"),
        );
    }
    run
}

fn code_paragraph(code: &str) -> Paragraph {
    let font = || {
        RunFonts::new()
            .ascii("Noto Sans Mono")
            .hi_ansi("Noto Sans Mono")
    };
    let mut para = Paragraph::new().style("Code");
    para.property = para
        .property
        .shading(docx_rs::Shading::new().fill("F2F2F2"));
    let mut first = true;
    for line in code.lines() {
        if !first {
            para = para.add_run(Run::new().add_break(BreakType::TextWrapping));
        }
        first = false;
        para = para.add_run(Run::new().add_text(line).fonts(font()));
    }
    para
}

fn rule_paragraph() -> Paragraph {
    Paragraph::new().add_run(Run::new().add_text("-------------"))
}

fn table(rows: &[Vec<Vec<Inline>>], alignments: &[pulldown_cmark::Alignment]) -> Table {
    let table_rows = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let cells = row
                .iter()
                .enumerate()
                .map(|(column, cell)| {
                    let mut content = cell.clone();
                    if index == 0 {
                        for inline in &mut content {
                            inline.bold = true;
                        }
                    }
                    let align = match alignments.get(column) {
                        Some(pulldown_cmark::Alignment::Right) => docx_rs::AlignmentType::Right,
                        Some(pulldown_cmark::Alignment::Center) => docx_rs::AlignmentType::Center,
                        _ => docx_rs::AlignmentType::Left,
                    };
                    let cell =
                        TableCell::new().add_paragraph(paragraph(&content, None).align(align));
                    if index == 0 {
                        cell.shading(docx_rs::Shading::new().fill("E8EDF2"))
                    } else {
                        cell
                    }
                })
                .collect::<Vec<_>>();
            TableRow::new(cells)
        })
        .collect::<Vec<_>>();
    Table::new(table_rows)
}

fn document_text(document: &Document) -> String {
    let mut out = String::new();
    for child in &document.children {
        match child {
            DocumentChild::Paragraph(para) => {
                paragraph_text(para, &mut out);
                out.push('\n');
            }
            DocumentChild::Table(table) => table_text(table, &mut out),
            _ => {}
        }
    }
    out
}

fn paragraph_text(para: &Paragraph, out: &mut String) {
    for child in &para.children {
        match child {
            ParagraphChild::Run(run) => run_text(run, out),
            ParagraphChild::Hyperlink(link) => {
                for nested in &link.children {
                    if let ParagraphChild::Run(run) = nested {
                        run_text(run, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn run_text(run: &Run, out: &mut String) {
    for child in &run.children {
        match child {
            RunChild::Text(text) => out.push_str(&text.text),
            RunChild::Tab(_) => out.push('\t'),
            RunChild::Break(_) | RunChild::CarriageReturn(_) => out.push('\n'),
            _ => {}
        }
    }
}

fn table_text(table: &Table, out: &mut String) {
    for child in &table.rows {
        let TableChild::TableRow(row) = child;
        let mut first = true;
        for cell in &row.cells {
            let TableRowChild::TableCell(cell) = cell;
            if !first {
                out.push('\t');
            }
            first = false;
            for content in &cell.children {
                if let TableCellContent::Paragraph(para) = content {
                    paragraph_text(para, out);
                }
            }
        }
        out.push('\n');
    }
}
