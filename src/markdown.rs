//! Markdown parsing shared by the DOCX and PDF writers.

use std::collections::BTreeMap;
use std::path::Path;

use printpdf::Base64OrRaw;
use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::messages::{MessageCode, Msg, params};

/// A run of text with its inline formatting.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Inline {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub link: Option<String>,
    pub code: bool,
    pub image: Option<crate::document::Image>,
}

/// A top-level block of a document.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Heading(u8, Vec<Inline>),
    Paragraph(Vec<Inline>),
    ListItem {
        ordered: bool,
        number: u64,
        level: usize,
        list: usize,
        content: Vec<Inline>,
    },
    Table {
        rows: Vec<Vec<Vec<Inline>>>,
        alignments: Vec<Alignment>,
    },
    Quote(Vec<Inline>),
    Code(String),
    Rule,
}

fn options() -> Options {
    let mut opts = Options::all();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts
}

/// Font family name under which the embedded TTF is registered.
pub const FONT_FAMILY: &str = "DokuFont";

/// Wrap an HTML fragment into a full document that uses the embedded font.
/// Markdown's scoped rules match the generated Word document: 15 pt body
/// lines, quarter-point table borders and 5.4 pt cell side margins. The HTML
/// backend counts both collapsed borders, so subtract one from the 6 pt
/// paragraph space below each cell's text.
pub fn full_html(body: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><style>\
         body {{ margin: 0; font-family: \"{FONT_FAMILY}\"; font-size: 11pt; line-height: 1.36; }}\
         p {{ margin: 0 0 6pt; }}\n\
         h1, h2, h3 {{ margin: 0 0 6pt; }}\n\
         h1 {{ font-size: 20pt; font-weight: bold; }}\n h2 {{ font-size: 16pt; font-weight: bold; }}\n h3 {{ font-size: 13pt; font-weight: bold; }}\
         em {{ font-style: italic; }}\n\
         strong {{ font-weight: bold; }}\n\
         code, pre {{ font-family: monospace; }}\n\
         table {{ border-collapse: collapse; width: 100%; table-layout: fixed; }}\n\
         td, th {{ border: 1px solid #333; padding: 4px; }}\
         th {{ font-weight: bold; text-align: left; background-color: #e8edf2; }}\n\
         blockquote {{ margin: 0 0 6pt 18pt; font-style: italic; color: #666666; }}\n\
         pre {{ margin: 0 0 6pt; background-color: #f2f2f2; padding: 0; }}\n\
         a {{ color: #0645ad; }}\
         .markdown {{ line-height: 1.363636; }}\
         .markdown table {{ margin: 0; width: 100%; }}\
         .markdown td, .markdown th {{ border: 0.333333px solid #000; padding: 0 5.4pt 5.75pt; }}\
         .markdown pre {{ line-height: 15pt; }}\
         </style></head><body>{body}</body></html>"
    )
}

/// Bound recursive consumers before building HTML or a document model.
pub(crate) fn validate(source: &str) -> Result<(), crate::Error> {
    let mut html_depth = 0usize;
    for (count, tag) in crate::htmlpdf::markers::tags(source).into_iter().enumerate() {
        if tag.closing { html_depth=html_depth.saturating_sub(1); }
        else if !matches!(tag.name.as_str(), "img"|"br"|"hr"|"meta"|"link"|"input"|"source"|"wbr"|"area"|"base"|"col"|"embed"|"param"|"track") && !source[tag.range.clone()].ends_with("/>") { html_depth+=1; }
        if html_depth > 64 || count >= 100_000 {
            return Err(crate::Error::invalid(MessageCode::MarkdownLimit, params(&[("kind","HTML nesting or tags".into()),("limit",64.into())])));
        }
    }
    let mut depth = 0usize;
    for (count, event) in Parser::new_ext(source, options()).enumerate() {
        if matches!(event, Event::Start(_)) { depth += 1; }
        if depth > 64 || count >= 100_000 {
            return Err(crate::Error::invalid(MessageCode::MarkdownLimit, params(&[
                ("kind", if depth > 64 { "nesting" } else { "events" }.into()),
                ("limit", if depth > 64 { 64 } else { 100_000 }.into()),
            ])));
        }
        if matches!(event, Event::End(_)) { depth = depth.saturating_sub(1); }
    }
    Ok(())
}

/// Render markdown to an HTML fragment.
pub fn to_html(markdown: &str) -> String {
    // Keep list items as sibling boxes: nested HTML lists make the layout
    // engine paint the parent's text twice. Word uses the same flat sequence
    // with explicit numbering levels.
    let mut events = Vec::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut item_open = false;
    let mut in_code = false;
    let mut trailing_space = false;
    for event in Parser::new_ext(markdown, options()) {
        if trailing_space
            && matches!(
                event,
                Event::Start(Tag::Strong | Tag::Emphasis | Tag::Link { .. })
            )
        {
            // Writer reserves the preserved trailing space at a run boundary
            // separately from the following face's advance.
            events.push(Event::Html("&#160;".into()));
        }
        trailing_space = matches!(&event, Event::Text(text) if text.ends_with(' '));
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                in_code = true;
                // A second inline code box adds its own baseline strut. Keep
                // the monospace lines in one shaded block instead.
                events.push(Event::Html("<pre style=\"line-height:1.363636\">".into()));
            }
            Event::End(TagEnd::CodeBlock) => {
                in_code = false;
                events.push(Event::Html("</pre>".into()));
            }
            Event::Text(text) if in_code => {
                for line in text.trim_end_matches('\n').split('\n') {
                    let mut escaped = String::new();
                    push_escaped(&mut escaped, line);
                    // Fixed line boxes avoid the HTML backend's extra strut
                    // for a br inside a preformatted line.
                    events.push(Event::Html(
                        format!("<div style=\"height:15pt;line-height:15pt\">{escaped}</div>")
                            .into(),
                    ));
                }
            }
            Event::Start(Tag::List(start)) => {
                if item_open {
                    events.push(Event::Html("</div>".into()));
                    item_open = false;
                }
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Item) => {
                let marker = match lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}.");
                        *n += 1;
                        marker
                    }
                    _ => "•".into(),
                };
                // Word's generated numbering uses a hanging marker and a tab
                // before the text, not a space following an indented marker.
                let indent = lists.len().saturating_sub(1) * 27;
                let marker_width = if lists.last().is_some_and(Option::is_some) {
                    21
                } else {
                    9
                };
                events.push(Event::Html(
                    format!("<div style=\"margin-left:{indent}pt;margin-bottom:6pt\"><span style=\"display:inline-block;vertical-align:top;width:{marker_width}pt\">{marker}</span>")
                        .into(),
                ));
                item_open = true;
            }
            Event::End(TagEnd::Item) => {
                if item_open {
                    events.push(Event::Html("</div>".into()));
                    item_open = false;
                }
            }
            // Loose list paragraphs must not nest a paragraph box in the item.
            Event::Start(Tag::Paragraph) | Event::End(TagEnd::Paragraph) if !lists.is_empty() => {}
            other => events.push(other),
        }
    }
    let mut html = String::from("<div class=\"markdown\">");
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    html.push_str("</div>");
    html
}

