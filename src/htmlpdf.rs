//! Browser defaults for HTML input, isolated from Word and Markdown layout.
use std::collections::BTreeMap;

use crate::{Error, pdf};
use printpdf::Base64OrRaw;
#[cfg(test)]
use printpdf::PdfSaveOptions;

#[path = "htmlmarkers.rs"]
pub(crate) mod markers;

// HTML-only defaults; author declarations follow this sheet.
const DEFAULTS: &str = "<style>\
    body { font-family:'Liberation Serif'; font-size:12pt; }\
    i, em { font-style:italic; }\
    blockquote { margin:1em 40px; }\
    pre { display:block; white-space:pre; margin:1em 0; }\
    pre, code, kbd, samp { font-family:monospace; font-size:13px; }\
    table { border-collapse:separate; border-spacing:2px; }\
    td, th { padding:1px; }\
    th { font-weight:bold; text-align:center; }\
    </style>";

const NESTED_LIST_DEFAULTS: &str = "<style>\
    ul ul, ol ul { list-style-type:circle; }\
    ul ul ul, ul ol ul, ol ul ul, ol ol ul { list-style-type:square; }\
    ul ul, ul ol, ol ul, ol ol { margin-top:0; margin-bottom:0; }\
    </style>";

// Integral CSS-pixel line boxes of the default serif face in Chromium.
// Do not impose these metrics on documents with author CSS: their font or
// size may differ, and the backend must resolve normal line-height for them.
const UNSTYLED_LINE_BOXES: &str = "<style>\
    body { line-height:1.125; }\
    h1 { line-height:37px; } h2 { line-height:28px; }\
    h3 { line-height:22px; } h4 { line-height:18px; }\
    h5 { line-height:15px; } h6 { line-height:12px; }\
    pre { line-height:normal; }\
    </style>";

pub(crate) fn render(
    html: &str,
    font: &[u8],
    images: &BTreeMap<String, Base64OrRaw>,
) -> Result<Vec<u8>, Error> {
    let setup = page_setup(html);
    let (html, anchor) =
        markers::prepare(&with_defaults(html), markers::has_author_list_style(html));
    let mut document = pdf::html_document(&html, font, images, &setup, &[], &mut Vec::new())?;
    markers::paint(&mut document, &anchor);
    pdf::save_generated(document)
}

fn opening_end(html: &str, name: &str) -> Option<usize> {
    opening_tag(html, |tag, _| tag == name)
}

fn opening_tag(html: &str, matches: impl Fn(&str, &str) -> bool) -> Option<usize> {
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(start) = lower[at..].find('<') {
        at += start;
        if lower[at..].starts_with("<!--") {
            at += lower[at..].find("-->")? + 3;
            continue;
        }
        let mut quote = None;
        let end = html[at..].char_indices().find_map(|(index, ch)| {
            if let Some(q) = quote {
                if ch == q {
                    quote = None;
                }
            } else if ch == '\'' || ch == '"' {
                quote = Some(ch);
            } else if ch == '>' {
                return Some(at + index + 1);
            }
            None
        })?;
        let inside = &lower[at + 1..end - 1];
        let name_end = inside
            .find(|ch: char| ch.is_ascii_whitespace())
            .unwrap_or(inside.len());
        let tag = inside[..name_end].trim_end_matches('/');
        if matches(tag, &inside[name_end..]) {
            return Some(end);
        }
        at = end;
        if matches!(tag, "script" | "style") {
            at += lower[at..].find(&format!("</{tag}"))?;
        }
    }
    None
}

fn has_style_attribute(mut attributes: &str) -> bool {
    while !attributes.is_empty() {
        attributes = attributes.trim_start();
        let end = attributes
            .find(|ch: char| ch.is_ascii_whitespace() || ch == '=')
            .unwrap_or(attributes.len());
        if &attributes[..end] == "style" {
            return true;
        }
        attributes = attributes[end..].trim_start();
        if let Some(value) = attributes.strip_prefix('=') {
            attributes = value.trim_start();
            if let Some(quote) = attributes
                .chars()
                .next()
                .filter(|c| *c == '\'' || *c == '"')
            {
                attributes = &attributes[1..];
                let Some(end) = attributes.find(quote) else {
                    return false;
                };
                attributes = &attributes[end + 1..];
            } else {
                let end = attributes
                    .find(|ch: char| ch.is_ascii_whitespace())
                    .unwrap_or(attributes.len());
                attributes = &attributes[end..];
            }
        }
    }
    false
}

