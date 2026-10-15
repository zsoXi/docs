//! Bounds shared by PDF loading and all subsequent stream consumers.
use lopdf::{Document, LoadOptions, Object, Stream};

use crate::{
    Error,
    messages::{MessageCode, params},
};

pub(crate) const STREAM_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const OBJECTS: usize = 100_000;
pub(crate) const DEPTH: usize = 32;

#[path = "pdfpreflight.rs"]
mod preflight;

pub(crate) fn limit(kind: &str, maximum: usize) -> Error {
    Error::invalid(
        MessageCode::PdfContentLimit,
        params(&[("kind", kind.into()), ("limit", maximum.into())]),
    )
}

fn decode_error(error: lopdf::Error) -> Error {
    match error {
        lopdf::Error::Decompress(lopdf::DecompressError::MemoryLimitExceeded {
            limit: maximum,
        }) => limit("decompressed_bytes", maximum),
        other => other.into(),
    }
}

/// The only stream decoding gateway. Admission metadata is checked before
/// invoking bounded codec primitives; callers never use unbounded decoding.
fn decode(stream: &Stream, maximum: usize) -> Result<Vec<u8>, Error> {
    check_predictors(&stream.dict, maximum)?;
    if stream.content.len() > STREAM_BYTES {
        return Err(limit("encoded_bytes", STREAM_BYTES));
    }
    if stream.dict.has(b"Filter") {
        let filters = stream.filters()?;
        if filters.len() > DEPTH {
            return Err(limit("filters", DEPTH));
        }
        for filter in filters {
            if !matches!(
                filter,
                b"FlateDecode"
                    | b"LZWDecode"
                    | b"ASCII85Decode"
                    | b"ASCIIHexDecode"
                    | b"RunLengthDecode"
                    | b"BrotliDecode"
                    | b"DCTDecode"
                    | b"JPXDecode"
                    | b"CCITTFaxDecode"
                    | b"JBIG2Decode"
            ) {
                return Err(lopdf::Error::Parse(lopdf::ParseError::InvalidContentStream).into());
            }
        }
    }
    stream
        .get_plain_content_with_limit(maximum)
        .map_err(decode_error)
}

/// Predictor work buffers must also fit the granted budget, even when the
/// compressed payload is tiny. Check arithmetic before a codec sees metadata.
fn check_predictors(dict: &lopdf::Dictionary, maximum: usize) -> Result<(), Error> {
    let Ok(value) = dict.get(b"DecodeParms") else {
        return Ok(());
    };
    let values = match value {
        Object::Array(items) => items.as_slice(),
        _ => std::slice::from_ref(value),
    };
    for value in values {
        if matches!(value, Object::Null) {
            continue;
        }
        let settings = value.as_dict()?;
        let predictor = settings
            .get(b"Predictor")
            .and_then(Object::as_i64)
            .unwrap_or(1);
        if predictor == 1 {
            continue;
        }
        let positive = |key: &[u8], default| -> Result<usize, Error> {
            let n = match settings.get(key) {
                Ok(value) => value.as_i64()?,
                Err(_) => default,
            };
            usize::try_from(n)
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| limit("predictor_bytes", maximum))
        };
        let columns = positive(b"Columns", 1)?;
        let colors = positive(b"Colors", 1)?;
        let bits = positive(b"BitsPerComponent", 8)?;
        let row_bits = columns
            .checked_mul(colors)
            .and_then(|n| n.checked_mul(bits));
        if !matches!(bits, 1 | 2 | 4 | 8 | 16)
            || row_bits.is_none_or(|n| n.div_ceil(8) > maximum)
            || colors.checked_mul(2).is_none_or(|n| n > maximum)
        {
            return Err(limit("predictor_bytes", maximum));
        }
    }
    Ok(())
}

pub(crate) fn stream(stream: &Stream) -> Result<Vec<u8>, Error> {
    decode(stream, STREAM_BYTES)
}

/// Bound eager object/xref decoding too, then validate the document-wide sum
/// before handing any PDF to a font, layout, text or editing consumer.
pub(crate) fn load(bytes: &[u8]) -> Result<Document, Error> {
    preflight::check(bytes)?;
    let doc = Document::load_mem_with_options(
        bytes,
        LoadOptions {
            strict: true,
            max_decompressed_size: Some(STREAM_BYTES),
            ..LoadOptions::default()
        },
    )
    .map_err(decode_error)?;
    validate(&doc)?;
    Ok(doc)
}

pub(crate) fn validate(doc: &Document) -> Result<(), Error> {
    if doc.objects.len() > OBJECTS {
        return Err(limit("objects", OBJECTS));
    }
    let mut total = 0usize;
    let mut count = 0usize;
    let mut pending: Vec<_> = doc
        .objects
        .values()
        .map(|object| (object, 0usize))
        .collect();
    while let Some((object, depth)) = pending.pop() {
        count += 1;
        if count > OBJECTS {
            return Err(limit("objects", OBJECTS));
        }
        if depth > DEPTH {
            return Err(limit("object_depth", DEPTH));
        }
        match object {
            Object::Array(items) => pending.extend(items.iter().map(|item| (item, depth + 1))),
            Object::Dictionary(dict) => {
                pending.extend(dict.iter().map(|(_, item)| (item, depth + 1)))
            }
            Object::Stream(value) => {
                let maximum = STREAM_BYTES.min(DOCUMENT_BYTES.saturating_sub(total));
                // JPEG/JPX/CCITT codecs are handled by image consumers, not
                // lopdf's general-purpose stream decoder. Never swallow a
                // memory-limit or malformed supported-filter failure.
                let size = match decode(value, maximum) {
                    Ok(data) => data.len(),
                    Err(Error::Pdf(lopdf::Error::Unimplemented(_))) => value.content.len(),
                    Err(error) => return Err(error),
                };
                total = total
                    .checked_add(size)
                    .ok_or_else(|| limit("document_bytes", DOCUMENT_BYTES))?;
                if total > DOCUMENT_BYTES {
                    return Err(limit("document_bytes", DOCUMENT_BYTES));
                }
                pending.extend(value.dict.iter().map(|(_, item)| (item, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_gateway_bounds_raw_flate_and_chained_filters()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = vec![b'A'; 1024 * 1024];
        let raw = Stream::new(lopdf::dictionary! {}, bytes.clone());
        let mut flate = raw.clone();
        flate.compress()?;
        let mut chained = Stream::new(lopdf::dictionary! {}, flate.content.clone());
        chained.compress()?;
        chained.dict.set(
            "Filter",
            vec![
                Object::Name(b"FlateDecode".to_vec()),
                Object::Name(b"FlateDecode".to_vec()),
            ],
        );
        for stream in [raw, flate, chained] {
            let error = decode(&stream, 1024)
                .err()
                .ok_or("accepted excessive decoded output")?;
            assert_eq!(error.code(), MessageCode::PdfContentLimit);
            assert_eq!(error.params()["limit"], 1024);
            assert_eq!(decode(&stream, bytes.len())?, bytes);
        }
        Ok(())
    }
}
