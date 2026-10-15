//! Admission scan before lopdf allocates its xref/object graph. Stream payloads
//! are skipped by length, never tokenized as PDF syntax. Only one object's
//! dictionary and one bounded decoded stream are retained at a time.
use std::collections::BTreeMap;

use lopdf::{Dictionary, Object, Stream};

use super::{DEPTH, DOCUMENT_BYTES, OBJECTS, STREAM_BYTES, decode, limit};
use crate::Error;

fn syntax() -> Error {
    lopdf::Error::Parse(lopdf::ParseError::InvalidContentStream).into()
}

fn space(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}
fn delimiter(b: u8) -> bool {
    space(b) || b"()<>[]{}/%".contains(&b)
}

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Word(&'a [u8]),
    Name(Vec<u8>),
    OpenDict,
    CloseDict,
    OpenArray,
    CloseArray,
    String,
}

struct Scan<'a> {
    bytes: &'a [u8],
    at: usize,
    values: usize,
}
impl<'a> Scan<'a> {
    fn ws(&mut self) {
        loop {
            while self.bytes.get(self.at).is_some_and(|b| space(*b)) {
                self.at += 1;
            }
            if self.bytes.get(self.at) != Some(&b'%') {
                break;
            }
            while self
                .bytes
                .get(self.at)
                .is_some_and(|b| !matches!(b, b'\r' | b'\n'))
            {
                self.at += 1;
            }
        }
    }
    fn token(&mut self) -> Result<Option<Token<'a>>, Error> {
        self.ws();
        let Some(&b) = self.bytes.get(self.at) else {
            return Ok(None);
        };
        self.at += 1;
        Ok(Some(match b {
            b'<' if self.bytes.get(self.at) == Some(&b'<') => {
                self.at += 1;
                Token::OpenDict
            }
            b'>' if self.bytes.get(self.at) == Some(&b'>') => {
                self.at += 1;
                Token::CloseDict
            }
            b'[' => Token::OpenArray,
            b']' => Token::CloseArray,
            b'(' => {
                let mut depth = 1;
                while let Some(&c) = self.bytes.get(self.at) {
                    self.at += 1;
                    match c {
                        b'\\' => self.at = (self.at + 1).min(self.bytes.len()),
                        b'(' => {
                            depth += 1;
                            if depth > DEPTH {
                                return Err(limit("object_depth", DEPTH));
                            }
                        }
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if depth != 0 {
                    return Err(syntax());
                }
                Token::String
            }
            b'<' => {
                while self.bytes.get(self.at).is_some_and(|b| *b != b'>') {
                    self.at += 1;
                }
                if self.bytes.get(self.at) != Some(&b'>') {
                    return Err(syntax());
                }
                self.at += 1;
                Token::String
            }
            b'/' => {
                let start = self.at;
                while self.bytes.get(self.at).is_some_and(|b| !delimiter(*b)) {
                    self.at += 1;
                }
                let mut name = Vec::new();
                let mut i = start;
                while i < self.at {
                    if self.bytes[i] == b'#' && i + 2 < self.at {
                        let hex =
                            std::str::from_utf8(&self.bytes[i + 1..i + 3]).map_err(|_| syntax())?;
                        name.push(u8::from_str_radix(hex, 16).map_err(|_| syntax())?);
                        i += 3;
                    } else {
                        name.push(self.bytes[i]);
                        i += 1;
                    }
                }
                Token::Name(name)
            }
            _ => {
                let start = self.at - 1;
                while self.bytes.get(self.at).is_some_and(|b| !delimiter(*b)) {
                    self.at += 1;
                }
                Token::Word(&self.bytes[start..self.at])
            }
        }))
    }
    fn value(&mut self, token: Token<'a>, depth: usize) -> Result<Object, Error> {
        if depth > DEPTH {
            return Err(limit("object_depth", DEPTH));
        }
        self.values += 1;
        if self.values > OBJECTS {
            return Err(limit("objects", OBJECTS));
        }
        Ok(match token {
            Token::OpenDict => {
                let mut dict = Dictionary::new();
                loop {
                    match self.token()?.ok_or_else(syntax)? {
                        Token::CloseDict => break,
                        Token::Name(name) => {
                            let token = self.token()?.ok_or_else(syntax)?;
                            let value = self.value(token, depth + 1)?;
                            dict.set(name, value);
                        }
                        _ => return Err(syntax()),
                    }
                }
                Object::Dictionary(dict)
            }
            Token::OpenArray => {
                let mut items = Vec::new();
                loop {
                    let token = self.token()?.ok_or_else(syntax)?;
                    if token == Token::CloseArray {
                        break;
                    }
                    items.push(self.value(token, depth + 1)?);
                }
                Object::Array(items)
            }
            Token::Name(name) => Object::Name(name),
            Token::String => Object::Null, // Strings have no admission metadata.
            Token::Word(word) => {
                let number = std::str::from_utf8(word)
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok());
                if let Some(number) = number {
                    let saved = self.at;
                    if let Some(Token::Word(generation)) = self.token()?
                        && let Ok(generation) =
                            std::str::from_utf8(generation).unwrap_or("").parse::<u16>()
                        && self.token()? == Some(Token::Word(b"R"))
                    {
                        Object::Reference((
                            u32::try_from(number).map_err(|_| syntax())?,
                            generation,
                        ))
                    } else {
                        self.at = saved;
                        Object::Integer(number)
                    }
                } else {
                    Object::Null
                }
            }
            _ => return Err(syntax()),
        })
    }
    fn stream_data(&mut self, length: Option<usize>) -> Result<&'a [u8], Error> {
        // The keyword is followed by EOL, not arbitrary whitespace (which may
        // also be the first byte of a valid binary stream).
        let cr = self.bytes.get(self.at) == Some(&b'\r');
        if cr {
            self.at += 1;
        }
        if self.bytes.get(self.at) == Some(&b'\n') {
            self.at += 1;
        } else if !cr {
            return Err(syntax());
        }
        let start = self.at;
        let end = match length {
            Some(n) => start
                .checked_add(n)
                .filter(|end| *end <= self.bytes.len())
                .ok_or_else(syntax)?,
            None => {
                // Forward indirect Length: use a bounded delimiter search,
                // then verify the resolved declaration after the scan.
                let rest = &self.bytes[start
                    ..self
                        .bytes
                        .len()
                        .min(start.saturating_add(STREAM_BYTES + 16))];
                let n = rest
                    .windows(9)
                    .position(|w| w == b"endstream")
                    .ok_or_else(|| limit("encoded_bytes", STREAM_BYTES))?;
                start + n
            }
        };
        if end - start > STREAM_BYTES {
            return Err(limit("encoded_bytes", STREAM_BYTES));
        }
        self.at = end;
        if self.token()? != Some(Token::Word(b"endstream")) {
            return Err(syntax());
        }
        Ok(&self.bytes[start..end])
    }
}

