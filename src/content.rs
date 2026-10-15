//! Content-stream parsing with byte spans.
//!
//! In-place PDF editing has to leave every byte it does not touch alone, so this
//! parser keeps the byte span of every operator and every string it reads.

use std::ops::Range;

use crate::{
    Error,
    messages::{MessageCode, params},
};

const MAX_DEPTH: usize = 64;
const MAX_OPERANDS: usize = 100_000;

fn limit(kind: &str, maximum: usize) -> Error {
    Error::invalid(
        MessageCode::PdfContentLimit,
        params(&[("kind", kind.into()), ("limit", maximum.into())]),
    )
}

/// A string operand with the byte range of its *content* (between the
/// delimiters, quotes excluded).
#[derive(Clone, Debug)]
pub(crate) struct StrVal {
    /// Decoded bytes (escapes resolved, hex pairs decoded).
    pub bytes: Vec<u8>,
    /// Absolute range of the content inside the stream.
    pub span: Range<usize>,
    /// `true` for `<...>`, `false` for `(...)`.
    pub hex: bool,
}

/// One operand of an operator.
#[derive(Clone, Debug)]
pub(crate) enum Operand {
    Number(f64),
    Name(Vec<u8>),
    Str(StrVal),
    Array(Vec<Operand>),
    /// A dictionary or anything else that does not matter for text editing.
    Other,
}

/// One operator with its operands and source span.
#[derive(Clone, Debug)]
pub(crate) struct Operator {
    pub keyword: Vec<u8>,
    pub operands: Vec<Operand>,
    /// First byte of the first operand (or of the keyword when there are none).
    pub start: usize,
    /// One past the last byte of the keyword.
    pub end: usize,
}

impl Operator {
    pub(crate) fn is(&self, keyword: &str) -> bool {
        self.keyword == keyword.as_bytes()
    }

    pub(crate) fn number(&self, index: usize) -> Option<f64> {
        match self.operands.get(index) {
            Some(Operand::Number(value)) => Some(*value),
            _ => None,
        }
    }

    /// The last `n` operands as numbers.
    pub(crate) fn tail_numbers<const N: usize>(&self) -> Option<[f64; N]> {
        let start = self.operands.len().checked_sub(N)?;
        let mut out = [0.0; N];
        for (slot, operand) in out.iter_mut().zip(&self.operands[start..]) {
            match operand {
                Operand::Number(value) => *slot = *value,
                _ => return None,
            }
        }
        Some(out)
    }

    pub(crate) fn name(&self, index: usize) -> Option<&[u8]> {
        match self.operands.get(index) {
            Some(Operand::Name(name)) => Some(name),
            _ => None,
        }
    }
}

fn is_ws(byte: u8) -> bool {
    matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0C' | b'\r' | b' ')
}

fn is_delim(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

struct Scanner<'a> {
    data: &'a [u8],
    pos: usize,
    operands: usize,
}

