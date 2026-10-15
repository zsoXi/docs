//! Font decoding and encoding for PDF text.
//!
//! Text in a content stream is stored as font-specific codes. To search or edit
//! it we map codes to Unicode through the font's `ToUnicode` CMap, its
//! `Encoding` (WinAnsi, MacRoman, Differences) or, for composite fonts, its CID
//! CMap.

use std::collections::HashMap;

use lopdf::{Dictionary, Document, Object};
use printpdf::cmap::{CidCMap, ToUnicodeCMap};

/// Everything needed to decode and re-encode text shown with one font.
#[derive(Debug, Default)]
pub(crate) struct FontInfo {
    type0: bool,
    to_unicode: Option<ToUnicodeCMap>,
    cid_cmap: Option<CidCMap>,
    /// Simple-font code → character.
    encoding: HashMap<u8, char>,
    /// Code (or CID) → width in 1/1000 em.
    widths: HashMap<u32, f64>,
    default_width: f64,
    embedded: Option<Vec<u8>>,
    geometry_glyphs: HashMap<char, u16>,
    geometry_font: Option<Vec<u8>>,
    /// `/BaseFont` without a subset prefix.
    base_font: String,
    /// Glyph ID to CID, from a non-identity `CIDToGIDMap` stream.
    gid_to_cid: Option<HashMap<u32, u32>>,
    bold: bool,
    italic: bool,
}

/// Decoded text and the byte range of every character inside the source bytes.
#[derive(Debug, Default)]
pub(crate) struct Decoded {
    pub text: String,
    /// Original Unicode glyphs for advance and ink measurements, before
    /// expanding typographic ligatures into searchable letters.
    pub glyph_text: String,
    /// `(start, end)` for every character, relative to the input bytes.
    pub chars: Vec<(usize, usize)>,
}

impl FontInfo {
    /// Read a font dictionary from a page's resources.
    pub(crate) fn load(dict: &Dictionary, doc: &Document) -> Self {
        let subtype = dict
            .get(b"Subtype")
            .ok()
            .and_then(|value| value.as_name().ok())
            .unwrap_or(b"")
            .to_vec();
        let type0 = subtype == b"Type0";
        let to_unicode = to_unicode_from_font(dict, doc);

        let mut info = FontInfo {
            type0,
            to_unicode,
            cid_cmap: if type0 {
                load_cid_cmap(dict, doc)
            } else {
                None
            },
            encoding: if type0 {
                HashMap::new()
            } else {
                load_simple_encoding(dict, doc)
            },
            widths: HashMap::new(),
            default_width: if type0 { 1000.0 } else { 0.0 },
            embedded: None,
            geometry_glyphs: HashMap::new(),
            geometry_font: None,
            gid_to_cid: None,
            bold: false,
            italic: false,
            base_font: dict
                .get(b"BaseFont")
                .ok()
                .and_then(|value| value.as_name().ok())
                .map(|name| {
                    let name = String::from_utf8_lossy(name).into_owned();
                    match name.split_once('+') {
                        Some((prefix, rest))
                            if prefix.len() == 6
                                && prefix.chars().all(|ch| ch.is_ascii_uppercase()) =>
                        {
                            rest.to_string()
                        }
                        _ => name,
                    }
                })
                .unwrap_or_default(),
        };
        let lower = info.base_font.to_ascii_lowercase();
        info.bold = lower.contains("bold") || lower.contains("black");
        info.italic = lower.contains("italic") || lower.contains("oblique");
        info.load_widths(dict, doc);
        info.embedded = embedded_font_bytes(dict, doc);
        if info.embedded.is_none()
            && let Some(builtin) = printpdf::BuiltinFont::from_id(&info.base_font)
        {
            let subset = builtin.get_subset_font();
            info.geometry_glyphs = subset
                .glyph_mapping
                .values()
                .filter_map(|(gid, text)| Some((text.chars().next()?, *gid)))
                .collect();
            info.geometry_font = Some(subset.bytes);
        }
        info.gid_to_cid = load_gid_to_cid(dict, doc);
        info
    }

