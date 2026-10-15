//! Shared HTML, link and raster-image carrier for document producers.

use std::collections::BTreeMap;

use crate::{Error, Options, markdown, messages::Msg, pdf::PageSetup};
use printpdf::Base64OrRaw;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Image {
    pub src: String,
    pub bytes: Vec<u8>,
    /// Display dimensions in CSS pixels (96 pixels per inch).
    pub width: f64,
    pub height: f64,
}

pub(crate) struct Document {
    pub body: String,
    pub images: BTreeMap<String, Vec<u8>>,
    assets: BTreeMap<String, Image>,
    markdown: Option<String>,
    pub warnings: Vec<Msg>,
    pub setup: PageSetup,
    pub families: Vec<String>,
}

pub(crate) fn fit(width: f64, height: f64, setup: &PageSetup) -> (f64, f64) {
    let max_width = (setup.width_mm - setup.margin_left_mm - setup.margin_right_mm) * 96.0 / 25.4;
    let max_height =
        (setup.height_mm - setup.margin_top_mm - setup.margin_bottom_mm) * 96.0 / 25.4 - 8.0;
    let factor = (max_width / width).min(max_height / height).min(1.0);
    (width * factor, height * factor)
}

impl Document {
    pub fn markdown(source: &str, opts: &Options) -> Result<Self, Error> {
        let mut model = Self::local(&markdown::to_html(source), opts, PageSetup::default())?;
        // Raw HTML images in Markdown share the same asset sequence as ordinary
        // Markdown images, while the original list/headings syntax stays intact.
        let mut edits = Vec::new();
        for (event, range) in
            pulldown_cmark::Parser::new_ext(source, pulldown_cmark::Options::all())
                .into_offset_iter()
        {
            if let pulldown_cmark::Event::Html(html) | pulldown_cmark::Event::InlineHtml(html) =
                event
            {
                edits.extend(
                    crate::htmlpdf::markers::tags(&html)
                        .into_iter()
                        .filter(|tag| tag.name == "img" && !tag.closing)
                        .map(|tag| range.start + tag.range.start..range.start + tag.range.end),
                );
            }
        }
        let mut source = source.to_string();
        for range in edits.into_iter().rev() {
            source.replace_range(range, "![](image)");
        }
        model.markdown = Some(source);
        Ok(model)
    }

    pub fn local(body: &str, opts: &Options, setup: PageSetup) -> Result<Self, Error> {
        let (body, images, warnings) = markdown::embed_local_images_with_setup(
            body,
            &opts.base_dir,
            opts.max_input_bytes,
            &setup,
        )?;
        let images = images
            .into_iter()
            .filter_map(|(src, data)| match data {
                Base64OrRaw::Raw(bytes) => Some((src, bytes)),
                _ => None,
            })
            .collect();
        Ok(Self::embedded(body, images, warnings, setup, Vec::new()))
    }

    pub fn embedded(
        body: String,
        images: BTreeMap<String, Vec<u8>>,
        warnings: Vec<Msg>,
        setup: PageSetup,
        families: Vec<String>,
    ) -> Self {
        let available =
            (setup.width_mm - setup.margin_left_mm - setup.margin_right_mm) * 96.0 / 25.4;
        let mut assets = BTreeMap::new();
        let mut edits = Vec::new();
        for tag in crate::htmlpdf::markers::tags(&body) {
            if tag.name != "img" || tag.closing {
                continue;
            }
            let Some(Some(src)) = tag.attr("src") else {
                continue;
            };
            let Some(bytes) = images.get(src) else {
                continue;
            };
            let raw = &body[tag.range.clone()];
            let (w, h) = markdown::image_size(raw, 1, 1, available);
            let (w, h) = fit(w, h, &setup);
            assets.insert(
                src.to_string(),
                Image {
                    src: src.to_string(),
                    bytes: bytes.clone(),
                    width: w,
                    height: h,
                },
            );
            // Both exporters use exactly the same bounded display dimensions.
            edits.push((
                tag.range.clone(),
                format!("<img src=\"{src}\" style=\"width:{w:.4}px;height:{h:.4}px\"/>"),
            ));
        }
        let mut normalized = body;
        for (range, replacement) in edits.into_iter().rev() {
            normalized.replace_range(range, &replacement);
        }
        Self {
            body: normalized,
            images,
            assets,
            markdown: None,
            warnings,
            setup,
            families,
        }
    }

    pub fn pdf_images(&self) -> BTreeMap<String, Base64OrRaw> {
        self.images
            .iter()
            .map(|(src, image)| (src.clone(), Base64OrRaw::Raw(image.clone())))
            .collect()
    }

    pub fn blocks(&self) -> Result<Vec<markdown::Block>, Error> {
        let text = match &self.markdown {
            Some(text) => text.clone(),
            None => crate::read::html::markdown(&self.body)?,
        };
        let mut blocks = markdown::to_blocks(&text);
        let ordered: Vec<_> = crate::htmlpdf::markers::tags(&self.body)
            .iter()
            .filter(|tag| tag.name == "img" && !tag.closing)
            .map(|tag| {
                tag.attr("src")
                    .flatten()
                    .and_then(|src| self.assets.get(src))
                    .cloned()
            })
            .collect();
        let mut ordered = ordered.into_iter();
        let mut resolve = |inlines: &mut Vec<markdown::Inline>| {
            for inline in inlines.iter_mut() {
                if inline.image.is_some() {
                    inline.image = ordered.next().flatten();
                }
            }
        };
        for block in &mut blocks {
            match block {
                markdown::Block::Heading(_, v)
                | markdown::Block::Paragraph(v)
                | markdown::Block::Quote(v)
                | markdown::Block::ListItem { content: v, .. } => resolve(v),
                markdown::Block::Table { rows, .. } => {
                    for row in rows {
                        for cell in row {
                            resolve(cell);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(blocks)
    }
}