fn declared_counts(dict: &Dictionary) -> Result<(), Error> {
    for key in [b"N".as_slice(), b"Size"] {
        if let Ok(n) = dict.get(key).and_then(Object::as_i64)
            && (n < 0 || n as u64 > (OBJECTS + 1) as u64)
        {
            return Err(limit("objects", OBJECTS));
        }
    }
    if dict
        .get(b"Type")
        .is_ok_and(|t| t.as_name().is_ok_and(|n| n == b"XRef"))
    {
        if let Ok(items) = dict.get(b"Index").and_then(Object::as_array) {
            if items.len() % 2 != 0 {
                return Err(syntax());
            }
            let mut total = 0usize;
            for pair in items.as_chunks::<2>().0 {
                let start = usize::try_from(pair[0].as_i64()?).map_err(|_| syntax())?;
                let count = usize::try_from(pair[1].as_i64()?).map_err(|_| syntax())?;
                total = total.saturating_add(count);
                if total > OBJECTS || start.saturating_add(count) > OBJECTS + 1 {
                    return Err(limit("objects", OBJECTS));
                }
            }
        }
        if let Ok(items) = dict.get(b"W").and_then(Object::as_array)
            && (items.len() != 3
                || items
                    .iter()
                    .any(|v| !v.as_i64().is_ok_and(|n| (0..=8).contains(&n))))
        {
            return Err(syntax());
        }
    }
    Ok(())
}