    /// Whether the face is bold, from its `/BaseFont` name.
    pub(crate) fn is_bold(&self) -> bool {
        self.bold
    }

    /// Whether the face is italic, from its `/BaseFont` name.
    pub(crate) fn is_italic(&self) -> bool {
        self.italic
    }

    /// The CID (the code written to the content stream) for a glyph ID.
    fn cid_for_gid(&self, gid: u32) -> u32 {
        self.gid_to_cid
            .as_ref()
            .and_then(|map| map.get(&gid).copied())
            .unwrap_or(gid)
    }

    fn load_widths(&mut self, dict: &Dictionary, doc: &Document) {
        if let Ok(descendants) = dict.get_deref(b"DescendantFonts", doc)
            && let Ok(array) = descendants.as_array()
            && let Some(first) = array.first()
            && let Ok(descendant) = resolve(first, doc)
            && let Ok(descendant) = descendant.as_dict()
        {
            self.default_width = descendant
                .get(b"DW")
                .ok()
                .and_then(|value| value.as_float().ok())
                .map(f64::from)
                .unwrap_or(1000.0);
            if let Ok(w) = descendant.get(b"W").and_then(|value| value.as_array()) {
                parse_w_array(w, &mut self.widths);
            }
            return;
        }
        let first = dict
            .get(b"FirstChar")
            .ok()
            .and_then(|value| value.as_i64().ok())
            .unwrap_or(0);
        if let Ok(array) = dict.get(b"Widths").and_then(|value| value.as_array()) {
            for (index, width) in array.iter().enumerate() {
                if let Ok(width) = width.as_float() {
                    let code = (first + index as i64).max(0) as u32;
                    self.widths.insert(code, f64::from(width));
                }
            }
        }
    }

    /// Decode one shown string into Unicode and per-character byte ranges.
    pub(crate) fn decode(&self, bytes: &[u8]) -> Decoded {
        let mut out = Decoded::default();
        if self.type0 {
            let Some(cmap) = &self.cid_cmap else {
                return self.decode_fallback(bytes);
            };
            let mut pos = 0usize;
            for (code, len) in cmap.split_codes(bytes) {
                let len = len as usize;
                let end = (pos + len).min(bytes.len());
                let cid = cmap.cid_for_code(code, len as u8);
                if let Some(text) = self.unicode_for(cid).or_else(|| self.unicode_for(code)) {
                    push_chars(&mut out, text, pos, end);
                }
                pos = end;
            }
            return out;
        }
        for (index, byte) in bytes.iter().enumerate() {
            let code = *byte as u32;
            let text = self
                .to_unicode
                .as_ref()
                .and_then(|cmap| cmap.lookup_string(code))
                .or_else(|| self.encoding.get(byte).map(|ch| ch.to_string()));
            if let Some(text) = text {
                push_chars(&mut out, text, index, index + 1);
            }
        }
        out
    }

    fn decode_fallback(&self, bytes: &[u8]) -> Decoded {
        let mut out = Decoded::default();
        for (index, byte) in bytes.iter().enumerate() {
            let ch = cp1252(*byte);
            push_chars(&mut out, ch.to_string(), index, index + 1);
        }
        out
    }

    fn unicode_for(&self, code: u32) -> Option<String> {
        self.to_unicode
            .as_ref()
            .and_then(|cmap| cmap.lookup_string(code))
    }

    /// Encode `text` with this font, when every character has a code. Returns
    /// `None` when a glyph is missing (the caller must substitute a font).
    pub(crate) fn encode(&self, text: &str) -> Option<Vec<u8>> {
        if self.type0 {
            self.encode_type0(text)
        } else {
            self.encode_simple(text)
        }
    }

    fn encode_type0(&self, text: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(text.len() * 2);
        for ch in text.chars() {
            let glyphs = self.glyphs_for_char(&ch)?;
            let gid = *glyphs.first()?;
            if gid == 0 || gid > u16::MAX as u32 {
                return None;
            }
            out.push((gid >> 8) as u8);
            out.push((gid & 0xFF) as u8);
        }
        Some(out)
    }

