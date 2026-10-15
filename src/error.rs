//! Error type for `docs`. Every failure keeps its cause and renders its
//! message from the catalog in [`crate::messages`].

use std::path::PathBuf;

use crate::messages::{MessageCode, Msg, Params, params, render};

/// Anything that can go wrong while running an operation.
#[derive(Debug, thiserror::Error)]
#[error("{message}", message = self.message("en"))]
pub enum Error {
    /// A supplied path was absolute; only paths below `base_dir` are allowed.
    AbsolutePath {
        /// The path as supplied by the caller.
        path: String,
    },

    /// A supplied path escaped `base_dir` after normalisation.
    PathEscapes {
        /// The path as supplied by the caller.
        path: String,
    },

    /// Reading an input file failed.
    Read {
        /// The path as supplied by the caller, relative to `base_dir`.
        path: String,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// Writing an output file failed.
    Write {
        /// The path as supplied by the caller, relative to `base_dir`.
        path: String,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// An input exceeded `Options::max_input_bytes`.
    TooLarge {
        /// The path as supplied by the caller, relative to `base_dir`.
        path: String,
        /// Actual size in bytes.
        size: u64,
        /// Configured limit in bytes.
        limit: u64,
    },

    /// The requested output format combination is not implemented.
    UnsupportedConversion {
        /// Source format name.
        from: String,
        /// Target format name.
        to: String,
    },

    /// An extension did not map to a known format.
    UnsupportedFormat(String),

    /// No font file could be found for PDF creation.
    FontNotFound,

    /// The configured font file could not be read.
    FontRead {
        /// The configured font path.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },

    /// A PDF operation failed inside `lopdf`.
    Pdf(#[source] lopdf::Error),

    /// Text extraction from a PDF failed.
    PdfText {
        /// The path as supplied by the caller, relative to `base_dir`.
        path: String,
        /// Original extractor failure, excluded from public diagnostics.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// A DOCX file could not be read.
    Docx(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A DOCX file could not be built.
    DocxBuild(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// A page range such as `"1-3,5"` was empty, malformed or out of bounds.
    PageRange(String),

    /// The input was well formed but not usable; carries a catalog message.
    Invalid(Msg),

    /// A reader or local decoder failed; the cause is retained but not exposed in messages.
    Reader {
        /// Stable error category.
        code: MessageCode,
        /// Underlying parser, codec or I/O failure.
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    /// Safe field or operation context without discarding the underlying failure.
    Context {
        /// Extra public parameters.
        params: Params,
        /// The original failure, retaining its stable category and source chain.
        source: Box<Error>,
    },

    /// The raw input was not valid JSON.
    Json(#[source] serde_json::Error),
}

impl Error {
    pub(crate) fn reader(
        code: MessageCode,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Reader {
            code,
            source: Box::new(source),
        }
    }

    /// An [`Error::Invalid`] carrying a catalog message.
    pub fn invalid(code: MessageCode, params: Params) -> Self {
        Self::Invalid(Msg::with(code, params))
    }

    /// The stable message code for this error.
    pub fn code(&self) -> MessageCode {
        match self {
            Self::AbsolutePath { .. } => MessageCode::PathAbsolute,
            Self::PathEscapes { .. } => MessageCode::PathEscapes,
            Self::Read { .. } => MessageCode::FileRead,
            Self::Write { .. } => MessageCode::FileWrite,
            Self::TooLarge { .. } => MessageCode::FileTooLarge,
            Self::UnsupportedConversion { .. } => MessageCode::ConvertUnsupported,
            Self::UnsupportedFormat(_) => MessageCode::FormatUnsupported,
            Self::FontNotFound => MessageCode::FontNotFound,
            Self::FontRead { .. } => MessageCode::FontRead,
            Self::Pdf(_) => MessageCode::PdfError,
            Self::PdfText { .. } => MessageCode::PdfTextExtract,
            Self::Docx(_) => MessageCode::DocxRead,
            Self::DocxBuild(_) => MessageCode::DocxBuild,
            Self::PageRange(_) => MessageCode::PdfPageRange,
            Self::Invalid(msg) => msg.code,
            Self::Json(_) => MessageCode::InputJson,
            Self::Reader { code, .. } => *code,
            Self::Context { source, .. } => source.code(),
        }
    }

    /// The parameters the message for this error is rendered with.
    pub fn params(&self) -> Params {
        let mut values = match self {
            Self::Context { params: extra, source } => {
                let mut values = source.params();
                values.extend(extra.clone());
                crate::messages::bounded_params(values)
            }
            Self::AbsolutePath { path } | Self::PathEscapes { path } => {
                params(&[("path", path.clone().into())])
            }
            Self::Read { path, source } | Self::Write { path, source } => params(&[
                ("path", path.clone().into()),
                ("kind", format!("{:?}", source.kind()).into()),
            ]),
            Self::TooLarge { path, size, limit } => params(&[
                ("path", path.clone().into()),
                ("size", (*size).into()),
                ("limit", (*limit).into()),
            ]),
            Self::UnsupportedConversion { from, to } => {
                params(&[("from", from.clone().into()), ("to", to.clone().into())])
            }
            Self::UnsupportedFormat(format) => params(&[("format", format.clone().into())]),
            Self::FontNotFound => Params::new(),
            Self::Reader { source, .. } => {
                if let Some(error)=source.downcast_ref::<Error>() {
                    let mut values=error.params();
                    values.insert("cause_code".into(),error.code().as_str().into());
                    values
                } else {
                    source.downcast_ref::<std::io::Error>()
                        .map(|source| params(&[("kind",format!("{:?}",source.kind()).into())]))
                        .unwrap_or_default()
                }
            },
            Self::FontRead { path, .. } => params(&[("path", path.display().to_string().into())]),
            Self::Pdf(source) => params(&[("detail", source.to_string().into())]),
            Self::PdfText { path, .. } => params(&[
                ("path", path.clone().into()),
                ("detail", crate::messages::PDF_EXTRACTION_FAILURE.into()),
            ]),
            Self::Docx(_) | Self::DocxBuild(_) => {
                // Keep parser diagnostics in the source chain, not public JSON.
                params(&[("detail", crate::messages::DOCX_FAILURE.into())])
            }
            Self::PageRange(range) => params(&[("range", range.clone().into())]),
            Self::Invalid(msg) => msg.params.clone(),
            Self::Json(source) => params(&[
                ("field", "".into()),
                ("detail", source.to_string().into()),
                ("line", source.line().into()),
                ("column", source.column().into()),
            ]),
        };
        let field = match self.code() {
            MessageCode::ReplaceFindEmpty => "/find",
            MessageCode::MergeNoInputs => "/inputs",
            MessageCode::FileWrite | MessageCode::ConvertOverwritesInput => "/output",
            MessageCode::InputJson => "",
            _ => "/input",
        };
        values.entry("field".into()).or_insert_with(||field.into());
        values
    }

    /// Attach safe structured context while retaining the original cause.
    pub fn with_params(self, params: Params) -> Self {
        Self::Context { params, source: Box::new(self) }
    }

    /// Render a failure using the same wire shape as a successful result.
    pub fn to_output(&self, locale: &str) -> crate::Output {
        crate::Output {
            ok: false,
            output: None,
            bytes: None,
            text: None,
            data: None,
            summary: self.message(locale),
            summary_code: self.code().as_str().into(),
            summary_params: self.params(),
            warnings: Vec::new(),
        }
    }

    /// The catalog message for this error, rendered in `locale`.
    pub fn message(&self, locale: &str) -> String {
        render(self.code(), &self.params(), locale)
    }
}

impl serde::Serialize for Error {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_output("en").serialize(serializer)
    }
}

impl From<lopdf::Error> for Error {
    fn from(source: lopdf::Error) -> Self {
        Self::Pdf(source)
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::Json(source)
    }
}
