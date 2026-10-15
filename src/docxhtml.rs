//! DOCX → HTML with OOXML style inheritance and table geometry.

use crate::docxxml::{self, Element};
use crate::error::Error;
use crate::messages::{MessageCode, Msg, params};
use crate::pdf::PageSetup;
use std::collections::BTreeMap;

pub(crate) type DocxHtml = crate::document::Document;

type Props = BTreeMap<String, Element>;
fn properties(node: Option<&Element>) -> Props {
    node.map(|v| {
        v.children
            .iter()
            .map(|v| (v.name.clone(), v.clone()))
            .collect()
    })
    .unwrap_or_default()
}
fn property<'a>(props: &'a Props, name: &str, attr: &str) -> Option<&'a str> {
    props.get(name)?.attr(attr)
}
fn numeric(text: Option<&str>) -> Option<f64> {
    text?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && v.abs() <= 2_000_000_000.0)
}
fn enabled(node: &Element) -> bool {
    !matches!(node.attr("val"), Some("0" | "false" | "off"))
}

pub(crate) fn convert(bytes: &[u8], docx: &docx_rs::Docx) -> Result<DocxHtml, Error> {
    let xml = docxxml::entry(bytes, "word/document.xml")?.ok_or_else(|| {
        Error::invalid(
            MessageCode::DocxPackageInvalid,
            crate::messages::Params::new(),
        )
    })?;
    let root = docxxml::parse(&xml)?;
    let styles = docxxml::entry(bytes, "word/styles.xml")?
        .map(|v| docxxml::parse(&v))
        .transpose()?
        .unwrap_or_default();
    let numberings = docxxml::entry(bytes, "word/numbering.xml")?
        .map(|v| docxxml::parse(&v))
        .transpose()?
        .unwrap_or_default();
    fn font_metrics(
        node: &Element,
        metrics: &mut BTreeMap<String, Option<(f64, f64)>>,
    ) -> Result<(), Error> {
        if node.name == "rFonts"
            && let Some(family) = node.attr("ascii").or_else(|| node.attr("hAnsi"))
            && !metrics.contains_key(family)
        {
            metrics.insert(family.to_string(), crate::pdf::family_metrics(family)?);
        }
        for child in &node.children {
            font_metrics(child, metrics)?;
        }
        Ok(())
    }
    let mut metrics = BTreeMap::new();
    font_metrics(&styles, &mut metrics)?;
    font_metrics(&root, &mut metrics)?;
    let mut converter = Converter {
        metrics,
        content_width: 0.0,
        body: String::new(),
        images: BTreeMap::new(),
        warnings: Vec::new(),
        families: Vec::new(),
        styles: styles
            .children
            .iter()
            .filter(|v| v.name == "style")
            .filter_map(|v| Some((v.attr("styleId")?.to_string(), v.clone())))
            .collect(),
        default_run: properties(styles.path(&["docDefaults", "rPrDefault", "rPr"])),
        default_para: properties(styles.path(&["docDefaults", "pPrDefault", "pPr"])),
        default_style: styles
            .children
            .iter()
            .find(|v| {
                v.name == "style"
                    && v.attr("type") == Some("paragraph")
                    && v.attr("default") == Some("1")
            })
            .and_then(|v| v.attr("styleId"))
            .unwrap_or("Normal")
            .to_string(),
        media: docx
            .images
            .iter()
            .map(|(id, _, original, preview)| {
                (
                    id.clone(),
                    if preview.0.is_empty() {
                        original.0.clone()
                    } else {
                        preview.0.clone()
                    },
                )
            })
            .collect(),
        links: docx
            .hyperlinks
            .iter()
            .map(|(id, url, _)| (id.clone(), url.clone()))
            .collect(),
        numberings,
        counts: BTreeMap::new(),
    };
    let mut setup = PageSetup::default();
    if let Some(section) = root.path(&["body", "sectPr"]) {
        if let Some(size) = section.child("pgSz") {
            if let Some(v) = numeric(size.attr("w")) {
                setup.width_mm = v * 25.4 / 1440.0;
            }
            if let Some(v) = numeric(size.attr("h")) {
                setup.height_mm = v * 25.4 / 1440.0;
            }
        }
        if let Some(margin) = section.child("pgMar") {
            for (name, target) in [
                ("top", &mut setup.margin_top_mm),
                ("bottom", &mut setup.margin_bottom_mm),
                ("left", &mut setup.margin_left_mm),
                ("right", &mut setup.margin_right_mm),
            ] {
                if let Some(v) = numeric(margin.attr(name)) {
                    *target = v * 25.4 / 1440.0;
                }
            }
        }
    }
    converter.content_width =
        (setup.width_mm - setup.margin_left_mm - setup.margin_right_mm) * 72.0 / 25.4;
    if let Some(body) = root.child("body") {
        converter.blocks(body);
    }
    let section = &docx.document.section_property;
    if let Some((_, header)) = section.get_headers().first() {
        let mut text = String::new();
        for child in &header.children {
            if let docx_rs::HeaderChild::Paragraph(p) = child {
                running_text(p, &mut text);
            }
        }
        setup.header = (!text.trim().is_empty()).then(|| text.trim().to_string());
    }
    if let Some((_, footer)) = section.get_footers().first() {
        let mut text = String::new();
        for child in &footer.children {
            if let docx_rs::FooterChild::Paragraph(p) = child {
                running_text(p, &mut text);
                setup.page_numbers |= p.children.iter().any(|v| {
                    matches!(
                        v,
                        docx_rs::ParagraphChild::PageNum(_) | docx_rs::ParagraphChild::NumPages(_)
                    )
                });
            }
        }
        setup.footer = (!text.trim().is_empty()).then(|| text.trim().to_string());
    }
    if !section.get_headers().is_empty() || !section.get_footers().is_empty() {
        converter
            .warnings
            .push(Msg::new(MessageCode::DocxHeadersPlain));
    }
    Ok(DocxHtml::embedded(
        converter.body, converter.images, converter.warnings, setup, converter.families,
    ))
}

