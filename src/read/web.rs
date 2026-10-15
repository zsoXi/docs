//! EPUB spine order, MIME archives and both XMind package generations.

use super::{
    Document, escape, invalid, selection,
    word::destination,
    xml::{self, Node},
};
use crate::messages::Msg;
use crate::{Error, MessageCode};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn epub(bytes: &[u8], spec: Option<&str>) -> Result<Document, Error> {
    let container = xml::part(bytes, "META-INF/container.xml")?;
    let package_name = container
        .find("rootfile")
        .map(|n| n.attr("full-path"))
        .ok_or_else(invalid)?;
    let package_name = xml::target("", package_name)?;
    let package = xml::part(bytes, &package_name)?;
    let manifest: BTreeMap<_, _> = package
        .child("manifest")
        .ok_or_else(invalid)?
        .all("item")
        .map(|n| (n.attr("id"), n))
        .collect();
    let spine = package.child("spine").ok_or_else(invalid)?;
    let chapters: Vec<_> = spine.all("itemref").collect();
    let mut doc = Document::default();
    let toc = if let Some(nav) = manifest
        .values()
        .find(|n| n.attr("properties").split_whitespace().any(|p| p == "nav"))
    {
        let name = xml::target(&package_name, nav.attr("href"))?;
        let data = xml::required(bytes, &name)?;
        super::html::markdown(
            std::str::from_utf8(&data).map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?,
        )?
    } else if let Some(ncx) = manifest.get(spine.attr("toc")) {
        let name = xml::target(&package_name, ncx.attr("href"))?;
        let root = xml::part(bytes, &name)?;
        let mut out = String::new();
        if let Some(map) = root.child("navMap") {
            ncx_toc(map, 0, &mut out);
        }
        out
    } else {
        let mut out = String::new();
        for item in &chapters {
            let entry = manifest.get(item.attr("idref")).ok_or_else(invalid)?;
            out.push_str(&format!(
                "- [{}]({})\n",
                escape(entry.attr("href")),
                destination(entry.attr("href"))
            ));
        }
        out
    };
    if !toc.trim().is_empty() {
        doc.push(
            format!(
                "# {}\n\n{}",
                Msg::new(MessageCode::ReadContents).render("en"),
                toc.trim()
            ),
            json!({"kind":"contents", "part":package_name}),
        );
    }
    for index in selection(spec, chapters.len())? {
        let entry = manifest
            .get(chapters[index - 1].attr("idref"))
            .ok_or_else(invalid)?;
        let name = xml::target(&package_name, entry.attr("href"))?;
        let data = xml::required(bytes, &name)?;
        doc.push(
            super::html::markdown(
                std::str::from_utf8(&data)
                    .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?,
            )?,
            json!({"kind":"chapter","chapter":index,"href":name}),
        );
    }
    Ok(doc)
}
fn ncx_toc(node: &Node, depth: usize, out: &mut String) {
    for point in node.all("navPoint") {
        let name = point.child("navLabel").map(Node::value).unwrap_or_default();
        let href = point.child("content").map(|n| n.attr("src")).unwrap_or("");
        out.push_str(&format!(
            "{}- [{}]({})\n",
            "  ".repeat(depth),
            escape(name.trim()),
            destination(href)
        ));
        ncx_toc(point, depth + 1, out);
    }
}

