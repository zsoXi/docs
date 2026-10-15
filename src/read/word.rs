//! DOCX paragraphs, lists, tables, hyperlinks and notes from OOXML.

use super::{
    Document, escape, invalid, selection, table,
    xml::{self, Node, Rel},
};
use crate::messages::{Msg, params};
use crate::{Error, MessageCode};
use serde_json::json;
use std::collections::BTreeMap;

pub(super) fn read(bytes: &[u8], spec: Option<&str>) -> Result<Document, Error> {
    let root = xml::part(bytes, "word/document.xml")?;
    let body = root.find("body").ok_or_else(invalid)?;
    let rels = xml::rels(bytes, "word/document.xml")?;
    let styles = xml::entry(bytes, "word/styles.xml")?
        .map(|b| xml::parse(&b))
        .transpose()?;
    let numbers = xml::entry(bytes, "word/numbering.xml")?
        .map(|b| xml::parse(&b))
        .transpose()?;
    let context = Context {
        rels: &rels,
        styles: styles.as_ref(),
        numbers: numbers.as_ref(),
        counters: std::cell::RefCell::new(BTreeMap::new()),
    };
    let mut blocks = Vec::new();
    collect_blocks(body, &mut blocks);
    let wanted = selection(spec, blocks.len())?;
    let mut doc = Document::default();
    if rels.values().any(|r| {
        matches!(
            r.kind.as_str(),
            "header" | "footer" | "chart" | "diagramData"
        )
    }) || root.find("oMath").is_some()
    {
        doc.warnings.push(Msg::with(
            MessageCode::ReadFlattened,
            params(&[("feature", "document_objects".into())]),
        ));
    }
    let mut paragraph = 0;
    for (index, node) in blocks.iter().enumerate() {
        let start = paragraph + 1;
        let mut paras = Vec::new();
        node.descendants("p", &mut paras);
        paragraph += paras.len();
        let text = if node.name == "tbl" {
            let rows = node
                .all("tr")
                .map(|row| {
                    row.all("tc")
                        .map(|cell| {
                            cell.all("p")
                                .map(|p| context.paragraph(p))
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .collect()
                })
                .collect::<Vec<_>>();
            if node.find("gridSpan").is_some() || node.find("vMerge").is_some() {
                doc.warnings.push(Msg::with(
                    MessageCode::ReadFlattened,
                    params(&[("feature", "merged_cells".into())]),
                ));
            }
            table(&rows)
        } else {
            context.paragraph(node)
        };
        if wanted.contains(&(index + 1)) {
            doc.push(text, json!({"kind":"paragraph", "paragraph":start, "paragraph_end":paragraph, "block":index+1}));
        }
    }
    for (file, tag, prefix) in [
        ("word/footnotes.xml", "footnote", "fn"),
        ("word/endnotes.xml", "endnote", "en"),
    ] {
        if let Some(data) = xml::entry(bytes, file)? {
            let notes = xml::parse(&data)?;
            for note in notes.all(tag).filter(|n| n.attr("type").is_empty()) {
                let id = note.attr("id");
                let marker = format!("[^{prefix}{id}]");
                if doc.chunks.iter().any(|c| c.text.contains(&marker)) {
                    let text = note
                        .all("p")
                        .map(|p| context.inline(p))
                        .collect::<Vec<_>>()
                        .join(" ");
                    doc.push(
                        format!("{marker}: {}", text.trim()),
                        json!({"kind":tag,"id":id}),
                    );
                }
            }
        }
    }
    Ok(doc)
}

fn collect_blocks<'a>(node: &'a Node, out: &mut Vec<&'a Node>) {
    for child in &node.children {
        if matches!(child.name.as_str(), "p" | "tbl") {
            out.push(child);
        } else {
            collect_blocks(child, out);
        }
    }
}

struct Context<'a> {
    rels: &'a BTreeMap<String, Rel>,
    styles: Option<&'a Node>,
    numbers: Option<&'a Node>,
    counters: std::cell::RefCell<BTreeMap<(String, usize), u64>>,
}

