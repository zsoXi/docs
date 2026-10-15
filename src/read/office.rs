//! OpenDocument, presentations and bounded worksheet tables.

use super::{
    Document, escape, invalid, selection, table,
    word::destination,
    xml::{self, Node},
};
use crate::messages::{Msg, params};
use crate::{Error, MessageCode};
use serde_json::json;
use std::collections::BTreeMap;

fn label(code: MessageCode) -> String {
    Msg::new(code).render("en")
}

pub(super) fn odf(bytes: &[u8], format: &str, spec: Option<&str>) -> Result<Document, Error> {
    let mut root = xml::part(bytes, "content.xml")?;
    let styles = xml::entry(bytes, "styles.xml")?
        .map(|b| xml::parse(&b))
        .transpose()?;
    let mut definitions = Vec::new();
    if let Some(styles) = &styles {
        styles.descendants("style", &mut definitions);
    }
    root.descendants("style", &mut definitions);
    let sizes: BTreeMap<String, f64> = definitions
        .iter()
        .filter_map(|n| {
            n.child("text-properties")
                .and_then(|p| p.attr("font-size").strip_suffix("pt"))
                .and_then(|s| s.parse().ok())
                .map(|size| (n.attr("name").into(), size))
        })
        .collect();
    let emphasis: BTreeMap<String, (String, Option<bool>, Option<bool>)> = definitions.iter().map(|n| {
        let properties = n.child("text-properties");
        let bold = properties.map(|p| p.attr("font-weight")).filter(|s| !s.is_empty()).map(|s| s == "bold");
        let italic = properties.map(|p| p.attr("font-style")).filter(|s| !s.is_empty()).map(|s| s == "italic");
        (n.attr("name").into(), (n.attr("parent-style-name").into(), bold, italic))
    }).collect();
    let mut list_definitions = Vec::new();
    if let Some(styles) = &styles {
        styles.descendants("list-style", &mut list_definitions);
    }
    root.descendants("list-style", &mut list_definitions);
    let ordered: BTreeMap<String, BTreeMap<usize, u64>> = list_definitions
        .iter()
        .map(|n| {
            (
                n.attr("name").into(),
                n.all("list-level-style-number")
                    .filter_map(|v| v.attr("level").parse().ok().map(|level| {
                        (level, v.attr("start-value").parse().unwrap_or(1))
                    }))
                    .collect(),
            )
        })
        .collect();
    mark_odf_lists(&mut root, &ordered, "", 0);
    mark_odf_emphasis(&mut root, &emphasis, (false, false));
    infer_odf_headings(&mut root, &sizes);
    let mut doc = Document::default();
    if format == "odp" {
        let mut slides = Vec::new();
        root.descendants("page", &mut slides);
        for index in selection(spec, slides.len())? {
            let slide = slides[index - 1];
            let title =
                Msg::with(MessageCode::ReadSlide, params(&[("slide", index.into())])).render("en");
            let mut text = format!("# {title}\n\n");
            for child in &slide.children {
                if child.name == "notes" {
                    text.push_str(&format!(
                        "\n\n## {}\n\n{}",
                        label(MessageCode::ReadNotes),
                        odf_text(child, 0)
                    ));
                } else {
                    text.push_str(&odf_text(child, 0));
                }
            }
            doc.push(
                text.trim().to_string(),
                json!({"kind":"slide", "slide":index}),
            );
        }
    } else {
        let body = root
            .find("body")
            .and_then(|n| n.child("text"))
            .ok_or_else(invalid)?;
        let blocks: Vec<_> = body
            .children
            .iter()
            .filter(|n| !n.name.is_empty() && n.name != "sequence-decls")
            .collect();
        for index in selection(spec, blocks.len())? {
            doc.push(
                odf_text(blocks[index - 1], 0).trim().to_string(),
                json!({"kind":"paragraph", "paragraph":index}),
            );
        }
    }
    Ok(doc)
}