pub(super) fn xmind(bytes: &[u8], spec: Option<&str>) -> Result<Document, Error> {
    let mut doc = Document::default();
    if let Some(data) = xml::entry(bytes, "content.json")? {
        let value: Value = serde_json::from_slice(&data)
            .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?;
        let sheets = value.as_array().ok_or_else(invalid)?;
        for index in selection(spec, sheets.len())? {
            let sheet = &sheets[index - 1];
            let mut text = format!(
                "# {}\n\n",
                escape(sheet.get("title").and_then(Value::as_str).unwrap_or(""))
            );
            json_topic(sheet.get("rootTopic").ok_or_else(invalid)?, 0, &mut text)?;
            doc.push(
                text.trim_end().to_string(),
                json!({"kind":"sheet","sheet":index}),
            );
        }
    } else {
        let root = xml::part(bytes, "content.xml")?;
        let sheets: Vec<_> = root.all("sheet").collect();
        for index in selection(spec, sheets.len())? {
            let sheet = sheets[index - 1];
            let mut text = format!(
                "# {}\n\n",
                escape(&sheet.child("title").map(Node::value).unwrap_or_default())
            );
            xml_topic(sheet.child("topic").ok_or_else(invalid)?, 0, &mut text);
            doc.push(
                text.trim_end().to_string(),
                json!({"kind":"sheet","sheet":index}),
            );
        }
    }
    Ok(doc)
}
fn json_topic(node: &Value, depth: usize, out: &mut String) -> Result<(), Error> {
    if depth > 64 || !node.is_object() {
        return Err(invalid());
    }
    let title = node
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    out.push_str(&format!("{}- {}\n", "  ".repeat(depth), escape(title)));
    if let Some(note) = node.pointer("/notes/plain/content").and_then(Value::as_str) {
        for line in note.lines() {
            out.push_str(&format!("{}> {}\n", "  ".repeat(depth + 1), escape(line)));
        }
    }
    if let Some(groups) = node.get("children").and_then(Value::as_object) {
        for key in ["attached", "detached"] {
            if let Some(children) = groups.get(key) {
                for child in children.as_array().ok_or_else(invalid)? {
                    json_topic(child, depth + 1, out)?;
                }
            }
        }
    }
    Ok(())
}
fn xml_topic(node: &Node, depth: usize, out: &mut String) {
    out.push_str(&format!(
        "{}- {}\n",
        "  ".repeat(depth),
        escape(&node.child("title").map(Node::value).unwrap_or_default())
    ));
    if let Some(note) = node.child("notes").and_then(|n| n.child("plain")) {
        for line in note.value().lines() {
            out.push_str(&format!("{}> {}\n", "  ".repeat(depth + 1), escape(line)));
        }
    }
    if let Some(children) = node.child("children") {
        for group in children.all("topics") {
            for topic in group.all("topic") {
                xml_topic(topic, depth + 1, out);
            }
        }
    }
}

