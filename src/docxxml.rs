//! Bounded OOXML package and element reading, independent of the DOCX object model.

use crate::error::Error;
use crate::messages::{MessageCode, Params};
use docx_rs::{EventReader, XmlEvent};
use std::collections::BTreeMap;

fn invalid() -> Error {
    Error::invalid(MessageCode::DocxPackageInvalid, Params::new())
}

pub(super) fn u16_at(bytes: &[u8], at: usize) -> Result<usize, Error> {
    let raw: [u8; 2] = bytes
        .get(at..at + 2)
        .ok_or_else(invalid)?
        .try_into()
        .map_err(|_| invalid())?;
    Ok(u16::from_le_bytes(raw) as usize)
}
pub(super) fn u32_at(bytes: &[u8], at: usize) -> Result<usize, Error> {
    let raw: [u8; 4] = bytes
        .get(at..at + 4)
        .ok_or_else(invalid)?
        .try_into()
        .map_err(|_| invalid())?;
    Ok(u32::from_le_bytes(raw) as usize)
}

/// Read a single non-ZIP64 OOXML entry without extracting paths to disk.
pub(crate) fn entry(bytes: &[u8], name: &str) -> Result<Option<Vec<u8>>, Error> {
    let end = bytes
        .windows(4)
        .rposition(|v| v == b"PK\x05\x06")
        .ok_or_else(invalid)?;
    let mut at = u32_at(bytes, end + 16)?;
    for _ in 0..u16_at(bytes, end + 10)? {
        if bytes.get(at..at + 4) != Some(b"PK\x01\x02") {
            return Err(invalid());
        }
        let length = u16_at(bytes, at + 28)?;
        let filename = bytes.get(at + 46..at + 46 + length).ok_or_else(invalid)?;
        if filename == name.as_bytes() {
            if u16_at(bytes, at + 8)? & 1 != 0 {
                return Err(invalid());
            }
            let method = u16_at(bytes, at + 10)?;
            let compressed = u32_at(bytes, at + 20)?;
            let size = u32_at(bytes, at + 24)?;
            if size > 100 * 1024 * 1024 {
                return Err(invalid());
            }
            let local = u32_at(bytes, at + 42)?;
            if bytes.get(local..local + 4) != Some(b"PK\x03\x04") {
                return Err(invalid());
            }
            let start = local + 30 + u16_at(bytes, local + 26)? + u16_at(bytes, local + 28)?;
            let data = bytes.get(start..start + compressed).ok_or_else(invalid)?;
            let decoded = match method {
                0 => data.to_vec(),
                8 => {
                    // lopdf accepts raw DEFLATE after a zlib header. ZIP supplies
                    // CRC32 instead of Adler32; verify that checksum below.
                    let mut zlib = vec![0x78, 0x9c];
                    zlib.extend_from_slice(data);
                    let stream =
                        lopdf::Stream::new(lopdf::dictionary! { "Filter" => "FlateDecode" }, zlib);
                    stream.decompressed_content_with_limit(size)?
                }
                _ => return Err(invalid()),
            };
            if decoded.len() != size || crc32(&decoded) as usize != u32_at(bytes, at + 16)? {
                return Err(invalid());
            }
            return Ok(Some(decoded));
        }
        at += 46 + length + u16_at(bytes, at + 30)? + u16_at(bytes, at + 32)?;
    }
    Ok(None)
}

pub(super) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Element {
    pub name: String,
    pub attrs: BTreeMap<String, String>,
    pub children: Vec<Element>,
    pub text: String,
}
impl Element {
    pub fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|v| v.name == name)
    }
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }
    pub fn value(&self, child: &str) -> Option<&str> {
        self.child(child)?.attr("val")
    }
    pub fn path(&self, names: &[&str]) -> Option<&Self> {
        names.iter().try_fold(self, |node, name| node.child(name))
    }
}
pub(crate) fn parse(bytes: &[u8]) -> Result<Element, Error> {
    let mut reader = EventReader::new(bytes);
    let mut stack = vec![Element::default()];
    loop {
        match reader
            .next_event()
            .map_err(|err| Error::Docx(Box::new(err)))?
        {
            XmlEvent::StartElement {
                name, attributes, ..
            } => {
                if stack.len() > 128 {
                    return Err(invalid());
                }
                stack.push(Element {
                    name: name.local_name,
                    attrs: attributes
                        .into_iter()
                        .map(|v| (v.name.local_name, v.value))
                        .collect(),
                    ..Element::default()
                });
            }
            XmlEvent::Characters(text) | XmlEvent::Whitespace(text) => {
                stack.last_mut().ok_or_else(invalid)?.text.push_str(&text);
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
    stack
        .pop()
        .and_then(|v| v.children.into_iter().next())
        .ok_or_else(invalid)
}