fn mark_odf_lists(
    node: &mut Node,
    styles: &BTreeMap<String, BTreeMap<usize, u64>>,
    parent: &str,
    depth: usize,
) {
    let mut style = parent.to_string();
    let mut depth = depth;
    if node.name == "list" {
        depth += 1;
        if !node.attr("style-name").is_empty() {
            style = node.attr("style-name").into();
        }
        if let Some(start) = styles.get(&style).and_then(|levels| levels.get(&depth)) {
            node.attrs.insert("_ordered".into(), "true".into());
            let mut number = *start;
            for item in node.children.iter_mut().filter(|n| n.name == "list-item") {
                number = item.attr("start-value").parse().unwrap_or(number);
                item.attrs.insert("_number".into(), number.to_string());
                number = number.saturating_add(1);
            }
        }
    }
    for child in &mut node.children {
        mark_odf_lists(child, styles, &style, depth);
    }
}

type EmphasisStyles = BTreeMap<String, (String, Option<bool>, Option<bool>)>;

fn mark_odf_emphasis(node: &mut Node, styles: &EmphasisStyles, inherited: (bool, bool)) {
    let mut chain = Vec::new();
    let mut name = node.attr("style-name");
    for _ in 0..32 {
        let Some(style) = styles.get(name) else { break };
        chain.push(style);
        name = &style.0;
    }
    let mut effective = inherited;
    for (_, bold, italic) in chain.into_iter().rev() {
        effective = (bold.unwrap_or(effective.0), italic.unwrap_or(effective.1));
    }
    if node.name.is_empty() {
        node.attrs.insert("_emphasis".into(), match effective {
            (true, true) => "***", (true, false) => "**", (false, true) => "*", _ => "",
        }.into());
    }
    for child in &mut node.children {
        mark_odf_emphasis(child, styles, effective);
    }
}

fn infer_odf_headings(node: &mut Node, sizes: &BTreeMap<String, f64>) {
    if node.name == "p" {
        let mut spans = Vec::new();
        node.descendants("span", &mut spans);
        let text: String = spans.iter().map(|n| n.value()).collect();
        if text.trim() == node.value().trim() && !text.trim().is_empty() {
            let smallest = spans
                .iter()
                .map(|n| sizes.get(n.attr("style-name")).copied().unwrap_or(11.0))
                .fold(f64::INFINITY, f64::min);
            if smallest >= 14.0 {
                node.name = "h".into();
                node.attrs.insert(
                    "outline-level".into(),
                    if smallest >= 20.0 {
                        "1"
                    } else if smallest >= 16.0 {
                        "2"
                    } else {
                        "3"
                    }
                    .into(),
                );
            }
        }
    }
    for child in &mut node.children {
        infer_odf_headings(child, sizes);
    }
}

fn odf_text(node: &Node, depth: usize) -> String {
    let content = || {
        node.children
            .iter()
            .map(|n| odf_text(n, depth))
            .collect::<String>()
    };
    match node.name.as_str() {
        "" => {
            let text = escape(&node.text);
            let marker = node.attr("_emphasis");
            if marker.is_empty() || text.trim().is_empty() { text } else {
                let start = text.len() - text.trim_start().len();
                let end = text.trim_end().len();
                format!("{}{marker}{}{marker}{}", &text[..start], &text[start..end], &text[end..])
            }
        },
        "h" => format!(
            "{} {}\n\n",
            "#".repeat(
                node.attr("outline-level")
                    .parse::<usize>()
                    .unwrap_or(1)
                    .clamp(1, 6)
            ),
            content().trim()
        ),
        "p" => format!("{}\n\n", content().trim()),
        "a" => format!("[{}]({})", content(), destination(node.attr("href"))),
        "s" => " ".repeat(node.attr("c").parse::<usize>().unwrap_or(1).min(1024)),
        "tab" => "\t".into(),
        "line-break" => "\n".into(),
        "list" => node
            .all("list-item")
            .map(|item| {
                let mut out = String::new();
                for child in &item.children {
                    if child.name == "list" {
                        out.push_str(&odf_text(child, depth + 1));
                    } else if !child.name.is_empty() {
                        out.push_str(&format!(
                            "{}{} {}\n",
                            "    ".repeat(depth),
                            if node.attr("_ordered") == "true" {
                                format!("{}.", item.attr("_number"))
                            } else {
                                "-".into()
                            },
                            odf_text(child, depth).trim()
                        ));
                    }
                }
                out
            })
            .collect(),
        "table" => {
            let mut rows = Vec::new();
            odf_rows(node, &mut rows);
            format!("{}\n\n", table(&rows))
        }
        "note" => {
            let id = node.attr("id");
            let body = node
                .child("note-body")
                .map(odf_text_body)
                .unwrap_or_default();
            format!("[^{id}]\n\n[^{id}]: {}\n\n", body.trim())
        }
        "image" | "binary-data" => String::new(),
        _ => content(),
    }
}
fn odf_text_body(node: &Node) -> String {
    odf_text(node, 0)
}
fn odf_rows(node: &Node, rows: &mut Vec<Vec<String>>) {
    for child in &node.children {
        if child.name == "table-row" {
            rows.push(
                child
                    .children
                    .iter()
                    .filter(|n| matches!(n.name.as_str(), "table-cell" | "covered-table-cell"))
                    .map(|n| odf_text(n, 0).trim().to_string())
                    .collect(),
            );
        } else {
            odf_rows(child, rows);
        }
    }
}