pub(super) fn mhtml(bytes: &[u8], spec: Option<&str>) -> Result<Document, Error> {
    let mut parts = Vec::new();
    mime(bytes, 0, &mut parts)?;
    if parts.is_empty() {
        return Err(invalid());
    }
    let mut doc = Document::default();
    for index in selection(spec, parts.len())? {
        let (html, location) = &parts[index - 1];
        doc.push(
            super::html::markdown(html)?,
            json!({"kind":"part","part":index,"location":location}),
        );
    }
    Ok(doc)
}
fn mime(bytes: &[u8], depth: usize, parts: &mut Vec<(String, String)>) -> Result<(), Error> {
    if depth > 16 {
        return Err(invalid());
    }
    let (at, sep) = bytes
        .windows(4)
        .position(|b| b == b"\r\n\r\n")
        .map(|i| (i, 4))
        .or_else(|| bytes.windows(2).position(|b| b == b"\n\n").map(|i| (i, 2)))
        .ok_or_else(invalid)?;
    let header = String::from_utf8_lossy(&bytes[..at]).replace("\r\n", "\n");
    let mut headers: BTreeMap<String, String> = BTreeMap::new();
    let mut last = String::new();
    for line in header.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(value) = headers.get_mut(&last) {
                value.push(' ');
                value.push_str(line.trim());
            }
        } else {
            let (name, value) = line.split_once(':').ok_or_else(invalid)?;
            last = name.to_ascii_lowercase();
            headers.insert(last.clone(), value.trim().into());
        }
    }
    let content_type = headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or("text/plain");
    let (kind, attrs) = mime_type(content_type)?;
    let body = &bytes[at + sep..];
    if kind.starts_with("multipart/") {
        let boundary = attrs.get("boundary").ok_or_else(invalid)?;
        if boundary.is_empty() || boundary.contains(['\r', '\n']) {
            return Err(invalid());
        }
        let delimiter = format!("--{boundary}");
        let end = format!("--{boundary}--");
        let mut begin = None;
        let mut offset = 0;
        let mut closed = false;
        for line in body.split_inclusive(|b| *b == b'\n') {
            let trimmed = line.trim_ascii_end();
            if trimmed == delimiter.as_bytes() || trimmed == end.as_bytes() {
                if let Some(start) = begin {
                    mime(&body[start..offset], depth + 1, parts)?;
                }
                if trimmed == end.as_bytes() {
                    closed = true;
                    break;
                }
                begin = Some(offset + line.len());
            }
            offset += line.len();
        }
        if !closed {
            return Err(invalid());
        }
    } else if kind == "text/html" || kind == "application/xhtml+xml" {
        let encoding = headers
            .get("content-transfer-encoding")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        let data = match encoding.as_str() {
            "base64" => base64(body)?,
            "quoted-printable" => quoted(body)?,
            "" | "7bit" | "8bit" | "binary" => body.to_vec(),
            _ => return Err(invalid()),
        };
        let charset = attrs
            .get("charset")
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_else(|| "utf-8".into());
        let text = match charset.as_str() {
            "utf-8" | "us-ascii" => {
                String::from_utf8(data).map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?
            }
            "iso-8859-1" | "latin1" => data.into_iter().map(char::from).collect(),
            "windows-1252" => data.into_iter().map(cp1252).collect(),
            _ => return Err(invalid()),
        };
        parts.push((
            text,
            headers.get("content-location").cloned().unwrap_or_default(),
        ));
    }
    Ok(())
}
fn mime_type(value: &str) -> Result<(String, BTreeMap<String, String>), Error> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch == ';' && !quoted {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    if quoted || escaped {
        return Err(invalid());
    }
    fields.push(current);
    let mut fields = fields.into_iter();
    let kind = fields
        .next()
        .ok_or_else(invalid)?
        .trim()
        .to_ascii_lowercase();
    let mut attrs = BTreeMap::new();
    for field in fields {
        let (key, value) = field.split_once('=').ok_or_else(invalid)?;
        attrs.insert(key.trim().to_ascii_lowercase(), value.trim().into());
    }
    Ok((kind, attrs))
}
fn base64(data: &[u8]) -> Result<Vec<u8>, Error> {
    let data: Vec<_> = data
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if data.len() % 4 != 0 {
        return Err(invalid());
    }
    let mut out = Vec::new();
    for (index, chunk) in data.as_chunks::<4>().0.iter().enumerate() {
        let decode = |b| match b {
            b'A'..=b'Z' => Ok(b - b'A'),
            b'a'..=b'z' => Ok(b - b'a' + 26),
            b'0'..=b'9' => Ok(b - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(invalid()),
        };
        let a = decode(chunk[0])?;
        let b = decode(chunk[1])?;
        out.push((a << 2) | (b >> 4));
        if chunk[2] == b'=' {
            if chunk[3] != b'=' || index + 1 != data.len() / 4 {
                return Err(invalid());
            }
        } else {
            let c = decode(chunk[2])?;
            out.push((b << 4) | (c >> 2));
            if chunk[3] == b'=' {
                if index + 1 != data.len() / 4 {
                    return Err(invalid());
                }
            } else {
                out.push((c << 6) | decode(chunk[3])?);
            }
        }
    }
    Ok(out)
}
fn quoted(data: &[u8]) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        if data[at] == b'=' {
            if data.get(at + 1) == Some(&b'\n') {
                at += 2;
                continue;
            }
            if data.get(at + 1..at + 3) == Some(b"\r\n") {
                at += 3;
                continue;
            }
            let pair = data.get(at + 1..at + 3).ok_or_else(invalid)?;
            let hex = std::str::from_utf8(pair).map_err(|_| invalid())?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| invalid())?);
            at += 3;
        } else {
            out.push(data[at]);
            at += 1;
        }
    }
    Ok(out)
}
fn cp1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    if (0x80..0xa0).contains(&byte) {
        HIGH[(byte - 0x80) as usize]
    } else {
        char::from(byte)
    }
}
