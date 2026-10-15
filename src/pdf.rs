//! PDF creation (printpdf), text extraction (pdf-extract) and page surgery (lopdf).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use lopdf::{Document, Object, ObjectId, dictionary};
use printpdf::{Base64OrRaw, GeneratePdfOptions, PdfDocument, PdfSaveOptions};

use crate::error::Error;
use crate::markdown::{FONT_FAMILY, text_to_html};
use crate::messages::{MessageCode, Params, params};
use crate::pdfedit::SubstituteFonts;

/// System fonts used when [`crate::Options::font_path`] is not set.
pub const FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

/// Load the font used to build PDFs.
pub fn load_font(font_path: Option<&Path>) -> Result<Vec<u8>, Error> {
    if let Some(path) = font_path {
        return std::fs::read(path).map_err(|source| Error::FontRead {
            path: path.to_path_buf(),
            source,
        });
    }
    for candidate in FONT_CANDIDATES {
        let path = Path::new(candidate);
        if path.is_file() {
            return std::fs::read(path).map_err(|source| Error::FontRead {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    Err(Error::FontNotFound)
}

/// System font files matching a style, in the same directories as
/// [`FONT_CANDIDATES`]. Bold text gets the bold face, italic the italic face.
fn font_candidates(bold: bool, italic: bool) -> Vec<String> {
    let (noto, dejavu) = match (bold, italic) {
        (true, true) => ("NotoSans-BoldItalic.ttf", "DejaVuSans-BoldOblique.ttf"),
        (true, false) => ("NotoSans-Bold.ttf", "DejaVuSans-Bold.ttf"),
        (false, true) => ("NotoSans-Italic.ttf", "DejaVuSans-Oblique.ttf"),
        (false, false) => ("NotoSans-Regular.ttf", "DejaVuSans.ttf"),
    };
    [
        "/usr/share/fonts/noto/",
        "/usr/share/fonts/TTF/",
        "/usr/share/fonts/truetype/dejavu/",
    ]
    .iter()
    .map(|dir| {
        let file = if dir.contains("noto") { noto } else { dejavu };
        format!("{dir}{file}")
    })
    .collect()
}

/// Load one styled font, falling back to the regular face. A configured
/// `font_path` always wins, for every style.
fn load_styled(font_path: Option<&Path>, bold: bool, italic: bool) -> Option<Vec<u8>> {
    if let Some(path) = font_path {
        return std::fs::read(path).ok();
    }
    for candidate in font_candidates(bold, italic) {
        let path = Path::new(&candidate);
        if path.is_file()
            && let Ok(bytes) = std::fs::read(path)
        {
            return Some(bytes);
        }
    }
    if bold || italic {
        return load_styled(None, false, false);
    }
    None
}

/// Load the regular, bold, italic and bold-italic faces used to substitute
/// glyphs a PDF font does not have. Missing faces are left empty; the
/// replacement then reports [`Error::FontNotFound`] only if one is needed.
pub fn load_substitute_fonts(font_path: Option<&Path>) -> SubstituteFonts {
    SubstituteFonts {
        regular: load_styled(font_path, false, false).unwrap_or_default(),
        bold: load_styled(font_path, true, false).unwrap_or_default(),
        italic: load_styled(font_path, false, true).unwrap_or_default(),
        bold_italic: load_styled(font_path, true, true).unwrap_or_default(),
    }
}

/// Page geometry and running text for an HTML → PDF render.
#[derive(Debug, Clone)]
pub struct PageSetup {
    pub width_mm: f64,
    pub height_mm: f64,
    pub margin_top_mm: f64,
    pub margin_bottom_mm: f64,
    pub margin_left_mm: f64,
    pub margin_right_mm: f64,
    pub header: Option<String>,
    pub footer: Option<String>,
    pub page_numbers: bool,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            width_mm: 210.0,
            height_mm: 297.0,
            margin_top_mm: 20.0,
            margin_bottom_mm: 20.0,
            margin_left_mm: 20.0,
            margin_right_mm: 20.0,
            header: None,
            footer: None,
            page_numbers: false,
        }
    }
}

/// Render an HTML fragment to PDF bytes with the given embedded TTF.
pub fn html_to_pdf(html: &str, font: &[u8]) -> Result<Vec<u8>, Error> {
    html_to_pdf_with_images(html, font, &BTreeMap::new())
}

/// Render an HTML fragment with images keyed by their `src` value.
pub fn html_to_pdf_with_images(
    html: &str,
    font: &[u8],
    images: &BTreeMap<String, Base64OrRaw>,
) -> Result<Vec<u8>, Error> {
    html_to_pdf_with_setup(html, font, images, &PageSetup::default())
}

/// Render an HTML fragment with explicit page geometry and running text.
pub fn html_to_pdf_with_setup(
    html: &str,
    font: &[u8],
    images: &BTreeMap<String, Base64OrRaw>,
    setup: &PageSetup,
) -> Result<Vec<u8>, Error> {
    html_to_pdf_with_families(html, font, images, setup, &[], &mut Vec::new())
}

/// Resolve requested document families before layout, reporting substitutions.
pub(crate) fn html_to_pdf_with_families(
    html: &str,
    font: &[u8],
    images: &BTreeMap<String, Base64OrRaw>,
    setup: &PageSetup,
    families: &[String],
    warnings: &mut Vec<crate::messages::Msg>,
) -> Result<Vec<u8>, Error> {
    let document = html_document(html, font, images, setup, families, warnings)?;
    save_generated(document)
}

/// Stable resource names follow first use, not backend process-global counters.
/// Only generated documents pass here; imported PDF editing retains its objects.
pub(crate) fn save_generated(mut document: PdfDocument) -> Result<Vec<u8>, Error> {
    use printpdf::{Op, PdfFontHandle};
    let mut fonts = BTreeMap::new();
    let mut images = BTreeMap::new();
    let mut states = BTreeMap::new();
    fn stable(ids: &mut BTreeMap<String, String>, id: &mut String, prefix: &str) {
        let next = ids.len() + 1;
        *id = ids.entry(id.clone()).or_insert_with(|| format!("{prefix}{next}")).clone();
    }
    for page in &mut document.pages {
        for op in &mut page.ops {
            match op {
                Op::SetFont { font: PdfFontHandle::External(id), .. } => stable(&mut fonts, &mut id.0, "Font"),
                Op::UseXobject { id, .. } => stable(&mut images, &mut id.0, "Image"),
                Op::LoadGraphicsState { gs } => stable(&mut states, &mut gs.0, "State"),
                _ => {}
            }
        }
    }
    document.resources.fonts.map = std::mem::take(&mut document.resources.fonts.map).into_iter()
        .filter_map(|(id, value)| fonts.get(&id.0).map(|name| (printpdf::FontId(name.clone()), value))).collect();
    document.resources.xobjects.map = std::mem::take(&mut document.resources.xobjects.map).into_iter()
        .filter_map(|(id, value)| images.get(&id.0).map(|name| (printpdf::XObjectId(name.clone()), value))).collect();
    document.resources.extgstates.map = std::mem::take(&mut document.resources.extgstates.map).into_iter()
        .filter_map(|(id, value)| states.get(&id.0).map(|name| (printpdf::ExtendedGraphicsStateId(name.clone()), value))).collect();
    let mut pdf = printpdf::serialize::to_lopdf_doc(&document, &PdfSaveOptions::default(), &mut Vec::new());
    // printpdf's instance/document trailer IDs are random even with epoch dates.
    pdf.trailer.remove(b"ID");
    pdf.compress();
    let mut bytes = Vec::new();
    pdf.save_to(&mut bytes).map_err(|e| Error::reader(MessageCode::FileWrite, e))?;
    Ok(bytes)
}

pub(crate) fn html_document(
    html: &str,
    font: &[u8],
    images: &BTreeMap<String, Base64OrRaw>,
    setup: &PageSetup,
    families: &[String],
    warnings: &mut Vec<crate::messages::Msg>,
) -> Result<PdfDocument, Error> {
    let mut fonts: BTreeMap<String, Base64OrRaw> = BTreeMap::new();
    fonts.insert(FONT_FAMILY.to_string(), Base64OrRaw::Raw(font.to_vec()));
    for (bold, italic, key) in [
        (true, false, "bold"),
        (false, true, "italic"),
        (true, true, "bolditalic"),
    ] {
        if let Some(bytes) = load_styled(None, bold, italic) {
            fonts.insert(format!("Doku-{key}"), Base64OrRaw::Raw(bytes));
        }
    }
    let default_family = font_family(font).unwrap_or_else(|| FONT_FAMILY.to_string());
    // Resolve the generated CSS alias, never the same word in visible text.
    let mut html = html.replacen(
        &format!("font-family: \"{FONT_FAMILY}\";"),
        &format!("font-family: \"{default_family}\";"),
        1,
    );
    if !families.is_empty() {
        let mut paths = Vec::new();
        font_files(Path::new("/usr/share/fonts"), &mut paths)?;
        let wanted: BTreeSet<_> = families
            .iter()
            .map(|v| v.to_ascii_lowercase())
            .chain(["noto serif".into(), "noto sans mono".into()])
            .collect();
        let mut available = BTreeSet::new();
        for path in paths {
            let bytes = std::fs::read(&path).map_err(|source| Error::FontRead {
                path: path.clone(),
                source,
            })?;
            let Some(family) = font_family(&bytes) else {
                continue;
            };
            if wanted.contains(&family.to_ascii_lowercase()) {
                available.insert(family.to_ascii_lowercase());
                fonts.insert(path.to_string_lossy().into_owned(), Base64OrRaw::Raw(bytes));
            }
        }
        for family in families {
            if available.contains(&family.to_ascii_lowercase()) {
                continue;
            }
            let lower = family.to_ascii_lowercase();
            let serif = ["times", "serif", "georgia", "cambria", "garamond"]
                .iter()
                .any(|v| lower.contains(v))
                && !lower.contains("sans");
            let substitute = if serif { "Noto Serif" } else { "Noto Sans" };
            html = html.replace(
                &format!(
                    "font-family:'{}'",
                    family.replace('\\', "\\\\").replace('\'', "\\'")
                ),
                &format!("font-family:'{substitute}'"),
            );
            warnings.push(crate::messages::Msg::with(
                MessageCode::DocxMissingFont,
                params(&[
                    ("font", family.clone().into()),
                    ("substitute", substitute.into()),
                ]),
            ));
        }
    }

    let options = GeneratePdfOptions {
        page_width: Some(setup.width_mm as f32),
        page_height: Some(setup.height_mm as f32),
        margin_top: Some(setup.margin_top_mm as f32),
        margin_bottom: Some(setup.margin_bottom_mm as f32),
        margin_left: Some(setup.margin_left_mm as f32),
        margin_right: Some(setup.margin_right_mm as f32),
        header_text: setup.header.clone(),
        footer_text: setup.footer.clone(),
        show_page_numbers: Some(setup.page_numbers),
        ..GeneratePdfOptions::default()
    };

    let (html, links) = crate::pdflinks::prepare(&html)?;
    let mut warnings = Vec::new();
    let mut document = PdfDocument::from_html(&html, images, &fonts, &options, &mut warnings).map_err(
        |message| {
            Error::invalid(
                MessageCode::PdfHtmlLayout,
                params(&[("detail", message.to_string().into())]),
            )
        },
    )?;
    if document.pages.is_empty() {
        return Err(Error::invalid(MessageCode::PdfHtmlNoPages, Params::new()));
    }
    links.annotate(&mut document)?;
    Ok(document)
}

/// Typographic ascent and line height in em for Word's default line boxes.
/// Keep this lookup local to a conversion; callers cache families, not files.
pub(crate) fn family_metrics(family: &str) -> Result<Option<(f64, f64)>, Error> {
    let mut paths = Vec::new();
    font_files(Path::new("/usr/share/fonts"), &mut paths)?;
    for path in paths {
        let bytes = std::fs::read(&path).map_err(|source| Error::FontRead { path, source })?;
        if font_family(&bytes).is_some_and(|name| name.eq_ignore_ascii_case(family))
            && let Ok(face) = ttf_parser::Face::parse(&bytes, 0)
        {
            let em = f64::from(face.units_per_em());
            return Ok(Some((
                f64::from(face.ascender()) / em,
                (f64::from(face.ascender()) - f64::from(face.descender())
                    + f64::from(face.line_gap()))
                    / em,
            )));
        }
    }
    Ok(None)
}

fn font_family(bytes: &[u8]) -> Option<String> {
    let face = ttf_parser::Face::parse(bytes, 0).ok()?;
    face.names()
        .into_iter()
        .find(|n| n.name_id == ttf_parser::name_id::FAMILY && n.is_unicode())
        .and_then(|n| n.to_string())
}

fn font_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Error> {
    let entries = std::fs::read_dir(dir).map_err(|source| Error::FontRead {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| Error::FontRead {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|source| Error::FontRead {
            path: path.clone(),
            source,
        })?;
        if kind.is_dir() {
            font_files(&path, out)?;
        } else if kind.is_file() && path.extension().is_some_and(|v| v == "ttf" || v == "otf") {
            out.push(path);
        }
    }
    out.sort();
    Ok(())
}

/// Build a PDF from plain text.
pub fn text_to_pdf(text: &str, font: &[u8]) -> Result<Vec<u8>, Error> {
    let body = text_to_html(text);
    html_to_pdf(&crate::markdown::full_html(&body), font)
}

/// Extract all text from a PDF.
#[cfg(test)]
pub fn extract_text(path: &Path, rel: &str) -> Result<String, Error> {
    let bytes = crate::path::read_limited(path, rel, 100 * 1024 * 1024)?;
    extract_text_bytes(&bytes, rel)
}

#[cfg(test)]
pub fn extract_text_bytes(bytes: &[u8], rel: &str) -> Result<String, Error> {
    Ok(extract_text_with_warnings(bytes, rel)?.0)
}

pub fn extract_text_with_warnings(bytes: &[u8], rel: &str) -> Result<(String, Vec<crate::messages::Msg>), Error> {
    let document = crate::pdfbudget::load(bytes)?;
    let mut warnings = Vec::new();
    for page in document.get_pages().into_values() {
        warnings.extend(crate::pdfedit::form_warnings(&document, page)?);
    }
    let text = crate::pdfedit::checked_document_text(&document)?;
    if !text.trim().is_empty() || !warnings.is_empty() {
        return Ok((text, warnings));
    }
    let text = pdf_extract::extract_text_from_mem(bytes)
        .map(|text| crate::fontdecode::expand_ligatures(&text))
        .map_err(|err| Error::PdfText {
            path: rel.to_string(),
            source: Box::new(err),
        })?;
    Ok((text, warnings))
}

/// Merge PDF files into one document, preserving page order.
pub fn merge(inputs: &[Vec<u8>]) -> Result<Document, Error> {
    let mut merged = Document::with_version("1.5");
    let mut max_id = 1u32;
    let mut pages: Vec<ObjectId> = Vec::new();
    let mut objects: BTreeMap<ObjectId, Object> = BTreeMap::new();

    for bytes in inputs {
        let mut document = crate::pdfbudget::load(bytes)?;
        materialize_pages(&mut document)?;
        document.renumber_objects_with(max_id);
        max_id = document.max_id + 1;
        pages.extend(document.get_pages().into_values());
        objects.extend(document.objects);
    }

    if pages.is_empty() {
        return Err(Error::invalid(MessageCode::MergeNoPages, Params::new()));
    }

    let pages_id = (max_id, 0);
    max_id += 1;
    for page_id in &pages {
        if let Some(Object::Dictionary(dict)) = objects.get_mut(page_id) {
            dict.set("Parent", pages_id);
        }
    }
    let kids = pages
        .iter()
        .map(|id| Object::Reference(*id))
        .collect::<Vec<_>>();
    objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => pages.len() as i64,
        }),
    );

    let catalog_id = (max_id, 0);
    objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        }),
    );

    merged.objects = objects;
    merged.max_id = max_id;
    merged.trailer.set("Root", catalog_id);
    merged.renumber_objects();
    merged.prune_objects();
    Ok(merged)
}