struct Converter {
    content_width: f64,
    body: String,
    images: BTreeMap<String, Vec<u8>>,
    warnings: Vec<Msg>,
    families: Vec<String>,
    styles: BTreeMap<String, Element>,
    default_run: Props,
    default_para: Props,
    default_style: String,
    media: BTreeMap<String, Vec<u8>>,
    links: BTreeMap<String, String>,
    numberings: Element,
    counts: BTreeMap<(String, String), usize>,
    metrics: BTreeMap<String, Option<(f64, f64)>>,
}
impl Converter {
    fn style_props(&self, id: &str, kind: &str) -> Props {
        let mut chain = Vec::new();
        let mut next = Some(id);
        while let Some(id) = next {
            if chain.contains(&id) || chain.len() >= 64 {
                break;
            }
            chain.push(id);
            next = self.styles.get(id).and_then(|s| s.value("basedOn"));
        }
        let mut result = Props::new();
        for id in chain.into_iter().rev() {
            if let Some(style) = self.styles.get(id) {
                result.extend(properties(style.child(kind)));
            }
        }
        result
    }
    fn blocks(&mut self, node: &Element) {
        for child in &node.children {
            match child.name.as_str() {
                "p" => self.paragraph(child),
                "tbl" => self.table(child),
                _ => {}
            }
        }
    }
    fn paragraph(&mut self, node: &Element) {
        let direct = node.child("pPr");
        let style = direct
            .and_then(|v| v.value("pStyle"))
            .unwrap_or(&self.default_style);
        let mut p = self.default_para.clone();
        p.extend(self.style_props(style, "pPr"));
        p.extend(properties(direct));
        let mut r = self.default_run.clone();
        r.extend(self.style_props(style, "rPr"));
        let mut css = String::from("margin:0;");
        // The paragraph strut must use its resolved face and size as well as
        // its runs; otherwise a large heading sits on the body font baseline.
        css.push_str(&self.run_css(&r));
        if property(&p, "spacing", "line").is_none()
            && let Some((_, height)) = property(&r, "rFonts", "ascii")
                .and_then(|f| self.metrics.get(f))
                .copied()
                .flatten()
        {
            css.push_str(&format!("line-height:{height};"));
        }
        if let Some(value) = property(&p, "jc", "val") {
            let align = match value {
                "both" => "justify",
                "center" => "center",
                "right" | "end" => "right",
                _ => "left",
            };
            css.push_str(&format!("text-align:{align};"));
        }
        for (attr, name) in [("before", "margin-top"), ("after", "margin-bottom")] {
            if let Some(v) = numeric(property(&p, "spacing", attr)) {
                css.push_str(&format!("{name}:{}pt;", v / 20.0));
            }
        }
        if let Some(v) = numeric(property(&p, "spacing", "line")) {
            if property(&p, "spacing", "lineRule") == Some("exact") {
                css.push_str(&format!("line-height:{}pt;", v / 20.0));
            } else {
                // Automatic spacing is a multiple of the font's natural
                // line box, not a multiple of its em size (CSS unitless).
                let height = property(&r, "rFonts", "ascii")
                    .or_else(|| property(&r, "rFonts", "hAnsi"))
                    .and_then(|f| self.metrics.get(f))
                    .copied()
                    .flatten()
                    .map(|(_, h)| h)
                    .unwrap_or(1.2);
                css.push_str(&format!("line-height:{};", v / 240.0 * height));
            }
        }
        for (attr, name) in [
            ("left", "margin-left"),
            ("right", "margin-right"),
            ("firstLine", "text-indent"),
        ] {
            if let Some(v) = numeric(property(&p, "ind", attr)) {
                css.push_str(&format!("{name}:{}pt;", v / 20.0));
            }
        }
        if let Some(fill) = property(&p, "shd", "fill").filter(|v| hex_color(v)) {
            css.push_str(&format!("background-color:#{fill};"));
        }
        if p.get("pageBreakBefore").is_some_and(enabled) {
            css.push_str("page-break-before:always;");
        }
        self.body.push_str(&format!("<p style=\"{css}\">"));
        if let Some(num) = p.get("numPr") {
            let id = num.value("numId").unwrap_or("0");
            let level = num.value("ilvl").unwrap_or("0");
            let abstract_id = self
                .numberings
                .children
                .iter()
                .find(|n| n.name == "num" && n.attr("numId") == Some(id))
                .and_then(|n| n.value("abstractNumId"));
            let definition = self
                .numberings
                .children
                .iter()
                .find(|n| n.name == "abstractNum" && n.attr("abstractNumId") == abstract_id)
                .and_then(|n| {
                    n.children
                        .iter()
                        .find(|n| n.name == "lvl" && n.attr("ilvl") == Some(level))
                });
            let start = definition
                .and_then(|n| n.value("start"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let count = self
                .counts
                .entry((id.into(), level.into()))
                .or_insert(start);
            let marker = if definition.and_then(|n| n.value("numFmt")) == Some("bullet") {
                "•".into()
            } else {
                format!("{count}.")
            };
            *count += 1;
            let indent = level.parse::<usize>().unwrap_or(0).min(8) * 18;
            self.body.push_str(&format!(
                "<span style=\"padding-left:{indent}pt\">{marker} </span>"
            ));
        }
        self.inlines(node, &r);
        self.body.push_str("</p>\n");
    }
    fn inlines(&mut self, node: &Element, inherited: &Props) {
        let inherited_css = self.run_css(inherited);
        for child in &node.children {
            match child.name.as_str() {
                "r" => {
                    let mut props = inherited.clone();
                    if let Some(style) = child.path(&["rPr", "rStyle"]).and_then(|v| v.attr("val"))
                    {
                        props.extend(self.style_props(style, "rPr"));
                    }
                    props.extend(properties(child.child("rPr")));
                    let css = self.run_css(&props);
                    let styled = css != inherited_css;
                    if styled {
                        self.body
                            .push_str(&format!("<span style=\"{}\">", escape(&css)));
                    }
                    for part in &child.children {
                        match part.name.as_str() {
                            "t" => self.body.push_str(&escape(&part.text)),
                            "tab" => self.body.push_str("&#9;"),
                            "br" if part.attr("type") == Some("page") => self
                                .body
                                .push_str("<div style=\"page-break-before:always\"></div>"),
                            "br" | "cr" => self.body.push_str("<br/>"),
                            "drawing" => self.drawing(part),
                            _ => {}
                        }
                    }
                    if styled {
                        self.body.push_str("</span>");
                    }
                }
                "hyperlink" => {
                    let url = child
                        .attr("id")
                        .and_then(|id| self.links.get(id))
                        .cloned()
                        .unwrap_or_default();
                    self.body
                        .push_str(&format!("<a href=\"{}\">", escape(&url)));
                    self.inlines(child, inherited);
                    self.body.push_str("</a>");
                }
                _ => {}
            }
        }
    }
    fn run_css(&mut self, props: &Props) -> String {
        let mut css = String::new();
        for (prop, on, off) in [
            ("b", "font-weight:bold;", "font-weight:normal;"),
            ("i", "font-style:italic;", "font-style:normal;"),
        ] {
            if let Some(value) = props.get(prop) {
                css.push_str(if enabled(value) { on } else { off });
            }
        }
        if props.contains_key("u") && !matches!(property(props, "u", "val"), Some("none" | "0")) {
            css.push_str("text-decoration:underline;");
        }
        if let Some(color) = property(props, "color", "val").filter(|v| hex_color(v)) {
            css.push_str(&format!("color:#{color};"));
        }
        if let Some(size) = numeric(property(props, "sz", "val")) {
            css.push_str(&format!("font-size:{}pt;", size / 2.0));
        }
        if let Some(fill) = property(props, "shd", "fill").filter(|v| hex_color(v)) {
            css.push_str(&format!("background-color:#{fill};"));
        }
        if let Some(family) =
            property(props, "rFonts", "ascii").or_else(|| property(props, "rFonts", "hAnsi"))
        {
            if !self.families.iter().any(|v| v == family) {
                self.families.push(family.to_string());
            }
            // CSS identifiers are quoted and escaped before insertion in HTML.
            css.push_str(&format!(
                "font-family:'{}';",
                family.replace('\\', "\\\\").replace('\'', "\\'")
            ));
        }
        css
    }
    fn drawing(&mut self, node: &Element) {
        fn descendant<'a>(n: &'a Element, name: &str) -> Option<&'a Element> {
            if n.name == name {
                return Some(n);
            }
            n.children.iter().find_map(|v| descendant(v, name))
        }
        let Some(id) = descendant(node, "blip").and_then(|v| v.attr("embed")) else {
            return;
        };
        let Some(data) = self.media.get(id) else {
            self.warnings.push(Msg::with(
                MessageCode::DocxImageMissing,
                params(&[("id", id.into())]),
            ));
            return;
        };
        let name = format!("img{}", self.images.len());
        self.images.insert(name.clone(), data.clone());
        let extent = descendant(node, "extent");
        let w = extent.and_then(|v| numeric(v.attr("cx"))).unwrap_or(9525.0) / 9525.0;
        let h = extent.and_then(|v| numeric(v.attr("cy"))).unwrap_or(9525.0) / 9525.0;
        self.body.push_str(&format!("<div style=\"width:{w}px;height:{h}px\"><img src=\"{name}\" style=\"width:{w}px;height:{h}px\"/></div>"));
    }
    fn table(&mut self, table: &Element) {
        let grid: Vec<f64> = table
            .child("tblGrid")
            .map(|v| {
                v.children
                    .iter()
                    .filter_map(|v| numeric(v.attr("w")))
                    .map(|v| v / 20.0)
                    .collect()
            })
            .unwrap_or_default();
        let width = grid.iter().sum::<f64>();
        let width_css = if width > 0.0 {
            format!("{width}pt")
        } else {
            "100%".into()
        };
        self.body.push_str(&format!(
            "<table style=\"border-collapse:collapse;width:{width_css};margin:0\">"
        ));
        let rows: Vec<_> = table.children.iter().filter(|v| v.name == "tr").collect();
        for (ri, row) in rows.iter().enumerate() {
            self.body.push_str("<tr>");
            let mut column = 0;
            for cell in row.children.iter().filter(|v| v.name == "tc") {
                let prop = cell.child("tcPr");
                let span = prop
                    .and_then(|v| v.value("gridSpan"))
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 63);
                if let Some(merge) = prop.and_then(|v| v.child("vMerge"))
                    && merge.attr("val") != Some("restart")
                {
                    column += span;
                    continue;
                }
                let mut rowspan = 1;
                if prop.and_then(|v| v.child("vMerge")).is_some() {
                    for following in rows.iter().skip(ri + 1) {
                        let mut next_col = 0;
                        let mut continuation = false;
                        for next in following.children.iter().filter(|v| v.name == "tc") {
                            if next_col == column {
                                continuation = next
                                    .path(&["tcPr", "vMerge"])
                                    .is_some_and(|v| v.attr("val") != Some("restart"));
                                break;
                            }
                            next_col += next
                                .path(&["tcPr", "gridSpan"])
                                .and_then(|v| v.attr("val"))
                                .and_then(|v| v.parse::<usize>().ok())
                                .unwrap_or(1);
                        }
                        if !continuation {
                            break;
                        }
                        rowspan += 1;
                    }
                }
                let cell_width =
                    numeric(prop.and_then(|v| v.child("tcW")).and_then(|v| v.attr("w")))
                        .map(|v| v / 20.0)
                        .unwrap_or_else(|| grid.iter().skip(column).take(span).sum());
                let mut css = String::from("border:none;padding:0;vertical-align:top;");
                let mut horizontal_padding = 0.0;
                for side in ["top", "bottom", "left", "right"] {
                    let margin = prop
                        .and_then(|v| v.path(&["tcMar", side]))
                        .or_else(|| table.path(&["tblPr", "tblCellMar", side]));
                    // OOXML's implicit cell margins are 108 twips on the
                    // left/right and zero vertically, including without tcMar.
                    let value = numeric(margin.and_then(|v| v.attr("w"))).unwrap_or(
                        if matches!(side, "left" | "right") {
                            108.0
                        } else {
                            0.0
                        },
                    );
                    css.push_str(&format!("padding-{side}:{}pt;", value / 20.0));
                    if matches!(side, "left" | "right") {
                        horizontal_padding += value / 20.0;
                    }
                }
                if cell_width > 0.0 {
                    let content_width = (cell_width - horizontal_padding).max(0.0);
                    css.push_str(&format!("width:{content_width}pt;"));
                } else {
                    // Without a declared cell width the backend shrinks the
                    // inline alignment box to its text, even in a fixed table.
                    let columns = rows
                        .iter()
                        .map(|row| {
                            row.children
                                .iter()
                                .filter(|v| v.name == "tc")
                                .map(|cell| {
                                    cell.path(&["tcPr", "gridSpan"])
                                        .and_then(|v| v.attr("val"))
                                        .and_then(|v| v.parse::<usize>().ok())
                                        .unwrap_or(1)
                                        .clamp(1, 63)
                                })
                                .sum::<usize>()
                        })
                        .max()
                        .unwrap_or(1)
                        .max(1);
                    css.push_str(&format!(
                        "width:{}pt;",
                        (self.content_width * span as f64 / columns as f64 - horizontal_padding)
                            .max(0.0)
                    ));
                }
                for side in ["top", "bottom", "left", "right"] {
                    let inside = match side {
                        "top" if ri > 0 => "insideH",
                        "bottom" if ri + 1 < rows.len() => "insideH",
                        "left" if column > 0 => "insideV",
                        "right" if column + span < grid.len() => "insideV",
                        _ => side,
                    };
                    let border = prop
                        .and_then(|v| v.path(&["tcBorders", side]))
                        .or_else(|| table.path(&["tblPr", "tblBorders", inside]));
                    if let Some(border) = border {
                        let kind = match border.attr("val") {
                            Some("nil" | "none") => "none",
                            Some("double") => "double",
                            Some("dashed") => "dashed",
                            _ => "solid",
                        };
                        let size = numeric(border.attr("sz")).unwrap_or(4.0) / 6.0;
                        let color = border
                            .attr("color")
                            .filter(|v| hex_color(v))
                            .unwrap_or("333333");
                        css.push_str(&format!("border-{side}-width:{size}px;border-{side}-style:{kind};border-{side}-color:#{color};"));
                    }
                }
                if let Some(fill) = prop
                    .and_then(|v| v.child("shd"))
                    .and_then(|v| v.attr("fill"))
                    .filter(|v| hex_color(v))
                {
                    css.push_str(&format!("background-color:#{fill};"));
                }
                if let Some(paragraph) = cell.child("p") {
                    let style = paragraph
                        .path(&["pPr", "pStyle"])
                        .and_then(|v| v.attr("val"))
                        .unwrap_or(&self.default_style);
                    let mut inherited = self.default_run.clone();
                    inherited.extend(self.style_props(style, "rPr"));
                    let mut p = self.default_para.clone();
                    p.extend(self.style_props(style, "pPr"));
                    p.extend(properties(paragraph.child("pPr")));
                    css.push_str(&self.run_css(&inherited));
                    // Cell text is emitted inline, so its strut needs the same
                    // font-driven automatic line height as a paragraph box.
                    let height = property(&inherited, "rFonts", "ascii")
                        .or_else(|| property(&inherited, "rFonts", "hAnsi"))
                        .and_then(|f| self.metrics.get(f))
                        .copied()
                        .flatten()
                        .map(|(_, h)| h)
                        .unwrap_or(1.2);
                    if property(&p, "spacing", "lineRule") == Some("exact") {
                        if let Some(line) = numeric(property(&p, "spacing", "line")) {
                            css.push_str(&format!("line-height:{}pt;", line / 20.0));
                        }
                    } else {
                        let multiple =
                            numeric(property(&p, "spacing", "line")).unwrap_or(240.0) / 240.0;
                        css.push_str(&format!("line-height:{};", height * multiple));
                    }
                    if let Some(after) = numeric(property(&p, "spacing", "after")) {
                        let bottom = prop
                            .and_then(|v| v.path(&["tcMar", "bottom"]))
                            .or_else(|| table.path(&["tblPr", "tblCellMar", "bottom"]))
                            .and_then(|v| numeric(v.attr("w")))
                            .unwrap_or(0.0);
                        // Inline cell boxes in this backend retain a 1.4 em
                        // strut and count bottom padding twice. Account for
                        // that excess and the duplicated collapsed border,
                        // rather than accumulating a drift on every row.
                        let size = numeric(property(&inherited, "sz", "val")).unwrap_or(22.0) / 2.0;
                        let border = prop
                            .and_then(|v| v.path(&["tcBorders", "bottom"]))
                            .or_else(|| table.path(&["tblPr", "tblBorders", "insideH"]))
                            .and_then(|v| numeric(v.attr("sz")))
                            .unwrap_or(0.0)
                            / 8.0;
                        let excess = (1.4 - height).max(0.0) * size + border;
                        let automatic = numeric(property(&p, "spacing", "line")).is_some()
                            && property(&p, "spacing", "lineRule") != Some("exact");
                        let padding = if automatic {
                            (bottom / 20.0 + after / 20.0 - excess).max(0.0) / 2.0
                        } else {
                            (bottom + after) / 20.0
                        };
                        css.push_str(&format!("padding-bottom:{padding}pt;"));
                    }
                    if let Some(align) = property(&p, "jc", "val")
                        .filter(|v| matches!(*v, "left" | "center" | "right"))
                    {
                        css.push_str(&format!("text-align:{align};"));
                    }
                    if let Some(fill) = property(&p, "shd", "fill").filter(|v| hex_color(v)) {
                        css.push_str(&format!("background-color:#{fill};"));
                    }
                }
                let spans = format!(
                    "{}{}",
                    if span > 1 {
                        format!(" colspan=\"{span}\"")
                    } else {
                        String::new()
                    },
                    if rowspan > 1 {
                        format!(" rowspan=\"{rowspan}\"")
                    } else {
                        String::new()
                    }
                );
                self.body.push_str(&format!("<td{spans} style=\"{css}\">"));
                // The HTML backend drops fonts in paragraph boxes nested in
                // table cells. Keep run styling inline, with explicit breaks.
                for (index, paragraph) in cell.children.iter().filter(|v| v.name == "p").enumerate()
                {
                    if index > 0 {
                        self.body.push_str("<br/>");
                    }
                    let style = paragraph
                        .path(&["pPr", "pStyle"])
                        .and_then(|v| v.attr("val"))
                        .unwrap_or(&self.default_style);
                    let mut inherited = self.default_run.clone();
                    inherited.extend(self.style_props(style, "rPr"));
                    let mut p = self.default_para.clone();
                    p.extend(self.style_props(style, "pPr"));
                    p.extend(properties(paragraph.child("pPr")));
                    let automatic = numeric(property(&p, "spacing", "line")).is_some()
                        && property(&p, "spacing", "lineRule") != Some("exact");
                    // Explicit auto spacing needs a uniform inline strut in
                    // both styled and unstyled cells. Leave natural/exact
                    // boxes without an extra span, preserving their baseline.
                    if automatic {
                        let css = self.run_css(&inherited);
                        self.body
                            .push_str(&format!("<span style=\"{}\">", escape(&css)));
                    }
                    self.inlines(paragraph, &inherited);
                    if automatic {
                        self.body.push_str("</span>");
                    }
                }
                self.body.push_str("</td>");
                column += span;
            }
            self.body.push_str("</tr>");
        }
        self.body.push_str("</table>\n");
    }
}
fn running_text(p: &docx_rs::Paragraph, out: &mut String) {
    for child in &p.children {
        if let docx_rs::ParagraphChild::Run(run) = child {
            for child in &run.children {
                if let docx_rs::RunChild::Text(t) = child {
                    out.push_str(&t.text);
                }
            }
        }
    }
    out.push(' ');
}
fn hex_color(value: &str) -> bool {
    value.len() == 6 && value.bytes().all(|v| v.is_ascii_hexdigit())
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
