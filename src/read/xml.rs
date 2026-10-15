//! Mixed-content XML in document order, using the existing XML dependency.

use super::invalid;
use crate::{Error, MessageCode};
use docx_rs::{EventReader, XmlEvent};
use std::collections::BTreeMap;

#[derive(Default, Debug)]
pub(super) struct Node {
    pub name: String,
    pub attrs: BTreeMap<String, String>,
    pub children: Vec<Node>,
    pub text: String,
}

impl Node {
    pub fn attr(&self, name: &str) -> &str {
        let value = if name == "id" {
            self.attrs.get("r:id").or_else(|| self.attrs.get(name))
        } else {
            self.attrs.get(name)
        };
        value.map(String::as_str).unwrap_or("")
    }
    pub fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|n| n.name == name)
    }
    pub fn all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Self> {
        self.children.iter().filter(move |n| n.name == name)
    }
    pub fn find(&self, name: &str) -> Option<&Self> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|n| n.find(name))
    }
    pub fn descendants<'a>(&'a self, name: &str, out: &mut Vec<&'a Self>) {
        if self.name == name {
            out.push(self);
        }
        for child in &self.children {
            child.descendants(name, out);
        }
    }
    pub fn value(&self) -> String {
        let mut text = self.text.clone();
        for node in &self.children {
            text.push_str(&node.value());
        }
        text
    }
    pub fn val(&self, name: &str) -> &str {
        self.child(name).map(|n| n.attr("val")).unwrap_or("")
    }
}

pub(super) fn parse(bytes: &[u8]) -> Result<Node, Error> {
    let mut reader = EventReader::new(bytes);
    let mut stack = vec![Node::default()];
    loop {
        match reader
            .next_event()
            .map_err(|source| Error::reader(MessageCode::ReadInvalid, source))?
        {
            XmlEvent::StartElement {
                name, attributes, ..
            } => {
                if stack.len() > 128 {
                    return Err(invalid());
                }
                stack.push(Node {
                    name: name.local_name,
                    attrs: attributes
                        .into_iter()
                        .flat_map(|a| {
                            let mut pairs = Vec::new();
                            if let Some(prefix) = a.name.prefix {
                                pairs.push((
                                    format!("{prefix}:{}", a.name.local_name),
                                    a.value.clone(),
                                ));
                            }
                            pairs.push((a.name.local_name, a.value));
                            pairs
                        })
                        .collect(),
                    ..Node::default()
                });
            }
            XmlEvent::Characters(text) | XmlEvent::Whitespace(text) => {
                stack.last_mut().ok_or_else(invalid)?.children.push(Node {
                    text,
                    ..Node::default()
                });
            }
            XmlEvent::EndElement { .. } => {
                let node = stack.pop().ok_or_else(invalid)?;
                stack.last_mut().ok_or_else(invalid)?.children.push(node);
            }
            XmlEvent::EndDocument => break,
        }
    }
    if stack.len() != 1 {
        return Err(invalid());
    }
    let root = stack.pop().ok_or_else(invalid)?;
    let mut elements = root.children.into_iter().filter(|n| !n.name.is_empty());
    let element = elements.next().ok_or_else(invalid)?;
    if elements.next().is_some() {
        return Err(invalid());
    }
    Ok(element)
}

pub(super) fn entry(bytes: &[u8], name: &str) -> Result<Option<Vec<u8>>, Error> {
    crate::docxxml::entry(bytes, name)
        .map_err(|source| Error::reader(MessageCode::ReadInvalid, source))
}

pub(super) fn required(bytes: &[u8], name: &str) -> Result<Vec<u8>, Error> {
    entry(bytes, name)?.ok_or_else(invalid)
}

pub(super) fn part(bytes: &[u8], name: &str) -> Result<Node, Error> {
    parse(&required(bytes, name)?)
}

/// Resolve only within the archive namespace. Never turn relationships into filesystem reads.
pub(super) fn target(part: &str, reference: &str) -> Result<String, Error> {
    let raw = reference.split('#').next().unwrap_or("").as_bytes();
    let mut decoded = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let pair = raw.get(at + 1..at + 3).ok_or_else(invalid)?;
            let hex = std::str::from_utf8(pair)
                .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?;
            decoded.push(
                u8::from_str_radix(hex, 16)
                    .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?,
            );
            at += 3;
        } else {
            decoded.push(raw[at]);
            at += 1;
        }
    }
    let reference =
        std::str::from_utf8(&decoded).map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?;
    if reference.contains(['\\', ':', '\0']) {
        return Err(invalid());
    }
    let mut segments: Vec<&str> = if reference.starts_with('/') {
        Vec::new()
    } else {
        part.rsplit_once('/')
            .map(|(parent, _)| parent.split('/').collect())
            .unwrap_or_default()
    };
    for segment in reference.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop().ok_or_else(invalid)?;
            }
            other => segments.push(other),
        }
    }
    Ok(segments.join("/"))
}

pub(super) struct Rel {
    pub target: String,
    pub kind: String,
    pub external: bool,
}

pub(super) fn rels(bytes: &[u8], part_name: &str) -> Result<BTreeMap<String, Rel>, Error> {
    let (parent, file) = part_name.rsplit_once('/').unwrap_or(("", part_name));
    let name = format!("{parent}/_rels/{file}.rels");
    let Some(data) = entry(bytes, name.trim_start_matches('/'))? else {
        return Ok(BTreeMap::new());
    };
    let root = parse(&data)?;
    let mut out = BTreeMap::new();
    for node in root.all("Relationship") {
        let external = node.attr("TargetMode") == "External";
        let value = if external {
            node.attr("Target").to_string()
        } else {
            target(part_name, node.attr("Target"))?
        };
        out.insert(
            node.attr("Id").into(),
            Rel {
                target: value,
                kind: node.attr("Type").rsplit('/').next().unwrap_or("").into(),
                external,
            },
        );
    }
    Ok(out)
}