/// Render plain text (non-markdown) to an HTML fragment, one paragraph per line.
pub fn text_to_html(text: &str) -> String {
    let mut html = String::new();
    let mut wrote = false;
    for line in text.lines() {
        html.push_str("<p>");
        push_escaped(&mut html, line);
        html.push_str("</p>");
        wrote = true;
    }
    if !wrote {
        html.push_str("<p></p>");
    }
    html
}

fn push_escaped(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
}

type EmbeddedImages = (String, BTreeMap<String, Base64OrRaw>, Vec<Msg>);

/// Size images against the geometry used by the caller's renderer.
pub(crate) fn embed_local_images_with_setup(
    body: &str,
    base: &Path,
    max: u64,
    setup: &crate::pdf::PageSetup,
) -> Result<EmbeddedImages, crate::Error> {
    let mut out = String::new();
    let mut images = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut at = 0;
    let mut counter = 0usize;
    for image_tag in crate::htmlpdf::markers::tags(body) {
        if image_tag.name != "img" || image_tag.closing { continue; }
        out.push_str(&body[at..image_tag.range.start]);
        let tag = &body[image_tag.range.clone()];
        at = image_tag.range.end;
        let Some(src) = attribute(tag, "src") else {
            out.push_str(tag);
            continue;
        };
        if src.starts_with("http://") || src.starts_with("https://") || src.starts_with("data:") {
            out.push_str(tag);
            warnings.push(Msg::with(
                MessageCode::ImageRemoteNotFetched,
                params(&[("src", format!("{src:?}").into())]),
            ));

            continue;
        }
        let path = crate::path::resolve(base, &src)?;
        let loaded = load_image(&path, &src, max)?;
        match loaded {
            Some((bytes, width, height)) => {
                let name = format!("local{counter}");
                counter += 1;
                images.insert(name.clone(), Base64OrRaw::Raw(bytes));
                // Author sizes are CSS pixels, not raster pixels. Constrain
                // against this document's printable page, preserving the ratio.
                let max_width = (setup.width_mm - setup.margin_left_mm - setup.margin_right_mm) * 96.0 / 25.4;
                let (width, height) = image_size(tag, width, height, max_width);
                let (width, height) = crate::document::fit(width, height, setup);
                if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
                    return Err(crate::Error::invalid(
                        MessageCode::ImageUnreadable,
                        params(&[("src", format!("{src:?}").into())]),
                    ));
                }
                out.push_str(&format!(
                    "<span style=\"display:inline-block;vertical-align:top;width:{width:.4}px;height:{height:.4}px;break-inside:avoid;page-break-inside:avoid\"><img src=\"{name}\" style=\"width:{width:.4}px;height:{height:.4}px\" /></span>"
                ));
            }
            None => {
                warnings.push(Msg::with(
                    MessageCode::ImageUnreadable,
                    params(&[("src", format!("{src:?}").into())]),
                ));

                out.push_str(tag);
                let fallback = attribute(tag, "alt").filter(|alt| !alt.is_empty()).unwrap_or_else(||
                    crate::messages::render(MessageCode::ImageUnreadable,
                        &params(&[("src", format!("{src:?}").into())]), "en"));
                push_escaped(&mut out, &fallback);
            }
        }
    }
    out.push_str(&body[at..]);
    Ok((out, images, warnings))
}