    /// CIDs that display `ch`: from the cmap of the embedded font program when
    /// available (a subset font only has the glyphs it kept, so a cmap hit
    /// whose glyph has no outline does not count), else from the reverse
    /// `ToUnicode` map.
    fn glyphs_for_char(&self, ch: &char) -> Option<Vec<u32>> {
        if let Some(bytes) = &self.embedded
            && let Ok(face) = ttf_parser::Face::parse(bytes, 0)
            && let Some(glyph) = face.glyph_index(*ch).filter(|glyph| glyph.0 != 0)
            && (ch.is_whitespace() || face.glyph_bounding_box(glyph).is_some())
        {
            return Some(vec![self.cid_for_gid(u32::from(glyph.0))]);
        }
        // No usable glyph in the embedded program (a subset without this
        // character); a code already shown in the document still has an
        // outline, so fall through to the reverse map.
        self.reverse_unicode(*ch)
    }

    fn reverse_unicode(&self, ch: char) -> Option<Vec<u32>> {
        let cmap = self.to_unicode.as_ref()?;
        let mut codes = Vec::new();
        for (code, values) in &cmap.mappings {
            if values
                .iter()
                .filter_map(|value| char::from_u32(*value))
                .eq(std::iter::once(ch))
            {
                codes.push(*code);
            }
        }
        if codes.is_empty() { None } else { Some(codes) }
    }

    fn encode_simple(&self, text: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(text.len());
        for ch in text.chars() {
            out.push(self.simple_code(ch)?);
        }
        Some(out)
    }

    fn simple_code(&self, ch: char) -> Option<u8> {
        if let Some(cmap) = &self.to_unicode {
            for (code, values) in &cmap.mappings {
                if *code <= 0xFF
                    && values
                        .iter()
                        .filter_map(|value| char::from_u32(*value))
                        .eq(std::iter::once(ch))
                {
                    let code = *code as u8;
                    if code != 0 {
                        return Some(code);
                    }
                }
            }
        }
        for (byte, mapped) in &self.encoding {
            if *mapped == ch && *byte != 0 {
                return Some(*byte);
            }
        }
        None
    }

    /// `/BaseFont` name, used to detect bold and italic faces.
    pub(crate) fn base_font(&self) -> &str {
        &self.base_font
    }

    /// Ink bounds in em, not the advance box (spaces have no ink).
    pub(crate) fn glyph_rect(&self, ch: char) -> Option<[f64; 4]> {
        if ch.is_whitespace() {
            return None;
        }
        let face = ttf_parser::Face::parse(
            self.embedded.as_deref().or(self.geometry_font.as_deref())?,
            0,
        )
        .ok()?;
        let glyph = face
            .glyph_index(ch)
            .or_else(|| {
                self.geometry_glyphs
                    .get(&ch)
                    .copied()
                    .map(ttf_parser::GlyphId)
            })
            .or_else(|| {
                if !self.type0 {
                    return None;
                }
                let cid = *self.reverse_unicode(ch)?.first()?;
                let gid = self
                    .gid_to_cid
                    .as_ref()
                    .and_then(|m| m.iter().find_map(|(gid, c)| (*c == cid).then_some(*gid)))
                    .unwrap_or(cid);
                u16::try_from(gid).ok().map(ttf_parser::GlyphId)
            })?;
        let rect = face.glyph_bounding_box(glyph)?;
        let em = f64::from(face.units_per_em());
        Some([
            f64::from(rect.x_min) / em,
            f64::from(rect.y_min) / em,
            f64::from(rect.x_max) / em,
            f64::from(rect.y_max) / em,
        ])
    }