fn with_defaults(html: &str) -> String {
    let unstyled = opening_tag(html, |tag, attributes| {
        matches!(tag, "style" | "link") || has_style_attribute(attributes)
    })
    .is_none();
    // The backend has no separate UA cascade origin. Avoid a more-specific
    // nested default overriding author list declarations; in that case leave
    // list styling to the backend's existing author cascade.
    let lists = if markers::has_author_list_style(html) {
        ""
    } else {
        NESTED_LIST_DEFAULTS
    };
    // Chromium discards the first list's collapsing margin at the page edge.
    // Do not impose this workaround when author box rules might prevent
    // collapsing (padding, borders, display changes or explicit margins).
    let first_list = if plain_box_rules(html) {
        "<style>body > ul:first-child, body > ol:first-child { margin-top:0; }</style>"
    } else {
        ""
    };
    let defaults = if unstyled {
        format!("{DEFAULTS}{lists}{first_list}{UNSTYLED_LINE_BOXES}")
    } else {
        format!("{DEFAULTS}{lists}{first_list}")
    };
    if let Some(at) = opening_end(html, "head") {
        format!("{}{defaults}{}", &html[..at], &html[at..])
    } else if let Some(at) = opening_end(html, "html") {
        format!("{}<head>{defaults}</head>{}", &html[..at], &html[at..])
    } else {
        format!("<html><head>{defaults}</head><body>{html}</body></html>")
    }
}

// This conservative eligibility check only enables a workaround; ambiguous
// author CSS leaves the original backend layout untouched.
fn plain_box_rules(html: &str) -> bool {
    if opening_tag(html, |_, attrs| has_style_attribute(attrs)).is_some() {
        return false;
    }
    let lower = html.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(start) = opening_end(rest, "style") {
        rest = &rest[start..];
        let Some(end) = rest.find("</style") else {
            return false;
        };
        let css = without_comments(&rest[..end]);
        for part in css.split(['{', '}', ';']) {
            if let Some((key, _)) = part.split_once(':') {
                let key = key.trim();
                if [
                    "margin", "padding", "border", "display", "position", "float", "overflow",
                ]
                .iter()
                .any(|prefix| key.starts_with(prefix))
                {
                    return false;
                }
            }
        }
        rest = &rest[end + 7..];
    }
    true
}

