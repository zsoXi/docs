//! Surgical text editing: retain original XML markup and untouched ZIP records.

use crate::{
    Error,
    content::splice,
    docxxml::{self, crc32, u16_at, u32_at},
    messages::{MessageCode, Params},
};
use std::ops::Range;

fn invalid() -> Error {
    Error::invalid(MessageCode::DocxPackageInvalid, Params::new())
}

/// Rebuild only the edited entry; other local records (including compressed
/// payloads and descriptors) and central metadata are copied without decoding.
pub(crate) fn replace_entry(bytes: &[u8], name: &str, data: &[u8]) -> Result<Vec<u8>, Error> {
    let end = bytes
        .windows(4)
        .rposition(|v| v == b"PK\x05\x06")
        .ok_or_else(invalid)?;
    let central = u32_at(bytes, end + 16)?;
    let count = u16_at(bytes, end + 10)?;
    if count == 0xffff
        || u16_at(bytes, end + 4)? != 0
        || u16_at(bytes, end + 6)? != 0
        || u16_at(bytes, end + 8)? != count
    {
        return Err(invalid());
    }
    let mut at = central;
    let mut records = Vec::new();
    for _ in 0..count {
        if bytes.get(at..at + 4) != Some(b"PK\x01\x02") {
            return Err(invalid());
        }
        let len = 46 + u16_at(bytes, at + 28)? + u16_at(bytes, at + 30)? + u16_at(bytes, at + 32)?;
        let record = bytes.get(at..at + len).ok_or_else(invalid)?.to_vec();
        let local = u32_at(&record, 42)?;
        if local >= central || u32_at(&record, 24)? == 0xffffffff {
            return Err(invalid());
        }
        records.push((local, record));
        at += len;
    }
    if at != end || at - central != u32_at(bytes, end + 12)? {
        return Err(invalid());
    }
    let mut order: Vec<_> = (0..records.len()).collect();
    order.sort_by_key(|i| records[*i].0);
    let mut out = Vec::new();
    let mut changed = false;
    for (index, &i) in order.iter().enumerate() {
        let local = records[i].0;
        let next = order
            .get(index + 1)
            .map(|i| records[*i].0)
            .unwrap_or(central);
        let mut record = records[i].1.clone();
        let filename = record
            .get(46..46 + u16_at(&record, 28)?)
            .ok_or_else(invalid)?;
        let offset = u32::try_from(out.len()).map_err(|_| invalid())?;
        if bytes.get(local..local + 4) != Some(b"PK\x03\x04") || next <= local {
            return Err(invalid());
        }
        if filename == name.as_bytes() {
            if changed {
                return Err(invalid());
            }
            changed = true;
            let header_end = local + 30 + u16_at(bytes, local + 26)? + u16_at(bytes, local + 28)?;
            let mut header = bytes.get(local..header_end).ok_or_else(invalid)?.to_vec();
            let flags = u16_at(&record, 8)? as u16 & !8;
            let size = u32::try_from(data.len()).map_err(|_| invalid())?;
            let crc = crc32(data);
            header[6..8].copy_from_slice(&flags.to_le_bytes());
            header[8..10].copy_from_slice(&0u16.to_le_bytes());
            header[14..18].copy_from_slice(&crc.to_le_bytes());
            header[18..22].copy_from_slice(&size.to_le_bytes());
            header[22..26].copy_from_slice(&size.to_le_bytes());
            record[8..10].copy_from_slice(&flags.to_le_bytes());
            record[10..12].copy_from_slice(&0u16.to_le_bytes());
            record[16..20].copy_from_slice(&crc.to_le_bytes());
            record[20..24].copy_from_slice(&size.to_le_bytes());
            record[24..28].copy_from_slice(&size.to_le_bytes());
            out.extend(header);
            out.extend_from_slice(data);
        } else {
            out.extend_from_slice(bytes.get(local..next).ok_or_else(invalid)?);
        }
        record[42..46].copy_from_slice(&offset.to_le_bytes());
        records[i].1 = record;
    }
    if !changed {
        return Err(invalid());
    }
    let start = u32::try_from(out.len()).map_err(|_| invalid())?;
    for (_, record) in records {
        out.extend(record);
    }
    let size = u32::try_from(out.len()).map_err(|_| invalid())? - start;
    let mut tail = bytes
        .get(end..end + 22 + u16_at(bytes, end + 20)?)
        .ok_or_else(invalid)?
        .to_vec();
    tail[12..16].copy_from_slice(&size.to_le_bytes());
    tail[16..20].copy_from_slice(&start.to_le_bytes());
    out.extend(tail);
    Ok(out)
}