/// Resolve inline CSS over HTML attributes; an omitted axis keeps raster aspect.
pub(crate) fn image_size(tag: &str, width: u32, height: u32, available: f64) -> (f64, f64) {
    let style = attribute(tag, "style").unwrap_or_default();
    let dimension = |name: &str| {
        let css = style.split(';').filter_map(|s| s.split_once(':'))
            .rfind(|(key, _)| key.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim().to_string());
        css.or_else(|| attribute(tag, name)).and_then(|value| {
            let value = value.trim();
            let (number, factor) = if let Some(n) = value.strip_suffix("px") { (n, 1.0) }
                else if let Some(n) = value.strip_suffix("pt") { (n, 96.0 / 72.0) }
                else if let Some(n) = value.strip_suffix("mm") { (n, 96.0 / 25.4) }
                else if let Some(n) = value.strip_suffix("cm") { (n, 96.0 / 2.54) }
                else if let Some(n) = value.strip_suffix("in") { (n, 96.0) }
                else if let Some(n) = value.strip_suffix('%') { (n, available / 100.0) }
                else { (value, 1.0) };
            number.trim().parse::<f64>().ok().map(|n| n * factor)
                .filter(|n| n.is_finite() && *n > 0.0)
        })
    };
    match (dimension("width"), dimension("height")) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, w * f64::from(height) / f64::from(width)),
        (None, Some(h)) => (h * f64::from(width) / f64::from(height), h),
        _ => (f64::from(width), f64::from(height)),
    }
}

