//! The single message catalog.
//!
//! Every user-facing string (summaries, warnings, errors, CLI help) is defined
//! here as a stable code plus a template with `{name}` parameters. The only
//! built-in locale is `en`; adding a language means adding a template table,
//! with no changes to the logic. Unknown locales fall back to `en`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Message parameters, rendered into a template's `{name}` placeholders.
pub type Params = BTreeMap<String, serde_json::Value>;

/// Safe DOCX failure detail; the original error remains in the source chain.
pub(crate) const DOCX_FAILURE: &str = "invalid package";
pub(crate) const PDF_EXTRACTION_FAILURE: &str = "extractor failed";
pub(crate) const OCR_ASSET_FAILURE: &str = "OCR asset does not match its pinned size or SHA-256 hash";
pub(crate) const OCR_RUNTIME_FAILURE: &str = "cannot load ONNX Runtime";
pub(crate) const OCR_INFERENCE_FAILURE: &str = "ONNX inference failed";
pub(crate) const OCR_DATA_FAILURE: &str = "invalid OCR image, tensor, geometry or dictionary";
/// Safe schema validation detail, without echoing an untrusted value.
pub const JSON_TYPE_DETAIL: &str = "invalid field type";

/// Build [`Params`] from `(name, value)` pairs.
pub fn params(pairs: &[(&str, serde_json::Value)]) -> Params {
    bounded_params(
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect(),
    )
}

/// Bound diagnostic strings, not document contents. Preserve field names and
/// report original character counts so callers can distinguish a preview.
pub fn bounded_params(mut values: Params) -> Params {
    let mut truncated = values
        .remove("truncated_fields")
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    for (name, value) in &mut values {
        if truncated.contains_key(name) {
            continue;
        }
        if let serde_json::Value::String(text) = value {
            let count = text.chars().count();
            if count > 160 {
                *text = format!(
                    "{}… [truncated]",
                    text.chars().take(160).collect::<String>()
                );
                truncated.insert(name.clone(), count.into());
            }
        }
    }
    if !truncated.is_empty() {
        values.insert("truncated_fields".into(), truncated.into());
    }
    values
}

