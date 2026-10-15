//! Exact numeric lexemes, never parsed through floating point or reformatted.
use std::collections::BTreeMap;

/// Retain signs, grouping, currency, percentages, dates and long identifiers.
/// Whitespace remains a separator; no locale-dependent normalization is applied.
pub(crate) fn tokens(text: &str) -> BTreeMap<String, usize> {
    let mut tokens = BTreeMap::new();
    for part in text.split(|c: char| !(c.is_ascii_digit() || "$€£¥.,%()+-/:".contains(c))) {
        let part = part.trim_matches([',', ':', '/']).trim_end_matches('.');
        if part.chars().any(|c| c.is_ascii_digit()) {
            *tokens.entry(part.to_string()).or_default() += 1;
        }
    }
    tokens
}