pub(super) fn presentation(bytes: &[u8], spec: Option<&str>) -> Result<Document, Error> {
    let root = xml::part(bytes, "ppt/presentation.xml")?;
    let rels = xml::rels(bytes, "ppt/presentation.xml")?;
    let slides: Vec<_> = root
        .find("sldIdLst")
        .ok_or_else(invalid)?
        .all("sldId")
        .collect();
    let mut doc = Document::default();
    for index in selection(spec, slides.len())? {
        let rel = rels
            .get(slides[index - 1].attr("id"))
            .filter(|r| !r.external)
            .ok_or_else(invalid)?;
        let slide = xml::part(bytes, &rel.target)?;
        let links = xml::rels(bytes, &rel.target)?;
        let title =
            Msg::with(MessageCode::ReadSlide, params(&[("slide", index.into())])).render("en");
        let mut text = format!("# {title}\n\n{}", slide_text(&slide, &links));
        for note in links
            .values()
            .filter(|r| r.kind == "notesSlide" && !r.external)
        {
            let root = xml::part(bytes, &note.target)?;
            text.push_str(&format!(
                "\n\n## {}\n\n{}",
                label(MessageCode::ReadNotes),
                slide_text(&root, &xml::rels(bytes, &note.target)?)
            ));
        }
        doc.push(
            text.trim().to_string(),
            json!({"kind":"slide", "slide":index}),
        );
    }
    Ok(doc)
}

fn slide_text(node: &Node, rels: &BTreeMap<String, xml::Rel>) -> String {
    if node.name == "sp"
        && node
            .find("ph")
            .is_some_and(|n| matches!(n.attr("type"), "title" | "ctrTitle"))
    {
        let title: String = node.children.iter().map(|n| slide_text(n, rels)).collect();
        return format!("## {}\n\n", title.trim());
    }
    if node.name == "sp"
        && node
            .find("ph")
            .is_some_and(|n| matches!(n.attr("type"), "sldNum" | "dt" | "ftr" | "hdr"))
    {
        return String::new();
    }
    if node.name == "tbl" {
        let rows: Vec<_> = node
            .all("tr")
            .map(|r| {
                r.all("tc")
                    .map(|c| slide_text(c, rels).trim().to_string())
                    .collect()
            })
            .collect();
        return format!("{}\n\n", table(&rows));
    }
    if node.name == "p" {
        let mut text = String::new();
        for run in &node.children {
            if run.name == "br" {
                text.push('\n');
                continue;
            }
            if !matches!(run.name.as_str(), "r" | "fld") {
                continue;
            }
            let mut value = run
                .child("t")
                .map(|t| escape(&t.value()))
                .unwrap_or_default();
            if let Some(link) = run.find("hlinkClick").and_then(|h| rels.get(h.attr("id"))) {
                value = format!("[{value}]({})", destination(&link.target));
            }
            text.push_str(&value);
        }
        if text.trim().is_empty() {
            return String::new();
        }
        let p = node.child("pPr");
        if let Some(props) = p
            && (props.child("buChar").is_some() || props.child("buAutoNum").is_some())
        {
            let depth = props.attr("lvl").parse::<usize>().unwrap_or(0).min(32);
            return format!(
                "{}{} {}\n",
                "  ".repeat(depth),
                if props.child("buAutoNum").is_some() {
                    "1."
                } else {
                    "-"
                },
                text.trim()
            );
        }
        return format!("{}\n\n", text.trim());
    }
    if node.name == "cNvPr" && !node.attr("descr").is_empty() {
        return format!("{}\n\n", escape(node.attr("descr")));
    }
    node.children.iter().map(|n| slide_text(n, rels)).collect()
}