/// A stable message code. Codes never change; templates may be translated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageCode {
    /// Warning: PDF reconstruction did not retain every numeric lexeme.
    ReadNumbersUnverified,
    /// Error: a request discriminator is not one of the supported values.
    InputEnum,
    /// Error: Markdown nesting or event count exceeds the admission budget.
    MarkdownLimit,
    /// Summary: a PDF was read.
    ReadPages,
    /// Summary: a non-PDF file was read.
    ReadWords,
    /// Warning: extracted text was truncated.
    ReadTruncated,
    /// Warning: no OCR model directory is configured.
    ReadOcrUnavailable,
    /// Warning: OCR is an estimate of the image's text.
    ReadOcr,
    /// Error: malformed or unsupported reader data.
    ReadInvalid,
    /// Error: invalid read selection.
    ReadRange,
    /// Error: a local decoder could not run or failed.
    ReadDecoder,
    /// Error: an explicitly configured OCR model asset is missing.
    ReadOcrModelMissing,
    /// Error: ONNX Runtime is unconfigured or its library file is missing.
    ReadOcrRuntimeMissing,
    /// Error: image recognition failed.
    ReadOcrFailed,
    /// Warning: an isolated page failed or reached its memory limit.
    OcrPageFailed,
    /// Warning: an isolated page exceeded its deadline and was killed.
    OcrTimeLimit,
    /// Warning: the mean emitted-symbol confidence is low.
    OcrLowConfidence,
    /// Warning: rows beyond the configured limit were omitted.
    ReadRowsTruncated,
    /// Warning: PDF structure is inferred from layout.
    ReadPdfStructure,
    /// Warning: legacy input was normalized through LibreOffice.
    ReadLegacy,
    /// Warning: Markdown cannot represent all source features.
    ReadFlattened,
    /// A generated section label.
    ReadSlide,
    /// A generated notes label.
    ReadNotes,
    /// A generated table-of-contents label.
    ReadContents,
    /// Summary: a conversion finished.
    ConvertDone,
    /// Warning: PDF to text loses layout.
    ConvertPdfTextLossy,
    /// Warning: markdown images are dropped in DOCX.
    ConvertMarkdownImagesDocx,
    /// Warning: HTML images are dropped in DOCX.
    ConvertHtmlImagesDocx,
    /// Summary: a replacement finished.
    ReplaceDone,
    /// Warning: some occurrences were skipped.
    ReplaceSkipped,
    /// Summary: a document was created.
    CreateDone,
    /// Summary: PDFs were merged.
    MergeDone,
    /// Summary: pages were extracted.
    SplitDone,
    /// Warning: a remote image was not fetched.
    ImageRemoteNotFetched,
    /// Warning: a local image could not be read.
    ImageUnreadable,
    /// Warning: DOCX headers/footers lose formatting in PDF.
    DocxHeadersPlain,
    /// Warning: a DOCX image is missing from the package.
    DocxImageMissing,
    /// Warning: DOCX table merges are not preserved in PDF.
    DocxTableMerges,
    /// Warning: a PDF page has no extractable text.
    PdfNoTextPage,
    /// Warning: an image moved to the end of its page in DOCX.
    PdfDocxImageAtEnd,
    /// Warning: a PDF image could not be decoded for DOCX.
    PdfDocxImageDecodeFailed,
    /// Warning: PDF text was read column by column.
    PdfDocxColumns,
    /// Warning: ink bounds include estimates for unsupported painting.
    PdfDocxBoundsApproximate,
    /// Warning: a cyclic or too deeply nested Form invocation was omitted.
    PdfFormSkipped,
    /// Warning: editing invalidated a digital signature.
    PdfSignatureInvalid,
    /// Warning: a substitute font was used on a page.
    PdfSubstituteFont,
    /// Warning: substitute text could not be placed.
    PdfSubstituteUnplaced,
    /// Warning: replaced text does not fit its line even compressed.
    PdfLineOverflow,
    /// Warning: a match covers only part of an indivisible source glyph.
    PdfPartialGlyph,
    /// Error: an absolute path was given.
    PathAbsolute,
    /// Error: a path escapes the base directory.
    PathEscapes,
    /// Error: reading a file failed.
    FileRead,
    /// Error: writing a file failed.
    FileWrite,
    /// Error: an input file is too large.
    FileTooLarge,
    /// Error: a conversion pair is not supported.
    ConvertUnsupported,
    /// Error: a file extension is not a known format.
    FormatUnsupported,
    /// Error: no font was found.
    FontNotFound,
    /// Error: a font file could not be read.
    FontRead,
    /// Error: the substitute font could not be parsed.
    FontParseSubstitute,
    /// Error: a PDF library failure.
    PdfError,
    /// Error: PDF text extraction failed.
    PdfTextExtract,
    /// Error: content nesting or operand count exceeds the parser budget.
    PdfContentLimit,
    /// Error: a DOCX file could not be read.
    DocxRead,
    /// Error: malformed or unsupported OOXML package.
    DocxPackageInvalid,
    /// Warning: a document font is unavailable.
    DocxMissingFont,
    /// Warning: flow layout cannot reproduce the source renderer exactly.
    ConvertLayoutApproximate,
    /// Error: a DOCX file could not be built.
    DocxBuild,
    /// Error: a page range is invalid.
    PdfPageRange,
    /// Error: the output would overwrite the input.
    ConvertOverwritesInput,
    /// Error: the text to find is empty.
    ReplaceFindEmpty,
    /// Error: merge was given no inputs.
    MergeNoInputs,
    /// Error: merge found no pages.
    MergeNoPages,
    /// Error: an input file is empty.
    InputEmpty,
    /// Error: the PDF is encrypted.
    PdfEncrypted,
    /// Error: the PDF has no text layer.
    PdfNoText,
    /// Warning: the HTML PDF backend does not emit URI annotations.
    PdfLinksOmitted,
    /// Error: link positions could not be carried through HTML layout.
    PdfLinkLayout,
    /// Error: HTML layout for PDF failed.
    PdfHtmlLayout,
    /// Error: HTML produced no PDF pages.
    PdfHtmlNoPages,
    /// Error: the request was not valid JSON.
    InputJson,
    /// Error: an unexpected request field, with the schema's allowed names.
    InputUnknownField,
    /// Error: a required request field is missing.
    InputMissingField,
    /// Error: CLI input or working-directory access failed.
    CliIo,
    /// CLI: legacy error wrapper, retained for code compatibility.
    CliError,
    /// CLI: usage text for `--help`.
    CliUsage,
    /// Error: a path contains a symbolic link, including a dangling link.
    PathSymlink,
    /// Error: only regular files can be read or replaced.
    FileNotRegular,
    /// Error: a text input is not valid UTF-8.
    InputUtf8,
    /// Error: a CLI argument cannot be represented as UTF-8.
    CliArgumentEncoding,
    /// Error: the process or directory update lock could not be acquired.
    FileUpdateLock,
    /// Error: the CLI received more than one request argument.
    CliArguments,
    /// Warning: an annotation is not a URI link.
    PdfDocxAnnotationUnsupported,
    /// Warning: a URI annotation covers only part of a text fragment.
    PdfDocxLinkExtent,
    /// Warning: illustration text is rasterized.
    PdfDocxVectorText,
    /// Error: the high-resolution page exceeds the raster budget.
    PdfDocxRasterLimit,
    /// Error: local vector rasterization failed.
    PdfDocxRasterFailed,
    /// Warning: vector placement has unsupported page geometry.
    PdfDocxVectorGeometry,
    /// Warning: an unruled numeric table is inferred from repeated alignment.
    PdfDocxTableInferred,
    /// Warning: an original PDF font family was substituted.
    PdfDocxFontSubstituted,
}