fn load_image(
    path: &Path,
    rel: &str,
    max: u64,
) -> Result<Option<(Vec<u8>, u32, u32)>, crate::Error> {
    let bytes = match crate::path::read_limited(path, rel, max) {
        Ok(bytes) => bytes,
        // Missing/unreadable local images are a documented non-fatal omission.
        // Path, type and limit violations must retain the common error code.
        Err(crate::Error::Read { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let decoded = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.decode().ok());
    let Some(decoded) = decoded else { return Ok(None) };
    let (width, height) = (decoded.width(), decoded.height());
    let mut png = std::io::Cursor::new(Vec::new());
    decoded.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|source| crate::Error::reader(MessageCode::ImageUnreadable, source))?;
    Ok(Some((png.into_inner(), width, height)))
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    crate::htmlpdf::markers::tags(tag).first()?.attr(name)?.map(str::to_string)
}

#[derive(Default)]
struct TableState {
    rows: Vec<Vec<Vec<Inline>>>,
    row: Vec<Vec<Inline>>,
    alignments: Vec<Alignment>,
}

/// Parse markdown into the block model used to build DOCX files.
pub fn to_blocks(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<Inline>> = None;
    let mut fmt = Fmt::default();
    let mut heading: Option<u8> = None;
    let mut code: Option<String> = None;
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut counters: Vec<u64> = Vec::new();
    let mut item_stack = Vec::new();
    let mut list_ids = Vec::new();
    // docx-rs reserves ID 1 for its built-in numbering definition.
    let mut next_list = 1;
    let mut quote_depth = 0;
    let mut table: Option<TableState> = None;
    let mut cell: Option<Vec<Inline>> = None;
    let mut image = false;

    let flush = |current: &mut Option<Vec<Inline>>, blocks: &mut Vec<Block>, quote: bool| {
        if let Some(inlines) = current.take()
            && !inlines.is_empty()
        {
            blocks.push(if quote {
                Block::Quote(inlines)
            } else {
                Block::Paragraph(inlines)
            });
        }
    };
    let flush_item = |current: &mut Option<Vec<Inline>>,
                      blocks: &mut Vec<Block>,
                      lists: &[Option<u64>],
                      counters: &[u64],
                      ids: &[usize]| {
        if let Some(content) = current.take()
            && !content.is_empty()
        {
            blocks.push(Block::ListItem {
                ordered: lists.last().copied().flatten().is_some(),
                number: counters.last().copied().unwrap_or(1),
                level: lists.len().saturating_sub(1),
                list: ids.last().copied().unwrap_or(0),
                content,
            });
        }
    };

    for event in Parser::new_ext(markdown, options()) {
        match event {
            Event::Start(Tag::Paragraph) => {
                if current.is_none() {
                    current = Some(Vec::new());
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if item_stack.is_empty() {
                    flush(&mut current, &mut blocks, quote_depth > 0);
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                heading = Some(heading_level(level));
                current = Some(Vec::new());
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(inlines) = current.take() {
                    let level = heading.take().unwrap_or(1);
                    if !inlines.is_empty() {
                        blocks.push(Block::Heading(level, inlines));
                    }
                }
            }
            Event::Start(Tag::Image { dest_url, .. }) => {
                let mut inline = Inline { link: fmt.link.clone(), ..Inline::default() };
                inline.image = Some(crate::document::Image {
                    src: dest_url.to_string(), bytes: Vec::new(), width: 0.0, height: 0.0,
                });
                cell.as_mut().unwrap_or_else(|| current.get_or_insert_with(Vec::new)).push(inline);
                image = true;
            }
            Event::End(TagEnd::Image) => image = false,
            Event::Start(Tag::Strong) => fmt.bold = true,
            Event::End(TagEnd::Strong) => fmt.bold = false,
            Event::Start(Tag::Emphasis) => fmt.italic = true,
            Event::End(TagEnd::Emphasis) => fmt.italic = false,
            Event::Start(Tag::Link { dest_url, .. }) => fmt.link = Some(dest_url.to_string()),
            Event::End(TagEnd::Link) => fmt.link = None,
            Event::Start(Tag::CodeBlock(_)) => code = Some(String::new()),
            Event::End(TagEnd::CodeBlock) => {
                if let Some(text) = code.take() {
                    blocks.push(Block::Code(text.trim_end().to_string()));
                }
            }
            Event::Start(Tag::BlockQuote(_)) => quote_depth += 1,
            Event::End(TagEnd::BlockQuote(_)) => quote_depth -= 1,
            Event::Start(Tag::List(start)) => {
                flush_item(&mut current, &mut blocks, &lists, &counters, &list_ids);
                lists.push(start);
                counters.push(start.unwrap_or(1));
                next_list += 1;
                list_ids.push(next_list);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
                counters.pop();
                list_ids.pop();
            }
            Event::Start(Tag::Item) => {
                item_stack.push(());
                current = Some(Vec::new());
            }
            Event::End(TagEnd::Item) => {
                flush_item(&mut current, &mut blocks, &lists, &counters, &list_ids);
                item_stack.pop();
                if let Some(last) = counters.last_mut() {
                    *last += 1;
                }
            }
            Event::Start(Tag::Table(alignments)) => {
                table = Some(TableState {
                    alignments,
                    ..TableState::default()
                })
            }
            Event::Start(Tag::TableRow) => {
                if let Some(state) = table.as_mut() {
                    state.row = Vec::new();
                }
            }
            Event::End(TagEnd::TableRow | TagEnd::TableHead) => {
                if let Some(state) = table.as_mut() {
                    let row = std::mem::take(&mut state.row);
                    state.rows.push(row);
                }
            }
            Event::Start(Tag::TableCell) => cell = Some(Vec::new()),
            Event::End(TagEnd::TableCell) => {
                if let (Some(state), Some(cell_inlines)) = (table.as_mut(), cell.take()) {
                    state.row.push(cell_inlines);
                }
            }
            Event::End(TagEnd::Table) => {
                if let Some(state) = table.take()
                    && !state.rows.is_empty()
                {
                    blocks.push(Block::Table {
                        rows: state.rows,
                        alignments: state.alignments,
                    });
                }
            }
            Event::Text(_) if image => {}
            Event::Text(text) => {
                if let Some(buffer) = code.as_mut() {
                    buffer.push_str(&text);
                } else {
                    push_inline(&mut current, &mut cell, &text, &fmt);
                }
            }
            Event::Code(text) => {
                let mut code_fmt = fmt.clone();
                code_fmt.code = true;
                push_inline(&mut current, &mut cell, &text, &code_fmt);
            }
            Event::SoftBreak | Event::HardBreak => {
                push_inline(&mut current, &mut cell, " ", &fmt);
            }
            Event::TaskListMarker(checked) => {
                let marker = if checked { "[x] " } else { "[ ] " };
                push_inline(&mut current, &mut cell, marker, &fmt);
            }
            Event::Rule => blocks.push(Block::Rule),
            Event::Html(text) | Event::InlineHtml(text) => {
                push_inline(&mut current, &mut cell, &text, &fmt);
            }
            _ => {}
        }
    }

    // Flush a trailing block that had no explicit end (defensive).
    if let Some(inlines) = current.take()
        && !inlines.is_empty()
    {
        blocks.push(Block::Paragraph(inlines));
    }
    blocks
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[derive(Clone, Default)]
struct Fmt {
    bold: bool,
    italic: bool,
    link: Option<String>,
    code: bool,
}

fn push_inline(
    current: &mut Option<Vec<Inline>>,
    cell: &mut Option<Vec<Inline>>,
    text: &str,
    fmt: &Fmt,
) {
    let inline = Inline {
        text: text.to_string(),
        bold: fmt.bold,
        italic: fmt.italic,
        link: fmt.link.clone(),
        code: fmt.code,
        image: None,
    };
    if let Some(cell_inlines) = cell.as_mut() {
        cell_inlines.push(inline);
        return;
    }
    match current.as_mut() {
        Some(inlines) => inlines.push(inline),
        None => *current = Some(vec![inline]),
    }
}