// Keep quoted CSS strings opaque while finding comments and rule boundaries.
fn without_comments(css: &str) -> String {
    let mut out = String::new();
    let mut chars = css.chars().peekable();
    let mut quote = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            out.push(ch);
            if ch == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if ch == q {
                quote = None;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            out.push(ch);
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(ch) = chars.next() {
                if ch == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

fn page_rules(css: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut chars = css.char_indices();
    let mut quote = None;
    let mut depth = 0usize;
    let mut start = 0;
    let mut body = None;
    while let Some((at, ch)) = chars.next() {
        if let Some(q) = quote {
            if ch == '\\' {
                chars.next();
            } else if ch == q {
                quote = None;
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch == '{' {
            if depth == 0 && css[start..at].trim() == "@page" {
                body = Some(at + 1);
            }
            depth += 1;
        } else if ch == '}' && depth > 0 {
            depth -= 1;
            if depth == 0 {
                if let Some(body) = body.take() {
                    out.push(&css[body..at]);
                }
                start = at + 1;
            }
        } else if ch == ';' && depth == 0 {
            start = at + 1;
        }
    }
    out
}

fn length_mm(value: &str) -> Option<f64> {
    for (unit, factor) in [
        ("mm", 1.0),
        ("cm", 10.0),
        ("in", 25.4),
        ("pt", 25.4 / 72.0),
        ("px", 25.4 / 96.0),
    ] {
        if let Some(n) = value.strip_suffix(unit) {
            let n = n.trim().parse::<f64>().ok()? * factor;
            return (n.is_finite() && (0.0..=5080.0).contains(&n)).then_some(n);
        }
    }
    (value == "0").then_some(0.0)
}

pub(crate) fn page_setup(html: &str) -> pdf::PageSetup {
    // Chromium's default print sheet is Letter with 1 cm margins. Only
    // unqualified @page rules are applied; named/pseudo pages remain outside
    // this flow renderer's supported CSS subset.
    let mut setup = pdf::PageSetup {
        width_mm: 215.9,
        height_mm: 279.4,
        margin_top_mm: 10.0,
        margin_right_mm: 10.0,
        margin_bottom_mm: 10.0,
        margin_left_mm: 10.0,
        ..pdf::PageSetup::default()
    };
    let lower = html.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(start) = opening_end(rest, "style") {
        rest = &rest[start..];
        let Some(end) = rest.find("</style") else {
            break;
        };
        let css = without_comments(&rest[..end]);
        for body in page_rules(&css) {
            for declaration in body.split(';') {
                let Some((key, value)) = declaration.split_once(':') else {
                    continue;
                };
                let values: Vec<_> = value.split_whitespace().collect();
                match key.trim() {
                    "size" => {
                        let landscape = values.contains(&"landscape");
                        let size = if values.contains(&"a4") {
                            Some((210.0, 297.0))
                        } else if values.contains(&"letter") {
                            Some((215.9, 279.4))
                        } else if values.len() == 2 {
                            length_mm(values[0]).zip(length_mm(values[1]))
                        } else {
                            None
                        };
                        if let Some((mut w, mut h)) = size.filter(|(w, h)| *w > 0.0 && *h > 0.0) {
                            if landscape {
                                std::mem::swap(&mut w, &mut h);
                            }
                            setup.width_mm = w;
                            setup.height_mm = h;
                        }
                    }
                    "margin" if (1..=4).contains(&values.len()) => {
                        if let Some(v) = values
                            .iter()
                            .map(|v| length_mm(v))
                            .collect::<Option<Vec<_>>>()
                        {
                            setup.margin_top_mm = v[0];
                            setup.margin_right_mm = v[1 % v.len()];
                            setup.margin_bottom_mm = v[if v.len() > 2 { 2 } else { 0 }];
                            setup.margin_left_mm = v[if v.len() == 4 { 3 } else { 1 % v.len() }];
                        }
                    }
                    key => {
                        if let Some(v) = length_mm(value.trim()) {
                            match key {
                                "margin-top" => setup.margin_top_mm = v,
                                "margin-right" => setup.margin_right_mm = v,
                                "margin-bottom" => setup.margin_bottom_mm = v,
                                "margin-left" => setup.margin_left_mm = v,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        rest = &rest[end..];
    }
    setup
}

#[cfg(test)]
#[path = "htmlgeometry_tests.rs"]
mod geometry_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_rules_ignore_comments_strings_and_non_style_content() {
        let html = r#"<html><head><!-- <style>@page {size:1pt 1pt}</style> -->
            <script>let fake = '<style>@page {size:2pt 2pt}</style>';</script>
            <style>/* @page {size:3pt 3pt} */
            p::before {content:'@page {size:4pt 4pt}';}
            @page named {size:5pt 5pt}
            @page {size:A4 landscape; margin:1cm 2cm 3cm 4cm; margin-left:15mm;}
            </style></head><body>Text</body></html>"#;
        let page = page_setup(html);
        assert_eq!((page.width_mm, page.height_mm), (297.0, 210.0));
        assert_eq!(
            [
                page.margin_top_mm,
                page.margin_right_mm,
                page.margin_bottom_mm,
                page.margin_left_mm
            ],
            [10.0, 20.0, 30.0, 15.0]
        );
        let with = with_defaults(html);
        assert!(with.starts_with(&format!("<html><head>{DEFAULTS}")));
    }

    #[test]
    fn unstyled_metrics_do_not_override_author_fonts() {
        for html in [
            "<p style='font-size:20pt'>Text</p>",
            "<p STYLE \n = 'font-size:20pt'>Text</p>",
            "<style>body {font-family:serif}</style><p>Text</p>",
            "<link rel='stylesheet' href='theme.css'><p>Text</p>",
        ] {
            assert!(!with_defaults(html).contains(UNSTYLED_LINE_BOXES), "{html}");
        }
        for html in [
            "<p>Text</p>",
            "<!-- <p style='color:red'> --><p>Text</p>",
            "<script>const s = '<p style=\"color:red\">';</script><p>Text</p>",
            "<p title='style=\"font-size:20pt\"'>Text</p>",
        ] {
            assert!(with_defaults(html).contains(UNSTYLED_LINE_BOXES), "{html}");
        }
    }

    #[test]
    fn initial_list_workaround_does_not_override_author_box_rules() {
        assert!(plain_box_rules(
            "<style>body{font-size:24pt;line-height:36px}</style><ul><li>Text</li></ul>"
        ));
        for html in [
            "<style>body{padding-top:20px}</style><ul><li>Text</li></ul>",
            "<style>ul{margin-top:100px}</style><ul><li>Text</li></ul>",
            "<body style='border:1px solid'><ul><li>Text</li></ul></body>",
        ] {
            assert!(!plain_box_rules(html), "{html}");
            assert!(
                !with_defaults(html).contains("body > ul:first-child"),
                "{html}"
            );
        }
    }

    #[test]
    fn page_lengths_reject_nonfinite_and_unbounded_values() {
        assert_eq!(length_mm("72pt"), Some(25.4));
        assert_eq!(length_mm("-1mm"), None);
        assert_eq!(length_mm("NaNmm"), None);
        assert_eq!(length_mm("1e300mm"), None);
    }
}