impl MessageCode {
    /// Every code, for catalog completeness tests.
    pub const ALL: &'static [MessageCode] = &[
        Self::ReadNumbersUnverified,
        Self::InputEnum,
        Self::MarkdownLimit,
        Self::ReadPages,
        Self::ReadWords,
        Self::ReadTruncated,
        Self::ReadOcrUnavailable,
        Self::ReadOcr,
        Self::ReadInvalid,
        Self::ReadRange,
        Self::ReadDecoder,
        Self::ReadOcrModelMissing,
        Self::ReadOcrRuntimeMissing,
        Self::ReadOcrFailed,
        Self::OcrPageFailed,
        Self::OcrTimeLimit,
        Self::OcrLowConfidence,
        Self::ReadRowsTruncated,
        Self::ReadPdfStructure,
        Self::ReadLegacy,
        Self::ReadFlattened,
        Self::ReadSlide,
        Self::ReadNotes,
        Self::ReadContents,
        Self::ConvertDone,
        Self::ConvertPdfTextLossy,
        Self::ConvertMarkdownImagesDocx,
        Self::ConvertHtmlImagesDocx,
        Self::ReplaceDone,
        Self::ReplaceSkipped,
        Self::CreateDone,
        Self::MergeDone,
        Self::SplitDone,
        Self::ImageRemoteNotFetched,
        Self::ImageUnreadable,
        Self::DocxHeadersPlain,
        Self::DocxImageMissing,
        Self::DocxTableMerges,
        Self::PdfNoTextPage,
        Self::PdfDocxImageAtEnd,
        Self::PdfDocxImageDecodeFailed,
        Self::PdfDocxColumns,
        Self::PdfDocxBoundsApproximate,
        Self::PdfFormSkipped,
        Self::PdfSignatureInvalid,
        Self::PdfSubstituteFont,
        Self::PdfSubstituteUnplaced,
        Self::PdfLineOverflow,
        Self::PdfPartialGlyph,
        Self::PathAbsolute,
        Self::PathEscapes,
        Self::FileRead,
        Self::FileWrite,
        Self::FileTooLarge,
        Self::ConvertUnsupported,
        Self::FormatUnsupported,
        Self::FontNotFound,
        Self::FontRead,
        Self::FontParseSubstitute,
        Self::PdfError,
        Self::PdfTextExtract,
        Self::PdfContentLimit,
        Self::DocxRead,
        Self::DocxPackageInvalid,
        Self::DocxMissingFont,
        Self::ConvertLayoutApproximate,
        Self::DocxBuild,
        Self::PdfPageRange,
        Self::ConvertOverwritesInput,
        Self::ReplaceFindEmpty,
        Self::MergeNoInputs,
        Self::MergeNoPages,
        Self::InputEmpty,
        Self::PdfEncrypted,
        Self::PdfNoText,
        Self::PdfLinksOmitted,
        Self::PdfLinkLayout,
        Self::PdfHtmlLayout,
        Self::PdfHtmlNoPages,
        Self::InputJson,
        Self::InputUnknownField,
        Self::InputMissingField,
        Self::CliIo,
        Self::CliError,
        Self::CliUsage,
        Self::PathSymlink,
        Self::FileNotRegular,
        Self::InputUtf8,
        Self::CliArgumentEncoding,
        Self::FileUpdateLock,
        Self::CliArguments,
        Self::PdfDocxAnnotationUnsupported,
        Self::PdfDocxLinkExtent,
        Self::PdfDocxVectorText,
        Self::PdfDocxRasterLimit,
        Self::PdfDocxRasterFailed,
        Self::PdfDocxVectorGeometry,
        Self::PdfDocxTableInferred,
        Self::PdfDocxFontSubstituted,
    ];

    /// The stable string form of the code, used in JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadNumbersUnverified => "read.numbers_unverified",
            Self::InputEnum => "input.enum",
            Self::MarkdownLimit => "input.markdown_limit",
            Self::ReadPages => "read.pages",
            Self::ReadWords => "read.words",
            Self::ReadTruncated => "read.truncated",
            Self::ReadOcrUnavailable => "read.ocr_unavailable",
            Self::ReadOcr => "read.ocr",
            Self::ReadInvalid => "read.invalid",
            Self::ReadRange => "read.range",
            Self::ReadDecoder => "read.decoder",
            Self::ReadOcrModelMissing => "read.ocr_model_missing",
            Self::ReadOcrRuntimeMissing => "read.ocr_runtime_missing",
            Self::ReadOcrFailed => "read.ocr_failed",
            Self::OcrPageFailed => "ocr.page_failed",
            Self::OcrTimeLimit => "ocr.time_limit",
            Self::OcrLowConfidence => "ocr.low_confidence",
            Self::ReadRowsTruncated => "read.rows_truncated",
            Self::ReadPdfStructure => "read.pdf_structure",
            Self::ReadLegacy => "read.legacy",
            Self::ReadFlattened => "read.flattened",
            Self::ReadSlide => "read.slide",
            Self::ReadNotes => "read.notes",
            Self::ReadContents => "read.contents",
            Self::ConvertDone => "convert.done",
            Self::ConvertPdfTextLossy => "convert.pdf_text_lossy",
            Self::ConvertMarkdownImagesDocx => "convert.markdown_images_docx",
            Self::ConvertHtmlImagesDocx => "convert.html_images_docx",
            Self::ReplaceDone => "replace.done",
            Self::ReplaceSkipped => "replace.skipped",
            Self::CreateDone => "create.done",
            Self::MergeDone => "merge.done",
            Self::SplitDone => "split.done",
            Self::ImageRemoteNotFetched => "image.remote_not_fetched",
            Self::ImageUnreadable => "image.unreadable",
            Self::DocxHeadersPlain => "docx.headers_plain",
            Self::DocxImageMissing => "docx.image_missing",
            Self::DocxTableMerges => "docx.table_merges",
            Self::PdfNoTextPage => "pdf.no_text_page",
            Self::PdfDocxImageAtEnd => "pdfdocx.image_at_end",
            Self::PdfDocxImageDecodeFailed => "pdfdocx.image_decode_failed",
            Self::PdfDocxColumns => "pdfdocx.columns",
            Self::PdfDocxBoundsApproximate => "pdfdocx.bounds_approximate",
            Self::PdfFormSkipped => "pdf.form_skipped",
            Self::PdfDocxAnnotationUnsupported => "pdfdocx.annotation_unsupported",
            Self::PdfDocxLinkExtent => "pdfdocx.link_extent",
            Self::PdfDocxVectorText => "pdfdocx.vector_text_rasterized",
            Self::PdfDocxRasterLimit => "pdfdocx.raster_limit",
            Self::PdfDocxRasterFailed => "pdfdocx.raster_failed",
            Self::PdfDocxVectorGeometry => "pdfdocx.vector_geometry",
            Self::PdfDocxTableInferred => "pdfdocx.table_inferred",
            Self::PdfDocxFontSubstituted => "pdfdocx.font_substituted",
            Self::PdfSignatureInvalid => "pdf.signature_invalid",
            Self::PdfSubstituteFont => "pdf.substitute_font",
            Self::PdfSubstituteUnplaced => "pdf.substitute_unplaced",
            Self::PdfLineOverflow => "pdf.line_overflow",
            Self::PdfPartialGlyph => "pdf.partial_glyph",
            Self::PathAbsolute => "path.absolute",
            Self::PathEscapes => "path.escapes",
            Self::FileRead => "file.read",
            Self::FileWrite => "file.write",
            Self::FileTooLarge => "file.too_large",
            Self::ConvertUnsupported => "convert.unsupported",
            Self::FormatUnsupported => "format.unsupported",
            Self::FontNotFound => "font.not_found",
            Self::FontRead => "font.read",
            Self::FontParseSubstitute => "font.parse_substitute",
            Self::PdfError => "pdf.error",
            Self::PdfTextExtract => "pdf.text_extract",
            Self::PdfContentLimit => "pdf.content_limit",
            Self::DocxRead => "docx.read",
            Self::DocxPackageInvalid => "docx.package_invalid",
            Self::DocxMissingFont => "docx.missing_font",
            Self::ConvertLayoutApproximate => "convert.layout_approximate",
            Self::DocxBuild => "docx.build",
            Self::PdfPageRange => "pdf.page_range",
            Self::ConvertOverwritesInput => "convert.overwrites_input",
            Self::ReplaceFindEmpty => "replace.find_empty",
            Self::MergeNoInputs => "merge.no_inputs",
            Self::MergeNoPages => "merge.no_pages",
            Self::InputEmpty => "input.empty",
            Self::PdfEncrypted => "pdf.encrypted",
            Self::PdfNoText => "pdf.no_text",
            Self::PdfLinksOmitted => "pdf.links_omitted",
            Self::PdfLinkLayout => "pdf.link_layout",
            Self::PdfHtmlLayout => "pdf.html_layout",
            Self::PdfHtmlNoPages => "pdf.html_no_pages",
            Self::InputJson => "input.json",
            Self::InputUnknownField => "input.unknown_field",
            Self::InputMissingField => "input.missing_field",
            Self::CliIo => "cli.io",
            Self::CliError => "cli.error",
            Self::CliUsage => "cli.usage",
            Self::PathSymlink => "path.symlink",
            Self::FileNotRegular => "file.not_regular",
            Self::InputUtf8 => "input.utf8",
            Self::CliArgumentEncoding => "cli.argument_encoding",
            Self::FileUpdateLock => "file.update_lock",
            Self::CliArguments => "cli.arguments",
        }
    }
}