pub(super) fn check(bytes: &[u8]) -> Result<(), Error> {
    let mut scan = Scan {
        bytes,
        at: 0,
        values: 0,
    };
    let mut integers = BTreeMap::new();
    let mut lengths = Vec::new();
    let mut count = 0usize;
    let mut total = 0usize;
    let mut xref_count = 0usize;
    while let Some(token) = scan.token()? {
        if token == Token::Word(b"xref") {
            loop {
                let saved = scan.at;
                let first = scan.token()?;
                let second = scan.token()?;
                let number = |token| match token {
                    Some(Token::Word(word)) => {
                        std::str::from_utf8(word).ok()?.parse::<usize>().ok()
                    }
                    _ => None,
                };
                let (Some(start), Some(n)) = (number(first), number(second)) else {
                    scan.at = saved;
                    break;
                };
                xref_count = xref_count.saturating_add(n);
                if xref_count > OBJECTS || start.saturating_add(n) > OBJECTS + 1 {
                    return Err(limit("objects", OBJECTS));
                }
                for _ in 0..n {
                    for _ in 0..3 {
                        scan.token()?.ok_or_else(syntax)?;
                    }
                }
            }
            continue;
        }
        if token == Token::Word(b"trailer") {
            let token = scan.token()?.ok_or_else(syntax)?;
            let dict = scan.value(token, 0)?;
            declared_counts(dict.as_dict()?)?;
            continue;
        }
        let Token::Word(id) = token else { continue };
        let Ok(id) = std::str::from_utf8(id).unwrap_or("").parse::<u32>() else {
            continue;
        };
        let saved = scan.at;
        let Some(Token::Word(generation)) = scan.token()? else {
            scan.at = saved;
            continue;
        };
        let Ok(generation) = std::str::from_utf8(generation).unwrap_or("").parse::<u16>() else {
            scan.at = saved;
            continue;
        };
        if scan.token()? != Some(Token::Word(b"obj")) {
            scan.at = saved;
            continue;
        }
        count += 1;
        if count > OBJECTS {
            return Err(limit("objects", OBJECTS));
        }
        let token = scan.token()?.ok_or_else(syntax)?;
        let object = scan.value(token, 0)?;
        if let Object::Integer(n) = object {
            integers.insert((id, generation), n);
        }
        let next = scan.token()?;
        if next == Some(Token::Word(b"stream")) {
            let dict = object.as_dict()?;
            let declared = dict.get(b"Length")?;
            let length = match declared {
                Object::Integer(n) => Some(usize::try_from(*n).map_err(|_| syntax())?),
                Object::Reference(id) => integers.get(id).and_then(|n| usize::try_from(*n).ok()),
                _ => return Err(syntax()),
            };
            if length.is_some_and(|n| n > STREAM_BYTES) {
                return Err(limit("encoded_bytes", STREAM_BYTES));
            }
            declared_counts(dict)?;
            if let Ok(n) = dict.get(b"N").and_then(Object::as_i64) {
                count = count.saturating_add(usize::try_from(n).map_err(|_| syntax())?);
                if count > OBJECTS {
                    return Err(limit("objects", OBJECTS));
                }
            }
            let data = scan.stream_data(length)?;
            if let Object::Reference(id) = declared {
                lengths.push((*id, data.len()));
            }
            let stream = Stream::new(dict.clone(), data.to_vec());
            let maximum = STREAM_BYTES.min(DOCUMENT_BYTES.saturating_sub(total));
            let size = match decode(&stream, maximum) {
                Ok(decoded) => {
                    // Object streams contain more PDF values. Scan their syntax
                    // before the library eagerly parses them, too.
                    if dict
                        .get(b"Type")
                        .is_ok_and(|t| t.as_name().is_ok_and(|n| n == b"ObjStm"))
                    {
                        let mut nested = Scan {
                            bytes: &decoded,
                            at: 0,
                            values: scan.values,
                        };
                        while let Some(token) = nested.token()? {
                            nested.value(token, 0)?;
                        }
                        scan.values = nested.values;
                    }
                    decoded.len()
                }
                Err(Error::Pdf(lopdf::Error::Unimplemented(_))) => data.len(),
                Err(error) => return Err(error),
            };
            total = total.saturating_add(size);
            if total > DOCUMENT_BYTES {
                return Err(limit("document_bytes", DOCUMENT_BYTES));
            }
            if scan.token()? != Some(Token::Word(b"endobj")) {
                return Err(syntax());
            }
        } else if next != Some(Token::Word(b"endobj")) {
            return Err(syntax());
        }
    }
    for (id, observed) in lengths {
        let declared = integers
            .get(&id)
            .and_then(|n| usize::try_from(*n).ok())
            .ok_or_else(syntax)?;
        if declared > STREAM_BYTES {
            return Err(limit("encoded_bytes", STREAM_BYTES));
        }
        // A delimiter-based forward Length scan may include the final EOL.
        if observed < declared || observed - declared > 2 {
            return Err(syntax());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_strings_comments_and_length_delimited_payloads_are_not_objects() -> Result<(), Error>
    {
        let data = b"endstream 999999 0 obj << /Length 999999999 >>";
        let mut pdf = format!("%PDF-1.5\n% 1 0 obj /Length 999999999\n1 0 obj << /String (2 0 obj << /Length 999999999 >>) /Hex <656e6473747265616d> /Length {} >> stream\r", data.len()).into_bytes();
        pdf.extend_from_slice(data);
        pdf.extend_from_slice(b"\rendstream\nendobj\n");
        check(&pdf)
    }

    #[test]
    fn escaped_keys_forward_lengths_and_crlf_are_checked() -> Result<(), Error> {
        check(b"%PDF-1.5\n1 0 obj << /L#65ngth 2 0 R >> stream\r\nq Q\nendstream\nendobj\n2 0 obj 3 endobj")?;
        let error =
            check(b"%PDF-1.5\n1 0 obj << /L#65ngth 999999999 >> stream\nx\nendstream\nendobj")
                .err()
                .ok_or_else(syntax)?;
        assert_eq!(error.code(), crate::messages::MessageCode::PdfContentLimit);
        Ok(())
    }
}