#[derive(Clone)]
struct Area {
    sheet: Option<String>,
    first_col: usize,
    last_col: usize,
    first_row: usize,
    last_row: usize,
}
impl Area {
    fn parse(spec: Option<&str>) -> Result<Self, Error> {
        let mut area = Self {
            sheet: None,
            first_col: 1,
            last_col: 16384,
            first_row: 1,
            last_row: 1_048_576,
        };
        if let Some(spec) = spec {
            let (sheet, cells) = spec
                .rsplit_once('!')
                .map(|(s, c)| (Some(s.trim_matches('\'').replace("''", "'")), c))
                .unwrap_or((None, spec));
            let error =
                || Error::invalid(MessageCode::ReadRange, params(&[("range", spec.into())]));
            let (first, last) = cells.split_once(':').unwrap_or((cells, cells));
            let (c1, r1) = coordinate(first).ok_or_else(error)?;
            let (c2, r2) = coordinate(last).ok_or_else(error)?;
            if c1 > c2 || r1 > r2 {
                return Err(error());
            }
            area = Self {
                sheet,
                first_col: c1,
                last_col: c2,
                first_row: r1,
                last_row: r2,
            };
        }
        Ok(area)
    }
}
fn coordinate(value: &str) -> Option<(usize, usize)> {
    let value = value.trim();
    let split = value.find(|c: char| c.is_ascii_digit())?;
    let mut col = 0usize;
    for b in value[..split].bytes() {
        if !b.is_ascii_alphabetic() {
            return None;
        }
        col = col
            .checked_mul(26)?
            .checked_add((b.to_ascii_uppercase() - b'A' + 1) as usize)?;
    }
    let row = value[split..].parse().ok()?;
    if !(1..=16384).contains(&col) || !(1..=1_048_576).contains(&row) {
        return None;
    }
    Some((col, row))
}
fn column(mut col: usize) -> String {
    let mut name = Vec::new();
    while col > 0 {
        name.push((b'A' + ((col - 1) % 26) as u8) as char);
        col = (col - 1) / 26;
    }
    name.into_iter().rev().collect()
}