/// The `en` template table: code to template with `{name}` parameters.
fn template_en(code: MessageCode) -> &'static str {
    match code {
        MessageCode::ReadNumbersUnverified => "page {page}, text line {line}: numeric tokens were changed or omitted during structure reconstruction; consult the source",
        MessageCode::InputEnum => "invalid value for {field}; allowed values: {allowed}",
        MessageCode::MarkdownLimit => "Markdown exceeds the {kind} limit ({limit})",
        MessageCode::PdfDocxAnnotationUnsupported => {
            "page {page}: a non-URI or malformed annotation could not be preserved"
        }
        MessageCode::PdfDocxLinkExtent => {
            "page {page}: a link covers part of a text fragment; the whole fragment is linked"
        }
        MessageCode::PdfDocxVectorText => {
            "page {page}: text inside a vector illustration was rasterized and is no longer editable"
        }
        MessageCode::PdfDocxRasterLimit => {
            "page {page}: 216 DPI rendering exceeds the 40 megapixel raster limit"
        }
        MessageCode::PdfDocxRasterFailed => {
            "vector illustration rendering failed; install bubblewrap and Poppler"
        }
        MessageCode::PdfDocxVectorGeometry => {
            "page {page}: vector placement is unsupported for {feature}; illustration was omitted"
        }
        MessageCode::PdfDocxTableInferred => {
            "page {page}: an unruled numeric table was inferred from column alignment"
        }
        MessageCode::PdfDocxFontSubstituted => {
            "page {page}: PDF font {font} was replaced by {substitute}; glyph metrics may differ"
        }
        MessageCode::PdfFormSkipped => {
            "page {page}: cyclic or deeply nested Form content at depth {depth} was skipped"
        }
        MessageCode::PdfDocxBoundsApproximate => {
            "page {page}: ink bounds are approximate for unsupported font metrics, clipping or painting operations"
        }
        MessageCode::ReadPages => "read {pages} page(s), {words} word(s) from {path}",
        MessageCode::ReadWords => "read {words} word(s) from {path}",
        MessageCode::ReadTruncated => "text truncated to {max_chars} characters",
        MessageCode::ReadOcrUnavailable => {
            "OCR models are unavailable; configure Options.ocr_models with the pinned PP-OCRv5 model directory"
        }
        MessageCode::ReadOcr => {
            "text was recognized by OCR; spelling and structure may be inaccurate"
        }
        MessageCode::ReadInvalid => "malformed, oversized or unsupported document data",
        MessageCode::ReadRange => "invalid or inapplicable read range {range}",
        MessageCode::ReadDecoder => {
            "local document decoder failed; this format requires bubblewrap and LibreOffice, Poppler or ImageMagick"
        }
        MessageCode::ReadOcrModelMissing => "OCR model asset {asset} is missing",
        MessageCode::ReadOcrRuntimeMissing => "ONNX Runtime is unavailable; configure a trusted local runtime library",
        MessageCode::ReadOcrFailed => "OCR failed while loading models or recognizing an image",
        MessageCode::OcrPageFailed => "page {page}: OCR worker failed or exceeded its memory limit; recognized text was omitted",
        MessageCode::OcrTimeLimit => "page {page}: OCR exceeded its time limit; recognized text was omitted",
        MessageCode::OcrLowConfidence => "page {page}: OCR confidence is below the quality threshold",
        MessageCode::ReadRowsTruncated => {
            "sheet {sheet}: limited to {max_rows} row(s); additional rows were omitted"
        }
        MessageCode::ReadPdfStructure => {
            "page {page}: Markdown structure is inferred from PDF layout; links, footnotes and image descriptions may not be recoverable"
        }
        MessageCode::ReadLegacy => {
            "{format} was normalized through LibreOffice; unsupported layout and objects may be lost"
        }
        MessageCode::ReadFlattened => {
            "{feature} cannot be fully represented in Markdown and was flattened or omitted"
        }
        MessageCode::ReadSlide => "Slide {slide}",
        MessageCode::ReadNotes => "Notes",
        MessageCode::ReadContents => "Contents",
        MessageCode::ConvertDone => "converted {input} ({from}) to {output} ({to})",
        MessageCode::ConvertPdfTextLossy => {
            "PDF -> text: layout, columns and images are not preserved"
        }
        MessageCode::ConvertMarkdownImagesDocx => "markdown images are not preserved in DOCX",
        MessageCode::ConvertHtmlImagesDocx => "HTML images are not preserved in DOCX",
        MessageCode::ReplaceDone => "replaced {count} occurrence(s) of {find} in {path}",
        MessageCode::ReplaceSkipped => {
            "{skipped} of {total} occurrence(s) were skipped because the text is split across fragments"
        }
        MessageCode::CreateDone => "created {output} from markdown",
        MessageCode::MergeDone => "merged {count} pdf(s) into {output}",
        MessageCode::SplitDone => "extracted {pages} page(s) from {input} into {output}",
        MessageCode::ImageRemoteNotFetched => "remote image {src} was not fetched",
        MessageCode::ImageUnreadable => "image {src} could not be read",
        MessageCode::DocxHeadersPlain => {
            "headers/footers are rendered as plain text without their formatting"
        }
        MessageCode::DocxImageMissing => "image {id} is missing from the package",
        MessageCode::DocxTableMerges => "table cell merges are not preserved in DOCX -> PDF",
        MessageCode::PdfNoTextPage => "page {page} has no extractable text",
        MessageCode::PdfDocxImageAtEnd => {
            "page {page}: image is placed at the end of the page, its position is not preserved"
        }
        MessageCode::PdfDocxImageDecodeFailed => "page {page}: an image could not be decoded",
        MessageCode::PdfDocxColumns => {
            "page {page}: text was read column by column ({count} columns)"
        }
        MessageCode::PdfSignatureInvalid => "digital signature is no longer valid",
        MessageCode::PdfSubstituteFont => "substitute font used on page {page}",
        MessageCode::PdfSubstituteUnplaced => {
            "page {page}: cannot place substitute text; match skipped"
        }
        MessageCode::PdfLineOverflow => {
            "page {page}: replaced text does not fit the line even when compressed to 80%"
        }
        MessageCode::PdfPartialGlyph => {
            "{skipped} occurrence(s) were skipped because they cover only part of a source glyph"
        }
        MessageCode::PathAbsolute => {
            "path {path} is absolute; only paths relative to base_dir are allowed"
        }
        MessageCode::PathEscapes => "path {path} escapes base_dir",
        MessageCode::FileRead => "cannot read {path}",
        MessageCode::FileWrite => "cannot write {path}",
        MessageCode::FileTooLarge => {
            "input {path} is {size} bytes, over the limit of {limit} bytes"
        }
        MessageCode::ConvertUnsupported => "unsupported conversion {from} -> {to}",
        MessageCode::FormatUnsupported => "unsupported format {format}",
        MessageCode::FontNotFound => {
            "no font found; set Options.font_path or install a supported system font"
        }
        MessageCode::FontRead => "cannot read font {path}",
        MessageCode::FontParseSubstitute => "cannot parse substitute font: {detail}",
        MessageCode::PdfError => "pdf error: {detail}",
        MessageCode::PdfTextExtract => "cannot extract text from {path}: {detail}",
        MessageCode::PdfContentLimit => "PDF content exceeds the {kind} limit ({limit})",
        MessageCode::DocxRead => "cannot read docx: {detail}",
        MessageCode::DocxPackageInvalid => "malformed, oversized or unsupported OOXML package",
        MessageCode::DocxMissingFont => "font {font} is unavailable; using {substitute}",
        MessageCode::ConvertLayoutApproximate => {
            "{from} -> {to}: layout is reconstructed; spacing, decoration and pagination may differ (margins are estimated for PDF input)"
        }
        MessageCode::DocxBuild => "cannot build docx: {detail}",
        MessageCode::PdfPageRange => "invalid page range {range}",
        MessageCode::ConvertOverwritesInput => "output would overwrite the input file",
        MessageCode::ReplaceFindEmpty => "find must not be empty",
        MessageCode::MergeNoInputs => "merge needs at least one input",
        MessageCode::MergeNoPages => "no pages to merge",
        MessageCode::InputEmpty => "{path} is empty",
        MessageCode::PdfEncrypted => "PDF is encrypted; cannot edit text",
        MessageCode::PdfNoText => "PDF has no text (scan); nothing to replace",
        MessageCode::PdfLinksOmitted => "The PDF backend omitted link destinations ({count}); linked text is not clickable.",
        MessageCode::PdfLinkLayout => "cannot preserve link positions in this document layout",
        MessageCode::PdfHtmlLayout => "cannot lay out html: {detail}",
        MessageCode::PdfHtmlNoPages => "html produced no pdf pages",
        MessageCode::InputJson => "json error: {detail}",
        MessageCode::InputUnknownField => "unknown field {field}; allowed fields: {allowed}",
        MessageCode::InputMissingField => "required field {field} is missing",
        MessageCode::CliIo => "cannot access CLI input or working directory",
        MessageCode::CliError => "error: {detail}",
        MessageCode::PathSymlink => "path {path} contains a symbolic link",
        MessageCode::FileNotRegular => "path {path} is not a regular file",
        MessageCode::InputUtf8 => "text input must be valid UTF-8",
        MessageCode::CliArgumentEncoding => "CLI argument {field} must be valid UTF-8",
        MessageCode::FileUpdateLock => "cannot acquire the document update lock",
        MessageCode::CliArguments => "invalid {field}; expected one of {allowed}",
        MessageCode::CliUsage => {
            "usage: {name} '<json>' | {name} --schema | {name} --messages | {name} --help\nReads a JSON request from the argument or stdin and prints a JSON result."
        }
    }
}