impl Context<'_> {
    fn heading(&self, props: Option<&Node>) -> Option<usize> {
        if let Some(value) = props.map(|p| p.val("outlineLvl")).filter(|v| !v.is_empty()) {
            return value
                .parse::<usize>()
                .ok()
                .filter(|v| *v < 6)
                .map(|v| v + 1);
        }
        let mut id = props.map(|p| p.val("pStyle")).unwrap_or("");
        for _ in 0..32 {
            let lowered = id.to_ascii_lowercase();
            if lowered == "title" {
                return Some(1);
            }
            if let Some(level) = lowered
                .strip_prefix("heading")
                .and_then(|n| n.trim().parse::<usize>().ok())
                .filter(|v| (1..=6).contains(v))
            {
                return Some(level);
            }
            let style = self
                .styles
                .and_then(|s| s.all("style").find(|s| s.attr("styleId") == id))?;
            if let Some(level) = style
                .child("pPr")
                .map(|p| p.val("outlineLvl"))
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v < 6)
            {
                return Some(level + 1);
            }
            id = style.val("basedOn");
        }
        None
    }

    fn paragraph(&self, p: &Node) -> String {
        let text = self.inline(p);
        let props = p.child("pPr");
        let mut runs = Vec::new();
        p.descendants("r", &mut runs);
        let text_runs: Vec<_> = runs
            .into_iter()
            .filter(|r| r.child("t").is_some_and(|t| !t.value().trim().is_empty()))
            .collect();
        let smallest = text_runs
            .iter()
            .map(|r| {
                r.child("rPr")
                    .map(|p| p.val("sz"))
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(22)
            })
            .min()
            .unwrap_or(22);
        let inferred = (smallest >= 28).then_some(if smallest >= 40 {
            1
        } else if smallest >= 32 {
            2
        } else {
            3
        });
        if let Some(level) = self.heading(props).or(inferred) {
            let text = text.trim();
            let text = text
                .strip_prefix("**")
                .and_then(|s| s.strip_suffix("**"))
                .filter(|s| !s.contains("**"))
                .unwrap_or(text);
            return format!("{} {}", "#".repeat(level), text);
        }
        if let Some(num) = props.and_then(|n| n.child("numPr")) {
            let level = num.val("ilvl").parse::<usize>().unwrap_or(0).min(32);
            let id = num.val("numId");
            let definition = self.numbers
                .and_then(|n| n.all("num").find(|n| n.attr("numId") == id));
            let abstract_id = definition.map(|n| n.val("abstractNumId"));
            let base = self.numbers.and_then(|n| {
                n.all("abstractNum").find(|n| Some(n.attr("abstractNumId")) == abstract_id)
            }).and_then(|n| n.all("lvl").find(|n| n.attr("ilvl") == num.val("ilvl")));
            let override_level = definition.and_then(|n| n.all("lvlOverride")
                .find(|n| n.attr("ilvl") == num.val("ilvl")));
            let effective = override_level.and_then(|n| n.child("lvl")).or(base);
            let fmt = effective.map(|n| n.val("numFmt")).unwrap_or("bullet");
            let start = override_level.map(|n| n.val("startOverride"))
                .filter(|s| !s.is_empty())
                .or_else(|| effective.map(|n| n.val("start")))
                .and_then(|s| s.parse::<u64>().ok()).unwrap_or(1);
            let mut counters = self.counters.borrow_mut();
            counters.retain(|(list, depth), _| list != id || *depth <= level);
            let number = counters.entry((id.into(), level)).or_insert(start);
            let marker = if fmt == "bullet" { "-".into() } else { format!("{number}.") };
            *number = number.saturating_add(1);
            return format!("{}{} {}", "    ".repeat(level), marker, text.trim());
        }
        text.trim().to_string()
    }

    fn run_active(&self, props: Option<&Node>, name: &str) -> bool {
        let enabled = |n: &Node| !matches!(n.attr("val"), "false" | "0" | "off");
        if let Some(value) = props.and_then(|p|p.child(name)) { return enabled(value); }
        let mut id = props.map(|p|p.val("rStyle")).unwrap_or("");
        for _ in 0..32 {
            let Some(style) = self.styles.and_then(|s|s.all("style").find(|s|s.attr("styleId")==id)) else { break };
            if let Some(value)=style.child("rPr").and_then(|p|p.child(name)) { return enabled(value); }
            id=style.val("basedOn");
        }
        self.styles.and_then(|s|s.child("docDefaults")).and_then(|s|s.child("rPrDefault"))
            .and_then(|s|s.child("rPr")).and_then(|s|s.child(name)).is_some_and(enabled)
    }

    fn inline(&self, node: &Node) -> String {
        match node.name.as_str() {
            "t" => return escape(&node.value()),
            "tab" => return "\t".into(),
            "br" | "cr" => return "\n".into(),
            "footnoteReference" => return format!("[^fn{}]", node.attr("id")),
            "endnoteReference" => return format!("[^en{}]", node.attr("id")),
            "docPr" => {
                let caption = if node.attr("descr").is_empty() {
                    node.attr("title")
                } else {
                    node.attr("descr")
                };
                return escape(caption);
            }
            "pPr" | "rPr" | "instrText" | "del" | "sectPr" => return String::new(),
            _ => {}
        }
        let mut text: String = node.children.iter().map(|n| self.inline(n)).collect();
        if node.name == "hyperlink" {
            let target = self
                .rels
                .get(node.attr("id"))
                .map(|r| r.target.clone())
                .or_else(|| {
                    (!node.attr("anchor").is_empty()).then(|| format!("#{}", node.attr("anchor")))
                });
            if let Some(target) = target {
                text = format!("[{text}]({})", destination(&target));
            }
        }
        if node.name == "r" {
            let active = |name| self.run_active(node.child("rPr"), name);
            let marker = match (active("b"), active("i")) {
                (true, true) => "***",
                (true, false) => "**",
                (false, true) => "*",
                _ => "",
            };
            if !marker.is_empty() && !text.trim().is_empty() {
                let start = text.len() - text.trim_start().len();
                let end = text.trim_end().len();
                text = format!(
                    "{}{marker}{}{marker}{}",
                    &text[..start],
                    &text[start..end],
                    &text[end..]
                );
            }
        }
        text
    }
}

pub(super) fn destination(target: &str) -> String {
    target
        .replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace(['\r', '\n'], "")
}