struct Sheet<'a> {
    area: &'a Area,
    limit: usize,
    rows: BTreeMap<usize, BTreeMap<usize, String>>,
    truncated: bool,
    cells: usize,
    bytes: usize,
}
impl<'a> Sheet<'a> {
    fn new(area: &'a Area, limit: usize) -> Self {
        Self {
            area,
            limit,
            rows: BTreeMap::new(),
            truncated: false,
            cells: 0,
            bytes: 0,
        }
    }
    fn cell(&mut self, row: usize, col: usize, text: String) -> Result<(), Error> {
        if row < self.area.first_row
            || row > self.area.last_row
            || col < self.area.first_col
            || col > self.area.last_col
            || text.is_empty()
        {
            return Ok(());
        }
        if row - self.area.first_row >= self.limit {
            self.truncated = true;
            return Ok(());
        }
        self.cells = self.cells.saturating_add(1);
        self.bytes = self.bytes.saturating_add(text.len());
        if self.cells > 1_000_000 || self.bytes > 100 * 1024 * 1024 {
            return Err(invalid());
        }
        self.rows.entry(row).or_default().insert(col, text);
        Ok(())
    }
    fn finish(self, name: &str, doc: &mut Document) -> Result<(), Error> {
        if self.truncated {
            doc.warnings.push(Msg::with(
                MessageCode::ReadRowsTruncated,
                params(&[("sheet", name.into()), ("max_rows", self.limit.into())]),
            ));
        }
        let Some(last_row) = self.rows.keys().next_back().copied() else {
            return Ok(());
        };
        let last_col = self
            .rows
            .values()
            .filter_map(|r| r.keys().next_back())
            .copied()
            .max()
            .unwrap_or(self.area.first_col);
        if (last_row - self.area.first_row + 1).saturating_mul(last_col - self.area.first_col + 1)
            > 1_000_000
        {
            return Err(invalid());
        }
        let rows: Vec<_> = (self.area.first_row..=last_row)
            .map(|r| {
                (self.area.first_col..=last_col)
                    .map(|c| {
                        self.rows
                            .get(&r)
                            .and_then(|row| row.get(&c))
                            .cloned()
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .collect();
        let range = format!(
            "{}{}:{}{}",
            column(self.area.first_col),
            self.area.first_row,
            column(last_col),
            last_row
        );
        doc.push(
            format!("## {}\n\n{}", escape(name), table(&rows)),
            json!({"kind":"sheet","sheet":name,"range":range}),
        );
        Ok(())
    }
}

pub(super) fn spreadsheet(
    bytes: &[u8],
    format: &str,
    spec: Option<&str>,
    max_rows: usize,
) -> Result<Document, Error> {
    let area = Area::parse(spec)?;
    // Bound output allocation even if the caller requests an entire sparse sheet.
    if max_rows == 0 || max_rows > 100_000 {
        return Err(Error::invalid(
            MessageCode::ReadRange,
            params(&[("range", "max_rows".into())]),
        ));
    }
    let mut doc = Document::default();
    let mut found = false;
    let accepts = |name: &str| area.sheet.as_ref().is_none_or(|s| s == name);
    match format {
        "xlsx" => {
            let workbook = xml::part(bytes, "xl/workbook.xml")?;
            let rels = xml::rels(bytes, "xl/workbook.xml")?;
            let shared = xml::entry(bytes, "xl/sharedStrings.xml")?
                .map(|b| xml::parse(&b))
                .transpose()?;
            let strings: Vec<_> = shared
                .as_ref()
                .map(|r| r.all("si").map(Node::value).collect())
                .unwrap_or_default();
            for sheet in workbook.find("sheets").ok_or_else(invalid)?.all("sheet") {
                let name = sheet.attr("name");
                if !accepts(name) {
                    continue;
                }
                found = true;
                let rel = rels
                    .get(sheet.attr("id"))
                    .filter(|r| !r.external)
                    .ok_or_else(invalid)?;
                let root = xml::part(bytes, &rel.target)?;
                let mut out = Sheet::new(&area, max_rows);
                if let Some(data) = root.child("sheetData") {
                    for row in data.all("row") {
                        for cell in row.all("c") {
                            let (col, row) = coordinate(cell.attr("r")).ok_or_else(invalid)?;
                            let raw = cell.child("v").map(Node::value).unwrap_or_default();
                            let value = match cell.attr("t") {
                                "s" => strings
                                    .get(raw.parse::<usize>().map_err(|_| invalid())?)
                                    .cloned()
                                    .ok_or_else(invalid)?,
                                "inlineStr" => {
                                    cell.child("is").map(Node::value).unwrap_or_default()
                                }
                                "b" => {
                                    if raw == "1" {
                                        "TRUE".into()
                                    } else {
                                        "FALSE".into()
                                    }
                                }
                                _ => {
                                    if raw.is_empty() {
                                        cell.child("f")
                                            .map(|f| format!("={}", f.value()))
                                            .unwrap_or_default()
                                    } else {
                                        raw
                                    }
                                }
                            };
                            out.cell(row, col, value)?;
                        }
                    }
                }
                if root.find("f").is_some() {
                    doc.warnings.push(Msg::with(
                        MessageCode::ReadFlattened,
                        params(&[("feature", "formulas".into())]),
                    ));
                }
                out.finish(name, &mut doc)?;
            }
        }
        "ods" => {
            let root = xml::part(bytes, "content.xml")?;
            let body = root.find("spreadsheet").ok_or_else(invalid)?;
            for sheet in body.all("table") {
                let name = sheet.attr("name");
                if !accepts(name) {
                    continue;
                }
                found = true;
                let mut out = Sheet::new(&area, max_rows);
                let mut row_nodes = Vec::new();
                sheet.descendants("table-row", &mut row_nodes);
                let mut row = 1usize;
                for node in row_nodes {
                    let repeat = repeat(node, "number-rows-repeated", 1_048_576)?;
                    let end = row
                        .checked_add(repeat)
                        .filter(|v| *v <= 1_048_577)
                        .ok_or_else(invalid)?;
                    let mut col = 1usize;
                    for cell in node
                        .children
                        .iter()
                        .filter(|n| matches!(n.name.as_str(), "table-cell" | "covered-table-cell"))
                    {
                        let count = repeat_cell(cell)?;
                        let col_end = col
                            .checked_add(count)
                            .filter(|v| *v <= 16385)
                            .ok_or_else(invalid)?;
                        let text = cell
                            .all("p")
                            .map(|p| odf_text(p, 0).trim().to_string())
                            .collect::<Vec<_>>()
                            .join(" ");
                        let text = if text.is_empty() {
                            cell.attr("value").to_string()
                        } else {
                            text
                        };
                        if !text.is_empty() {
                            let stop = end.min(area.last_row.saturating_add(1));
                            let cutoff = area.first_row.saturating_add(max_rows);
                            if stop > cutoff
                                && row <= area.last_row
                                && col < area.last_col.saturating_add(1)
                                && col_end > area.first_col
                            {
                                out.truncated = true;
                            }
                            for r in row.max(area.first_row)..stop.min(cutoff) {
                                for c in col.max(area.first_col)..col_end.min(area.last_col + 1) {
                                    out.cell(r, c, text.clone())?;
                                }
                            }
                        }
                        col = col_end;
                    }
                    row = end;
                }
                out.finish(name, &mut doc)?;
            }
        }
        "csv" => {
            let name = "CSV";
            if accepts(name) {
                found = true;
                let mut sheet = Sheet::new(&area, max_rows);
                for (r, row) in csv(bytes)?.into_iter().enumerate() {
                    for (c, value) in row.into_iter().enumerate() {
                        sheet.cell(r + 1, c + 1, value)?;
                    }
                }
                sheet.finish(name, &mut doc)?;
            }
        }
        _ => return Err(invalid()),
    }
    if !found && area.sheet.is_some() {
        return Err(Error::invalid(
            MessageCode::ReadRange,
            params(&[("range", spec.unwrap_or("").into())]),
        ));
    }
    Ok(doc)
}
fn repeat(node: &Node, attr: &str, max: usize) -> Result<usize, Error> {
    if node.attr(attr).is_empty() {
        return Ok(1);
    }
    node.attr(attr)
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0 && *n <= max)
        .ok_or_else(invalid)
}
fn repeat_cell(node: &Node) -> Result<usize, Error> {
    repeat(node, "number-columns-repeated", 16384)
}

fn csv(bytes: &[u8]) -> Result<Vec<Vec<String>>, Error> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?
        .trim_start_matches('\u{feff}');
    let mut chars = text.chars().peekable();
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut closed = false;
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cell.push('"');
                } else {
                    quoted = false;
                    closed = true;
                }
            } else {
                cell.push(ch);
            }
        } else {
            match ch {
                '"' if cell.is_empty() && !closed => quoted = true,
                ',' => {
                    row.push(std::mem::take(&mut cell));
                    closed = false;
                }
                '\r' | '\n' => {
                    if ch == '\r' && chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                    row.push(std::mem::take(&mut cell));
                    rows.push(std::mem::take(&mut row));
                    closed = false;
                }
                _ if closed || ch == '"' => return Err(invalid()),
                _ => cell.push(ch),
            }
        }
    }
    if quoted {
        return Err(invalid());
    }
    if !cell.is_empty() || !row.is_empty() || closed {
        row.push(cell);
        rows.push(row);
    }
    Ok(rows)
}