/// Export the same catalog used to render every response.
pub fn catalog() -> serde_json::Value {
    let messages: Vec<_> = MessageCode::ALL.iter().map(|&code| serde_json::json!({
        "code":code.as_str(), "kind":kind(code), "template":template(code,"en"),
        "params":required_params(code,"en"),
    })).collect();
    serde_json::json!({"tool":crate::NAME,"version":env!("CARGO_PKG_VERSION"),"messages":messages})
}

fn kind(code: MessageCode) -> &'static str {
    use MessageCode::*;
    match code {
        ReadPages | ReadWords | ConvertDone | ReplaceDone | CreateDone | MergeDone | SplitDone | CliUsage => "summary",
        ReadSlide | ReadNotes | ReadContents => "label",
        ReadNumbersUnverified | ReadTruncated | ReadOcrUnavailable | ReadOcr | OcrPageFailed | OcrTimeLimit | OcrLowConfidence |
        ReadRowsTruncated | ReadPdfStructure | ReadLegacy | ReadFlattened | ConvertPdfTextLossy |
        ConvertMarkdownImagesDocx | ConvertHtmlImagesDocx | ReplaceSkipped | ImageRemoteNotFetched |
        ImageUnreadable | DocxHeadersPlain | DocxImageMissing | DocxTableMerges | PdfNoTextPage |
        PdfDocxImageAtEnd | PdfDocxImageDecodeFailed | PdfDocxColumns | PdfDocxBoundsApproximate |
        PdfFormSkipped | PdfSignatureInvalid | PdfSubstituteFont | PdfSubstituteUnplaced |
        PdfLineOverflow | PdfPartialGlyph | DocxMissingFont | ConvertLayoutApproximate |
        PdfLinksOmitted | PdfDocxAnnotationUnsupported | PdfDocxLinkExtent | PdfDocxVectorText |
        PdfDocxVectorGeometry | PdfDocxTableInferred | PdfDocxFontSubstituted => "warning",
        _ => "error",
    }
}