impl<'a> Scanner<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self {
            data,
            pos,
            operands: 0,
        }
    }

    fn skip_ws(&mut self) {
        loop {
            match self.data.get(self.pos) {
                Some(byte) if is_ws(*byte) => self.pos += 1,
                Some(b'%') => {
                    while let Some(byte) = self.data.get(self.pos) {
                        self.pos += 1;
                        if *byte == b'\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    /// Read one object, or `None` when the next token is a keyword.
    fn object(&mut self, depth: usize) -> Result<Option<Operand>, Error> {
        if depth >= MAX_DEPTH {
            return Err(limit("depth", MAX_DEPTH));
        }
        self.operands += 1;
        if self.operands > MAX_OPERANDS {
            return Err(limit("operands", MAX_OPERANDS));
        }
        let Some(&byte) = self.data.get(self.pos) else {
            return Ok(None);
        };
        Ok(match byte {
            b'(' => Some(Operand::Str(self.literal_string()?)),
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.skip_dict(depth)?;
                    Some(Operand::Other)
                } else {
                    Some(Operand::Str(self.hex_string()))
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.data.get(self.pos) {
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        Some(_) => match self.object(depth + 1)? {
                            Some(item) => items.push(item),
                            None => self.pos += 1,
                        },
                        None => break,
                    }
                }
                Some(Operand::Array(items))
            }
            b'/' => Some(Operand::Name(self.name())),
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.number().map(Operand::Number),
            _ => None,
        })
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.pos;
        if matches!(self.data.get(self.pos), Some(b'+') | Some(b'-')) {
            self.pos += 1;
        }
        let mut seen = false;
        while let Some(byte) = self.data.get(self.pos) {
            if byte.is_ascii_digit() {
                seen = true;
                self.pos += 1;
            } else if *byte == b'.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        if !seen {
            return None;
        }
        std::str::from_utf8(self.data.get(start..self.pos)?)
            .ok()?
            .parse::<f64>()
            .ok()
    }

    fn name(&mut self) -> Vec<u8> {
        self.pos += 1;
        let start = self.pos;
        while let Some(byte) = self.data.get(self.pos) {
            if is_ws(*byte) || is_delim(*byte) {
                break;
            }
            self.pos += 1;
        }
        self.data.get(start..self.pos).unwrap_or_default().to_vec()
    }

    fn literal_string(&mut self) -> Result<StrVal, Error> {
        let content_start = self.pos + 1;
        self.pos += 1;
        let mut depth = 1usize;
        let mut bytes = Vec::new();
        while let Some(&byte) = self.data.get(self.pos) {
            match byte {
                b'\\' => {
                    self.pos += 1;
                    if let Some(escaped) = self.escape() {
                        bytes.push(escaped);
                    }
                }
                b'(' => {
                    depth += 1;
                    if depth > MAX_DEPTH {
                        return Err(limit("depth", MAX_DEPTH));
                    }
                    bytes.push(byte);
                    self.pos += 1;
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    bytes.push(byte);
                    self.pos += 1;
                }
                _ => {
                    bytes.push(byte);
                    self.pos += 1;
                }
            }
        }
        let content_end = self.pos;
        if self.data.get(self.pos) == Some(&b')') {
            self.pos += 1;
        }
        Ok(StrVal {
            bytes,
            span: content_start..content_end,
            hex: false,
        })
    }

    fn escape(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos)?;
        self.pos += 1;
        match byte {
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'b' => Some(b'\x08'),
            b'f' => Some(b'\x0C'),
            b'(' => Some(b'('),
            b')' => Some(b')'),
            b'\\' => Some(b'\\'),
            b'\n' => None,
            b'0'..=b'7' => {
                let mut value = (byte - b'0') as u16;
                for _ in 0..2 {
                    match self.data.get(self.pos) {
                        Some(next @ b'0'..=b'7') => {
                            value = value * 8 + (next - b'0') as u16;
                            self.pos += 1;
                        }
                        _ => break,
                    }
                }
                Some((value & 0xFF) as u8)
            }
            other => Some(other),
        }
    }

    fn hex_string(&mut self) -> StrVal {
        let content_start = self.pos + 1;
        self.pos += 1;
        let mut digits = Vec::new();
        while let Some(&byte) = self.data.get(self.pos) {
            if byte == b'>' {
                break;
            }
            if byte.is_ascii_hexdigit() {
                digits.push(byte);
            }
            self.pos += 1;
        }
        let content_end = self.pos;
        if self.data.get(self.pos) == Some(&b'>') {
            self.pos += 1;
        }
        if digits.len() % 2 == 1 {
            digits.push(b'0');
        }
        let mut bytes = Vec::with_capacity(digits.len() / 2);
        for pair in digits.chunks(2) {
            if let [high, low] = pair
                && let (Some(high), Some(low)) = (hex_value(*high), hex_value(*low))
            {
                bytes.push((high << 4) | low);
            }
        }
        StrVal {
            bytes,
            span: content_start..content_end,
            hex: true,
        }
    }

    fn skip_dict(&mut self, depth: usize) -> Result<(), Error> {
        self.pos += 2;
        loop {
            self.skip_ws();
            if self.data.get(self.pos..self.pos + 2) == Some(b">>") {
                self.pos += 2;
                break;
            }
            if self.pos >= self.data.len() {
                break;
            }
            let at = self.pos;
            self.object(depth + 1)?;
            if self.pos == at {
                self.pos += 1;
            }
        }
        Ok(())
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parse a decoded content stream into operators. Unreadable bytes are skipped.
pub(crate) fn parse(data: &[u8]) -> Result<Vec<Operator>, Error> {
    let mut operators = Vec::new();
    let mut scanner = Scanner::new(data, 0);
    let mut operands: Vec<Operand> = Vec::new();
    let mut operands_start: Option<usize> = None;

    loop {
        scanner.skip_ws();
        let Some(&byte) = scanner.data.get(scanner.pos) else {
            break;
        };
        let at = scanner.pos;
        let is_operand = matches!(
            byte,
            b'(' | b'<' | b'[' | b'/' | b'+' | b'-' | b'.' | b'0'..=b'9'
        );
        if is_operand {
            match scanner.object(0)? {
                Some(operand) => {
                    if operands.is_empty() {
                        operands_start = Some(at);
                    }
                    operands.push(operand);
                }
                None => scanner.pos = at + 1,
            }
            continue;
        }
        if is_delim(byte) {
            scanner.pos += 1;
            continue;
        }
        let keyword = scanner.read_keyword();
        if keyword.is_empty() {
            scanner.pos = at + 1;
            continue;
        }
        if keyword == b"BI" {
            scanner.pos = skip_inline_image(data, scanner.pos);
            operands.clear();
            operators.push(Operator {
                keyword,
                operands: Vec::new(),
                start: at,
                end: scanner.pos,
            });
            operands_start = None;
            continue;
        }
        operators.push(Operator {
            keyword,
            operands: std::mem::take(&mut operands),
            start: operands_start.take().unwrap_or(at),
            end: scanner.pos,
        });
    }
    Ok(operators)
}

impl<'a> Scanner<'a> {
    fn read_keyword(&mut self) -> Vec<u8> {
        let start = self.pos;
        while let Some(byte) = self.data.get(self.pos) {
            if is_ws(*byte) || is_delim(*byte) {
                break;
            }
            self.pos += 1;
        }
        self.data.get(start..self.pos).unwrap_or_default().to_vec()
    }
}

/// Skip an inline image: find `ID`, then the next `EI` token.
fn skip_inline_image(data: &[u8], from: usize) -> usize {
    let mut pos = from;
    // Find the `ID` keyword.
    loop {
        while data.get(pos).is_some_and(|byte| is_ws(*byte)) {
            pos += 1;
        }
        if pos + 2 > data.len() {
            return data.len();
        }
        if data.get(pos..pos + 2) == Some(b"ID")
            && data
                .get(pos + 2)
                .is_none_or(|byte| is_ws(*byte) || is_delim(*byte))
        {
            pos += 2;
            break;
        }
        pos += 1;
    }
    // One whitespace byte separates `ID` from the data.
    if data.get(pos).is_some_and(|byte| is_ws(*byte)) {
        pos += 1;
    }
    // Find `EI` preceded by whitespace.
    let mut i = pos;
    while i + 2 <= data.len() {
        if data.get(i..i + 2) == Some(b"EI")
            && i > pos
            && data.get(i - 1).is_some_and(|byte| is_ws(*byte))
            && data
                .get(i + 2)
                .is_none_or(|byte| is_ws(*byte) || is_delim(*byte))
        {
            return i + 2;
        }
        i += 1;
    }
    data.len()
}

/// Splice byte ranges into a stream. `edits` are non-overlapping and sorted by
/// start; every other byte is copied unchanged.
pub(crate) fn splice(data: &[u8], edits: &[(Range<usize>, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut at = 0usize;
    for (range, replacement) in edits {
        let start = range.start.clamp(at, data.len());
        let end = range.end.clamp(start, data.len());
        out.extend_from_slice(data.get(at..start).unwrap_or_default());
        out.extend_from_slice(replacement);
        at = end;
    }
    out.extend_from_slice(data.get(at..).unwrap_or_default());
    out
}

/// Escape bytes for a PDF literal string.
pub(crate) fn literal(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.push(b'(');
    escape_literal_into(bytes, &mut out);
    out.push(b')');
    out
}

/// Escape bytes for the interior of a literal string (no delimiters).
pub(crate) fn escape_literal(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    escape_literal_into(bytes, &mut out);
    out
}

fn escape_literal_into(bytes: &[u8], out: &mut Vec<u8>) {
    for &byte in bytes {
        match byte {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(byte);
            }
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x20..=0x7E => out.push(byte),
            other => out.extend_from_slice(format!("\\{other:03o}").as_bytes()),
        }
    }
}

/// Serialize bytes as a PDF hex string.
pub(crate) fn hex_string(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2 + 2);
    out.push(b'<');
    hex_digits(bytes, &mut out);
    out.push(b'>');
    out
}

/// Hex digits for the interior of a hex string (no delimiters).
pub(crate) fn hex_content(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    hex_digits(bytes, &mut out);
    out
}

fn hex_digits(bytes: &[u8], out: &mut Vec<u8>) {
    for byte in bytes {
        out.extend_from_slice(format!("{byte:02X}").as_bytes());
    }
}