    /// Supply standard-face advances only to the ink-bounds interpreter.
    /// Editing keeps its historical policy for PDFs without explicit Widths.
    pub(crate) fn with_geometry_widths(mut self) -> Self {
        if self.widths.is_empty()
            && self.default_width <= 0.0
            && let Some(bytes) = &self.geometry_font
            && let Ok(face) = ttf_parser::Face::parse(bytes, 0)
        {
            let em = f64::from(face.units_per_em());
            for (&code, &ch) in &self.encoding {
                let width = if ch == ' ' {
                    // Empty outlines are absent from the bundled subsets.
                    Some(if self.base_font.starts_with("Courier") {
                        600.0
                    } else if self.base_font.starts_with("Times") || self.base_font == "Symbol" {
                        250.0
                    } else {
                        278.0
                    })
                } else {
                    self.geometry_glyphs
                        .get(&ch)
                        .copied()
                        .map(ttf_parser::GlyphId)
                        .or_else(|| face.glyph_index(ch))
                        .and_then(|g| face.glyph_hor_advance(g))
                        .map(|w| f64::from(w) / em * 1000.0)
                };
                if let Some(width) = width {
                    self.widths.insert(u32::from(code), width);
                }
            }
        }
        self
    }

    /// Width of `text` in 1/1000 em, when widths are known.
    pub(crate) fn text_width(&self, text: &str) -> Option<f64> {
        if self.widths.is_empty() && self.default_width <= 0.0 {
            return None;
        }
        let mut total = 0.0;
        for ch in text.chars() {
            let code = if self.type0 {
                self.glyphs_for_char(&ch)?.first().copied()?
            } else {
                u32::from(self.simple_code(ch)?)
            };
            total += self
                .widths
                .get(&code)
                .copied()
                .unwrap_or(self.default_width);
        }
        Some(total)
    }
}

fn push_chars(out: &mut Decoded, text: String, start: usize, end: usize) {
    out.glyph_text.push_str(&text);
    for ch in text.chars() {
        if let Some(letters) = ligature_letters(ch) {
            out.text.push_str(letters);
            out.chars.extend(letters.chars().map(|_| (start, end)));
        } else {
            out.text.push(ch);
            out.chars.push((start, end));
        }
    }
}

fn ligature_letters(ch: char) -> Option<&'static str> {
    match ch {
        'ﬀ' => Some("ff"),
        'ﬁ' => Some("fi"),
        'ﬂ' => Some("fl"),
        'ﬃ' => Some("ffi"),
        'ﬄ' => Some("ffl"),
        'ﬅ' | 'ﬆ' => Some("st"),
        _ => None,
    }
}

pub(crate) fn expand_ligatures(text: &str) -> String {
    text.chars().fold(String::new(), |mut out, ch| {
        if let Some(letters) = ligature_letters(ch) {
            out.push_str(letters);
        } else {
            out.push(ch);
        }
        out
    })
}

fn resolve<'a>(object: &'a Object, doc: &'a Document) -> Result<&'a Object, lopdf::Error> {
    match object {
        Object::Reference(id) => doc.get_object(*id),
        other => Ok(other),
    }
}

fn to_unicode_from_font(dict: &Dictionary, doc: &Document) -> Option<ToUnicodeCMap> {
    let object = dict.get_deref(b"ToUnicode", doc).ok()?;
    let content = crate::pdfbudget::stream(object.as_stream().ok()?).ok()?;
    let text = String::from_utf8_lossy(&content);
    ToUnicodeCMap::parse(&text).ok()
}

/// Glyph ID to CID from a `CIDToGIDMap` stream on the descendant font.
/// The name `Identity` (or no map) means CID and GID are the same.
fn load_gid_to_cid(dict: &Dictionary, doc: &Document) -> Option<HashMap<u32, u32>> {
    let descendants = dict.get_deref(b"DescendantFonts", doc).ok()?;
    let descendant = resolve(descendants.as_array().ok()?.first()?, doc)
        .ok()?
        .as_dict()
        .ok()?;
    let object = descendant.get_deref(b"CIDToGIDMap", doc).ok()?;
    let stream = object.as_stream().ok()?;
    let data = crate::pdfbudget::stream(stream).ok()?;
    let mut map: HashMap<u32, u32> = HashMap::new();
    for (cid, pair) in data.as_chunks::<2>().0.iter().enumerate() {
        let gid = u32::from_be_bytes([0, 0, pair[0], pair[1]]);
        if gid != 0 {
            map.entry(gid).or_insert(cid as u32);
        }
    }
    Some(map)
}