struct Text {
    span: Range<usize>,
    opening: String,
    closing: String,
    value: String,
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn edit_group(
    nodes: &mut Vec<Text>,
    find: &str,
    replacement: &str,
    all: bool,
    count: &mut usize,
    present: &mut usize,
    edits: &mut Vec<(Range<usize>, Vec<u8>)>,
) -> Result<(), Error> {
    let text: String = nodes.iter().map(|n| n.value.as_str()).collect();
    let matches: Vec<_> = text
        .match_indices(find)
        .map(|(at, _)| at..at + find.len())
        .collect();
    *present += matches.len();
    let chosen: Vec<_> = matches
        .into_iter()
        .take(if all {
            usize::MAX
        } else if *count == 0 {
            1
        } else {
            0
        })
        .collect();
    *count += chosen.len();
    let mut offset = 0;
    for node in nodes.drain(..) {
        let end = offset + node.value.len();
        let mut changes = Vec::new();
        for range in &chosen {
            if range.start < end && range.end > offset {
                changes.push((
                    range.start.max(offset) - offset..range.end.min(end) - offset,
                    if range.start >= offset {
                        replacement.as_bytes().to_vec()
                    } else {
                        Vec::new()
                    },
                ));
            }
        }
        if !changes.is_empty() {
            let value = String::from_utf8(splice(node.value.as_bytes(), &changes))
                .map_err(|e| Error::Docx(Box::new(e)))?;
            let opening = if node.opening.contains("xml:space=") {
                node.opening
            } else {
                format!(
                    "{} xml:space=\"preserve\">",
                    node.opening.trim_end_matches('>')
                )
            };
            edits.push((
                node.span,
                format!(
                    "{opening}{}{closing}",
                    escape(&value),
                    closing = node.closing
                )
                .into_bytes(),
            ));
        }
        offset = end;
    }
    Ok(())
}

/// Edit text across adjacent runs, but never across paragraph, tab or break
/// boundaries. XML outside affected text elements stays byte-for-byte intact.
pub(crate) fn replace(
    bytes: &[u8],
    find: &str,
    replacement: &str,
    all: bool,
) -> Result<(Vec<u8>, usize, usize), Error> {
    let xml = docxxml::entry(bytes, "word/document.xml")?.ok_or_else(invalid)?;
    docxxml::parse(&xml)?;
    let source = std::str::from_utf8(&xml).map_err(|e| Error::Docx(Box::new(e)))?;
    let mut at = 0;
    let mut prefix = None;
    let mut nodes = Vec::new();
    let mut edits = Vec::new();
    let (mut count, mut present) = (0, 0);
    while let Some(relative) = source[at..].find('<') {
        let start = at + relative;
        if source[start..].starts_with("<!--") {
            at = start + source[start..].find("-->").ok_or_else(invalid)? + 3;
            continue;
        }
        if source[start..].starts_with("<!") {
            return Err(invalid());
        }
        let mut quote = None;
        let mut end = start + 1;
        for (i, ch) in source[start + 1..].char_indices() {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                } else if ch == '>' {
                    end = start + 1 + i + 1;
                    break;
                }
            }
        }
        if end <= start + 1 {
            return Err(invalid());
        }
        let tag = &source[start..end];
        let name = tag[1..]
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/' || c == '>')
            .next()
            .ok_or_else(invalid)?;
        if prefix.is_none() && name.rsplit(':').next() == Some("document") {
            prefix = Some(
                name.strip_suffix("document")
                    .ok_or_else(invalid)?
                    .to_string(),
            );
        }
        let word = prefix.as_deref().unwrap_or("w:");
        if name == format!("{word}p")
            || ["tab", "br", "cr"]
                .iter()
                .any(|n| name == format!("{word}{n}"))
        {
            edit_group(
                &mut nodes,
                find,
                replacement,
                all,
                &mut count,
                &mut present,
                &mut edits,
            )?;
        }
        if name == format!("{word}t") && !tag.starts_with("</") && !tag.ends_with("/>") {
            let closing = format!("</{name}>");
            let close = end + source[end..].find(&closing).ok_or_else(invalid)?;
            let inner = &source[end..close];
            let value = docxxml::parse(format!("<t>{inner}</t>").as_bytes())?.text;
            nodes.push(Text {
                span: start..close + closing.len(),
                opening: tag.into(),
                closing,
                value,
            });
            at = close + format!("</{name}>").len();
        } else {
            at = end;
        }
    }
    edit_group(
        &mut nodes,
        find,
        replacement,
        all,
        &mut count,
        &mut present,
        &mut edits,
    )?;
    if count == 0 {
        return Ok((bytes.to_vec(), count, present));
    }
    let edited = splice(&xml, &edits);
    docxxml::parse(&edited)?;
    Ok((
        replace_entry(bytes, "word/document.xml", &edited)?,
        count,
        present,
    ))
}