/// Keep only the requested 1-based pages of a document.
pub fn keep_pages(document: &mut Document, keep: &BTreeSet<u32>) -> Result<(), Error> {
    materialize_pages(document)?;
    let total = document.get_pages().len() as u32;
    let delete = (1..=total)
        .filter(|number| !keep.contains(number))
        .collect::<Vec<_>>();
    document.delete_pages(&delete);
    Ok(())
}

/// Resolve inherited page attributes before any page-tree surgery. The nearest
/// value wins as a whole; indirect values retain their original object IDs.
pub(crate) fn materialize_pages(document: &mut Document) -> Result<(), Error> {
    for page in document.get_pages().into_values() {
        let mut inherited = lopdf::Dictionary::new();
        let mut current = page;
        let mut seen = BTreeSet::new();
        loop {
            if !seen.insert(current) || seen.len() > crate::pdfbudget::DEPTH {
                return Err(crate::pdfbudget::limit(
                    "page_depth",
                    crate::pdfbudget::DEPTH,
                ));
            }
            let dict = document.get_dictionary(current)?;
            for key in [b"Resources".as_slice(), b"MediaBox", b"CropBox", b"Rotate"] {
                if !inherited.has(key)
                    && let Ok(value) = dict.get(key)
                {
                    inherited.set(key, value.clone());
                }
            }
            match dict.get(b"Parent") {
                Ok(parent) => current = parent.as_reference()?,
                Err(_) => break,
            }
        }
        let dict = document.get_dictionary_mut(page)?;
        for (key, value) in inherited {
            dict.set(key, value);
        }
    }
    Ok(())
}

/// Parse a page range such as `"1-3,5"` into 1-based page numbers.
pub fn parse_pages(spec: &str, total: u32) -> Result<BTreeSet<u32>, Error> {
    let error = || Error::PageRange(spec.to_string());
    let mut pages = BTreeSet::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(error());
        }
        match part.split_once('-') {
            Some((start, end)) => {
                let start: u32 = start.trim().parse().map_err(|_| error())?;
                let end: u32 = end.trim().parse().map_err(|_| error())?;
                if start == 0 || end < start || end > total {
                    return Err(error());
                }
                for number in start..=end {
                    pages.insert(number);
                }
            }
            None => {
                let number: u32 = part.parse().map_err(|_| error())?;
                if number == 0 || number > total {
                    return Err(error());
                }
                pages.insert(number);
            }
        }
    }
    if pages.is_empty() {
        return Err(error());
    }
    Ok(pages)
}