fn embedded_font_bytes(dict: &Dictionary, doc: &Document) -> Option<Vec<u8>> {
    let descriptor = descriptor_dict(dict, doc)?;
    for key in [b"FontFile2".as_slice(), b"FontFile3", b"FontFile"] {
        if let Ok(object) = descriptor.get_deref(key, doc)
            && let Ok(stream) = object.as_stream()
        {
            return crate::pdfbudget::stream(stream).ok();
        }
    }
    None
}

fn descriptor_dict<'a>(dict: &'a Dictionary, doc: &'a Document) -> Option<&'a Dictionary> {
    if let Ok(descriptor) = dict.get_deref(b"FontDescriptor", doc)
        && let Ok(descriptor) = descriptor.as_dict()
    {
        return Some(descriptor);
    }
    let descendants = dict.get_deref(b"DescendantFonts", doc).ok()?;
    let descendant = resolve(descendants.as_array().ok()?.first()?, doc)
        .ok()?
        .as_dict()
        .ok()?;
    descendant
        .get_deref(b"FontDescriptor", doc)
        .ok()?
        .as_dict()
        .ok()
}

fn load_cid_cmap(dict: &Dictionary, doc: &Document) -> Option<CidCMap> {
    let encoding = dict.get_deref(b"Encoding", doc).ok()?;
    match encoding {
        Object::Name(name) => {
            if name == b"Identity-H" {
                Some(CidCMap::identity(0))
            } else if name == b"Identity-V" {
                Some(CidCMap::identity(1))
            } else {
                Some(CidCMap::identity(0))
            }
        }
        Object::Stream(stream) => {
            let content = crate::pdfbudget::stream(stream).ok()?;
            let text = String::from_utf8_lossy(&content);
            CidCMap::parse(&text).ok().map(|(cmap, _)| cmap)
        }
        _ => Some(CidCMap::identity(0)),
    }
}