/// The template for `code` in `locale`. `en` is the only built-in table for
/// now, so every locale (known or not) renders from it; a new language adds
/// its table here.
pub fn template(code: MessageCode, locale: &str) -> &'static str {
    let _ = locale;
    template_en(code)
}

/// The parameter names a template needs, in order of first appearance.
pub fn required_params(code: MessageCode, locale: &str) -> Vec<&'static str> {
    let text = template(code, locale);
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else { break };
        let name = &after[..close];
        if !names.contains(&name) {
            names.push(name);
        }
        rest = &after[close + 1..];
    }
    names
}

/// Render `code` with `params` in `locale`. Unknown locales use `en`;
/// a missing parameter is left as `{name}` so catalog gaps stay visible.
pub fn render(code: MessageCode, params: &Params, locale: &str) -> String {
    let mut out = template(code, locale).to_string();
    for (name, value) in params {
        let text = match value {
            serde_json::Value::String(text)
                if code == MessageCode::ReadFlattened && name == "feature" =>
            {
                match text.as_str() {
                    "merged_cells" => "merged table cells".into(),
                    "document_objects" => "headers, footers, charts or equations".into(),
                    "formulas" => {
                        "spreadsheet formulas (cached values are used; formulas are not evaluated)"
                            .into()
                    }
                    "tiff_frames" => {
                        "multi-frame TIFF images (only the first frame is read)".into()
                    }
                    _ => text.clone(),
                }
            }
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        out = out.replace(&format!("{{{name}}}"), &text);
    }
    out
}

/// An unrendered message: a code plus its parameters. Internal code passes
/// these around; they are rendered once, in the caller's locale, at the edges.
#[derive(Debug, Clone, PartialEq)]
pub struct Msg {
    /// The message code.
    pub code: MessageCode,
    /// Parameters for the template.
    pub params: Params,
}

impl Msg {
    /// A message with no parameters.
    pub fn new(code: MessageCode) -> Self {
        Self {
            code,
            params: Params::new(),
        }
    }

    /// A message with parameters.
    pub fn with(code: MessageCode, params: Params) -> Self {
        Self {
            code,
            params: bounded_params(params),
        }
    }

    /// Render in `locale`.
    pub fn render(&self, locale: &str) -> String {
        render(self.code, &self.params, locale)
    }

    /// Render as a JSON-ready [`Warning`].
    pub fn warning(&self, locale: &str) -> Warning {
        Warning {
            code: self.code.as_str().to_string(),
            message: self.render(locale),
            params: self.params.clone(),
        }
    }
}

/// One non-fatal problem, as returned in [`crate::Output::warnings`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Warning {
    /// Stable message code, for example `pdf.substitute_font`.
    pub code: String,
    /// The message rendered in the requested locale.
    pub message: String,
    /// Parameters the message was rendered with.
    #[serde(default)]
    pub params: Params,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_complete_en_template() {
        let mut seen = std::collections::BTreeSet::new();
        for code in MessageCode::ALL {
            assert!(
                seen.insert(code.as_str()),
                "duplicate code {}",
                code.as_str()
            );
            let text = template(*code, "en");
            assert!(
                !text.trim().is_empty(),
                "empty template for {}",
                code.as_str()
            );
            let mut supplied = Params::new();
            for name in required_params(*code, "en") {
                supplied.insert(name.to_string(), serde_json::Value::String("x".to_string()));
            }
            let rendered = render(*code, &supplied, "en");
            assert!(
                !rendered.contains('{') && !rendered.contains('}'),
                "unfilled parameter for {}: {rendered}",
                code.as_str()
            );
        }
    }

    #[test]
    fn unknown_locale_falls_back_to_en() {
        let rendered = render(MessageCode::PdfNoText, &Params::new(), "xx-unknown");
        assert_eq!(rendered, template_en(MessageCode::PdfNoText));
    }
}
