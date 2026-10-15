//! HTML5 parsing with non-content elements removed and consistent Markdown tables.

use crate::Error;
use html2md::{Handle, NodeData, StructuredPrinter, TagHandler, TagHandlerFactory};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

pub(crate) fn markdown(source: &str) -> Result<String, Error> {
    let excessive = Rc::new(Cell::new(false));
    let flag = excessive.clone();
    let mut handlers = handlers();
    handlers.insert("html".into(), Box::new(move || Guard(flag.clone())));
    let text = html2md::parse_html_custom(source, &handlers);
    if excessive.get() {
        return Err(super::invalid());
    }
    Ok(text)
}
fn handlers() -> HashMap<String, Box<dyn TagHandlerFactory>> {
    let mut out: HashMap<String, Box<dyn TagHandlerFactory>> = HashMap::new();
    for tag in ["head", "script", "style", "template"] {
        out.insert(tag.into(), Box::new(|| Ignore));
    }
    out.insert("table".into(), Box::new(|| Table));
    for level in 1..=6 {
        out.insert(format!("h{level}"), Box::new(move || Heading(level)));
    }
    out.insert(
        "a".into(),
        Box::new(|| Reference {
            fallback: html2md::anchors::AnchorHandler::default(),
            skip: false,
        }),
    );
    for tag in ["aside", "section", "li", "p"] {
        out.insert(
            tag.into(),
            Box::new(move || Footnote {
                fallback: match tag {
                    "li" => Box::new(html2md::lists::ListItemHandler::default()),
                    "p" => Box::new(html2md::paragraphs::ParagraphHandler::default()),
                    _ => Box::new(html2md::containers::ContainerHandler),
                },
                skip: false,
                start: 0,
            }),
        );
    }
    out.insert(
        "figcaption".into(),
        Box::new(html2md::containers::ContainerHandler::default),
    );
    out
}
fn attr(node: &Handle, key: &str) -> String {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| a.name.local.as_ref() == key)
            .map(|a| a.value.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}
fn note_id(id: &str) -> String {
    id.replace(['[', ']', '\r', '\n', ' '], "_")
}
struct Reference {
    fallback: html2md::anchors::AnchorHandler,
    skip: bool,
}
impl TagHandler for Reference {
    fn handle(&mut self, node: &Handle, printer: &mut StructuredPrinter) {
        let href = attr(node, "href");
        if attr(node, "role") == "doc-backlink" {
            self.skip = true;
        } else if (attr(node, "role") == "doc-noteref" || attr(node, "type") == "noteref")
            && let Some(id) = href.strip_prefix('#')
        {
            printer.append_str(&format!("[^{}]", note_id(id)));
            self.skip = true;
        } else {
            self.fallback.handle(node, printer);
        }
    }
    fn after_handle(&mut self, printer: &mut StructuredPrinter) {
        if !self.skip {
            self.fallback.after_handle(printer);
        }
    }
    fn skip_descendants(&self) -> bool {
        self.skip
    }
}
struct Footnote {
    fallback: Box<dyn TagHandler>,
    skip: bool,
    start: usize,
}
impl TagHandler for Footnote {
    fn handle(&mut self, node: &Handle, printer: &mut StructuredPrinter) {
        let id = attr(node, "id");
        if !id.is_empty()
            && (matches!(attr(node, "role").as_str(), "doc-footnote" | "doc-endnote")
                || matches!(attr(node, "type").as_str(), "footnote" | "endnote"))
        {
            let mut body = StructuredPrinter::default();
            for child in node.children.borrow().iter() {
                html2md::walk(child, &mut body, &handlers());
            }
            printer.append_str(&format!(
                "\n\n[^{}]: {}\n\n",
                note_id(&id),
                body.data.trim().replace('\n', "\n    ")
            ));
            self.skip = true;
        } else {
            self.fallback.handle(node, printer);
            self.start = printer.data.len();
        }
    }
    fn after_handle(&mut self, printer: &mut StructuredPrinter) {
        if !self.skip {
            // html2md escapes single-character block markers, but not ordered
            // list delimiters. Escape the delimiter, not the visible digits.
            let text = &printer.data[self.start..];
            let digits = text.bytes().take_while(u8::is_ascii_digit).count();
            if (1..=9).contains(&digits)
                && matches!(text.as_bytes().get(digits), Some(b'.' | b')'))
                && text.as_bytes().get(digits + 1).is_some_and(u8::is_ascii_whitespace)
            {
                printer.data.insert(self.start + digits, '\\');
            }
            self.fallback.after_handle(printer);
        }
    }
    fn skip_descendants(&self) -> bool {
        self.skip || self.fallback.skip_descendants()
    }
}

struct Heading(usize);
impl TagHandler for Heading {
    fn handle(&mut self, _: &Handle, printer: &mut StructuredPrinter) {
        printer.append_str(&format!("\n\n{} ", "#".repeat(self.0)));
    }
    fn after_handle(&mut self, printer: &mut StructuredPrinter) {
        printer.append_str("\n\n");
    }
}

struct Ignore;
impl TagHandler for Ignore {
    fn handle(&mut self, _: &Handle, _: &mut StructuredPrinter) {}
    fn after_handle(&mut self, _: &mut StructuredPrinter) {}
    fn skip_descendants(&self) -> bool {
        true
    }
}
struct Guard(Rc<Cell<bool>>);
impl TagHandler for Guard {
    fn handle(&mut self, root: &Handle, _: &mut StructuredPrinter) {
        let mut queue = vec![(root.clone(), 0)];
        while let Some((node, depth)) = queue.pop() {
            if depth > 128 {
                self.0.set(true);
                node.children.borrow_mut().clear();
            } else {
                queue.extend(
                    node.children
                        .borrow()
                        .iter()
                        .map(|n| (n.clone(), depth + 1)),
                );
            }
        }
    }
    fn after_handle(&mut self, _: &mut StructuredPrinter) {}
}
struct Table;
impl TagHandler for Table {
    fn handle(&mut self, root: &Handle, printer: &mut StructuredPrinter) {
        let mut queue = vec![root.clone()];
        let mut rows = Vec::new();
        while let Some(node) = queue.pop() {
            if tag(&node) == "tr" {
                let cells = node
                    .children
                    .borrow()
                    .iter()
                    .filter(|n| matches!(tag(n), "td" | "th"))
                    .map(|n| {
                        let mut cell = StructuredPrinter::default();
                        html2md::walk(n, &mut cell, &handlers());
                        cell.data.trim().to_string()
                    })
                    .collect();
                rows.push(cells);
            } else {
                queue.extend(node.children.borrow().iter().rev().cloned());
            }
        }
        printer.append_str(&format!("\n\n{}\n\n", super::table(&rows)));
    }
    fn after_handle(&mut self, _: &mut StructuredPrinter) {}
    fn skip_descendants(&self) -> bool {
        true
    }
}
fn tag(node: &Handle) -> &str {
    match &node.data {
        NodeData::Element { name, .. } => name.local.as_ref(),
        _ => "",
    }
}