fn load_simple_encoding(dict: &Dictionary, doc: &Document) -> HashMap<u8, char> {
    let mut base = HashMap::new();
    let mut differences: Vec<(u8, String)> = Vec::new();
    let mut base_name: Option<Vec<u8>> = None;

    if let Ok(encoding) = dict.get_deref(b"Encoding", doc) {
        match encoding {
            Object::Name(name) => base_name = Some(name.clone()),
            Object::Dictionary(inner) => {
                if let Ok(name) = inner.get(b"BaseEncoding").and_then(|value| value.as_name()) {
                    base_name = Some(name.to_vec());
                }
                if let Ok(array) = inner.get(b"Differences").and_then(|value| value.as_array()) {
                    let mut code = 0u32;
                    for item in array {
                        match item {
                            Object::Integer(value) => code = (*value).max(0) as u32,
                            Object::Name(name) => {
                                differences.push((
                                    code.min(255) as u8,
                                    String::from_utf8_lossy(name).into_owned(),
                                ));
                                code = code.saturating_add(1);
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let name = base_name.as_deref().unwrap_or(b"WinAnsiEncoding");
    for byte in 0u16..=255 {
        let byte = byte as u8;
        let ch = match name {
            b"MacRomanEncoding" => macroman(byte),
            _ => cp1252(byte),
        };
        base.insert(byte, ch);
    }
    for (code, glyph) in differences {
        if let Some(ch) = glyph_name_char(&glyph) {
            base.insert(code, ch);
        }
    }
    base
}

fn parse_w_array(array: &[Object], widths: &mut HashMap<u32, f64>) {
    let mut index = 0usize;
    while index < array.len() {
        let Some(Object::Integer(first)) = array.get(index) else {
            index += 1;
            continue;
        };
        match array.get(index + 1) {
            Some(Object::Array(list)) => {
                for (offset, width) in list.iter().enumerate() {
                    if let Ok(width) = width.as_float() {
                        widths.insert(*first as u32 + offset as u32, f64::from(width));
                    }
                }
                index += 2;
            }
            Some(last) => {
                let last = last.as_i64().unwrap_or(*first);
                if let Some(width) = array.get(index + 2).and_then(|value| value.as_float().ok()) {
                    let width = f64::from(width);
                    let mut code = (*first).max(0) as u32;
                    while code <= last.max(0) as u32 {
                        widths.insert(code, width);
                        code += 1;
                    }
                }
                index += 3;
            }
            None => break,
        }
    }
}

/// Windows-1252 / Latin-1 mapping for a byte.
pub(crate) fn cp1252(byte: u8) -> char {
    match byte {
        0x80 => '\u{20AC}',
        0x82 => '\u{201A}',
        0x83 => '\u{0192}',
        0x84 => '\u{201E}',
        0x85 => '\u{2026}',
        0x86 => '\u{2020}',
        0x87 => '\u{2021}',
        0x88 => '\u{02C6}',
        0x89 => '\u{2030}',
        0x8A => '\u{0160}',
        0x8B => '\u{2039}',
        0x8C => '\u{0152}',
        0x8E => '\u{017D}',
        0x91 => '\u{2018}',
        0x92 => '\u{2019}',
        0x93 => '\u{201C}',
        0x94 => '\u{201D}',
        0x95 => '\u{2022}',
        0x96 => '\u{2013}',
        0x97 => '\u{2014}',
        0x98 => '\u{02DC}',
        0x99 => '\u{2122}',
        0x9A => '\u{0161}',
        0x9B => '\u{203A}',
        0x9C => '\u{0153}',
        0x9E => '\u{017E}',
        0x9F => '\u{0178}',
        _ => byte as char,
    }
}

/// MacRoman mapping for a byte.
pub(crate) fn macroman(byte: u8) -> char {
    const TABLE: [u16; 128] = [
        0x00C4, 0x00C5, 0x00C7, 0x00C9, 0x00D1, 0x00D6, 0x00DC, 0x00E1, 0x00E0, 0x00E2, 0x00E4,
        0x00E3, 0x00E5, 0x00E7, 0x00E9, 0x00E8, 0x00EA, 0x00EB, 0x00ED, 0x00EC, 0x00EE, 0x00EF,
        0x00F1, 0x00F3, 0x00F2, 0x00F4, 0x00F6, 0x00F5, 0x00FA, 0x00F9, 0x00FB, 0x00FC, 0x2020,
        0x00B0, 0x00A2, 0x00A3, 0x00A7, 0x2022, 0x00B6, 0x00DF, 0x00AE, 0x00A9, 0x2122, 0x00B4,
        0x00A8, 0x2260, 0x00C6, 0x00D8, 0x221E, 0x00B1, 0x2264, 0x2265, 0x00A5, 0x00B5, 0x2202,
        0x2211, 0x220F, 0x03C0, 0x222B, 0x00AA, 0x00BA, 0x03A9, 0x00E6, 0x00F8, 0x00BF, 0x00A1,
        0x00AC, 0x221A, 0x0192, 0x2248, 0x2206, 0x00AB, 0x00BB, 0x2026, 0x00A0, 0x00C0, 0x00C3,
        0x00D5, 0x0152, 0x0153, 0x2013, 0x2014, 0x201C, 0x201D, 0x2018, 0x2019, 0x00F7, 0x25CA,
        0x00FF, 0x0178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02, 0x2021, 0x00B7, 0x201A,
        0x201E, 0x2030, 0x00C2, 0x00CA, 0x00C1, 0x00CB, 0x00C8, 0x00CD, 0x00CE, 0x00CF, 0x00CC,
        0x00D3, 0x00D4, 0xF8FF, 0x00D2, 0x00DA, 0x00DB, 0x00D9, 0x0131, 0x02C6, 0x02DC, 0x00AF,
        0x02D8, 0x02D9, 0x02DA, 0x00B8, 0x02DD, 0x02DB, 0x02C7,
    ];
    if byte < 0x80 {
        return byte as char;
    }
    let code = TABLE[(byte - 0x80) as usize];
    char::from_u32(u32::from(code)).unwrap_or('\u{FFFD}')
}

/// Map an Adobe glyph name to a character for `Differences` encodings.
pub(crate) fn glyph_name_char(name: &str) -> Option<char> {
    let mut chars = name.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        return Some(ch);
    }
    let value = match name {
        "space" => 0x0020,
        "exclam" => 0x0021,
        "quotedbl" => 0x0022,
        "numbersign" => 0x0023,
        "dollar" => 0x0024,
        "percent" => 0x0025,
        "ampersand" => 0x0026,
        "quotesingle" => 0x0027,
        "parenleft" => 0x0028,
        "parenright" => 0x0029,
        "asterisk" => 0x002A,
        "plus" => 0x002B,
        "comma" => 0x002C,
        "hyphen" => 0x002D,
        "period" => 0x002E,
        "slash" => 0x002F,
        "zero" => 0x0030,
        "one" => 0x0031,
        "two" => 0x0032,
        "three" => 0x0033,
        "four" => 0x0034,
        "five" => 0x0035,
        "six" => 0x0036,
        "seven" => 0x0037,
        "eight" => 0x0038,
        "nine" => 0x0039,
        "colon" => 0x003A,
        "semicolon" => 0x003B,
        "less" => 0x003C,
        "equal" => 0x003D,
        "greater" => 0x003E,
        "question" => 0x003F,
        "at" => 0x0040,
        "bracketleft" => 0x005B,
        "backslash" => 0x005C,
        "bracketright" => 0x005D,
        "asciicircum" => 0x005E,
        "underscore" => 0x005F,
        "grave" => 0x0060,
        "braceleft" => 0x007B,
        "bar" => 0x007C,
        "braceright" => 0x007D,
        "asciitilde" => 0x007E,
        "euro" => 0x20AC,
        "quotedblleft" => 0x201C,
        "quotedblright" => 0x201D,
        "quoteleft" => 0x2018,
        "quoteright" => 0x2019,
        "endash" => 0x2013,
        "emdash" => 0x2014,
        "bullet" => 0x2022,
        "ellipsis" => 0x2026,
        "dagger" => 0x2020,
        "daggerdbl" => 0x2021,
        "guillemotleft" => 0x00AB,
        "guillemotright" => 0x00BB,
        "quotesinglbase" => 0x201A,
        "quotedblbase" => 0x201E,
        "AE" => 0x00C6,
        "ae" => 0x00E6,
        "OE" => 0x0152,
        "oe" => 0x0153,
        "Oslash" => 0x00D8,
        "oslash" => 0x00F8,
        "Lslash" => 0x0141,
        "lslash" => 0x0142,
        "Zdotaccent" => 0x017B,
        "zdotaccent" => 0x017D,
        "Aacute" => 0x00C1,
        "aacute" => 0x00E1,
        "Cacute" => 0x0106,
        "cacute" => 0x0107,
        "Eacute" => 0x00C9,
        "eacute" => 0x00E9,
        "Eogonek" => 0x0118,
        "eogonek" => 0x0119,
        "Lacute" => 0x0139,
        "lacute" => 0x013A,
        "Nacute" => 0x0143,
        "nacute" => 0x0144,
        "Oacute" => 0x00D3,
        "oacute" => 0x00F3,
        "Sacute" => 0x015A,
        "sacute" => 0x015B,
        "Scedilla" => 0x015E,
        "scedilla" => 0x015F,
        "Zacute" => 0x0179,
        "zacute" => 0x017A,
        "Aogonek" => 0x0104,
        "aogonek" => 0x0105,
        "Ccedilla" => 0x00C7,
        "ccedilla" => 0x00E7,
        "Agrave" => 0x00C0,
        "agrave" => 0x00E0,
        "Acircumflex" => 0x00C2,
        "acircumflex" => 0x00E2,
        "Adieresis" => 0x00C4,
        "adieresis" => 0x00E4,
        "Aring" => 0x00C5,
        "aring" => 0x00E5,
        "Atilde" => 0x00C3,
        "atilde" => 0x00E3,
        "Iacute" => 0x00CD,
        "iacute" => 0x00ED,
        "Ocircumflex" => 0x00D4,
        "ocircumflex" => 0x00F4,
        "Odieresis" => 0x00D6,
        "odieresis" => 0x00F6,
        "Uacute" => 0x00DA,
        "uacute" => 0x00FA,
        "Udieresis" => 0x00DC,
        "udieresis" => 0x00FC,
        "germandbls" => 0x00DF,
        _ => return None,
    };
    char::from_u32(value)
}
