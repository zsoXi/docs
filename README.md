# docs

One small tool for documents. An agent sends a short JSON request, and `docs`
does the work deterministically and returns a short JSON result (output path,
size, one sentence). It reads, converts, edits, creates, merges and splits
common document, presentation, spreadsheet, ebook and image formats. Conversion
and editing still support PDF, DOCX, Markdown, HTML and text.

- Small surface: one request in, one result out.
- No network, no accounts.
- All paths are relative to a base directory and are validated before use.
- Imperfect results are reported in `warnings`. Impossible requests return an
  error instead of a silently broken file.

## Operations

| `op` | Fields | What it does |
|---|---|---|
| `read` | `input`, `max_chars` (20000), `locators` (false), `pages`, `range`, `max_rows` (1000) | Return the input as Markdown, optionally with source locators. Truncates and warns. |
| `convert` | `input`, `output` | Convert by file extension (see pairs below). |
| `replace` | `input`, `output`, `find`, `replace`, `all` (default true) | Replace text in PDF, DOCX, Markdown, text or HTML. |
| `create` | `markdown`, `output` | New DOCX or PDF from Markdown (headings, paragraphs, lists, bold/italic, tables, links, local images). |
| `merge` | `inputs`, `output` | Merge PDFs. |
| `split` | `input`, `pages` (e.g. `"1-3,5"`), `output` | Extract pages of a PDF into a new file. |

Supported format pairs:

- PDF → DOCX, MD, TXT
- DOCX → PDF, DOCX, MD, TXT
- MD → DOCX, PDF
- HTML → PDF, DOCX, MD
- TXT → PDF, DOCX

`convert` refuses identical input/output paths and same-format file aliases.
Different-format hard-link outputs are detached by atomic replacement, and the
source stays intact. All conversions load a checked input snapshot in memory.

### File access

Request paths must stay under `base_dir`. A symbolic link in any path component
returns `path.symlink`, even a dangling link or one that points inside the base.
Only regular files are accepted as input (`file.not_regular` for directories and
FIFOs). On Linux, directory traversal is descriptor-anchored with `O_NOFOLLOW`.
Inputs are opened nonblocking, and the opened descriptor's type and size are
checked again.

Every writer, merge and split included, stages a sibling temporary file, syncs
it and renames it atomically. On Linux this uses `openat`/`renameat` against the
same opened directory descriptor. If that directory is moved during a write and
a symlink is put at its old path, the result still can't be redirected. A failed
write keeps the previous output. Updates within one process share one lock, and
on Linux a directory lock also serializes cooperating CLI processes that share
the same base directory. Unrelated directories don't need to share a filesystem
lock. The descriptor-anchored implementation requires `/proc`. Text editing and
conversion require valid UTF-8 (`input.utf8`) and check it before any write.

### Editing PDFs in place

`replace` on a PDF changes only the matched text. The content streams are
parsed with their text state (`Tf`, `Tm`, `Td`, `TJ`, colours, …), and each
shown string is decoded through its font (`ToUnicode`, `Encoding` or CID
CMap). Typographic ligatures (`ﬀ ﬁ ﬂ ﬃ ﬄ ﬅ ﬆ`) are exposed as ordinary
letters for reading, conversion and matching, so `firmą` also finds `ﬁrmą`.
Source glyph advances and byte ranges are kept separately. A match that covers
only part of one glyph is skipped with `pdf.partial_glyph` instead of deleting
the glyph's unmatched letters. To edit it, search for the whole ligature or word.

If the original font has every character of the replacement, the string bytes
are re-encoded with that font. Glyphs are checked against the embedded font's
cmap, not only `ToUnicode`. Otherwise the old text is removed and the new text
is drawn at the same position in an embedded substitute font, which is reported
in `warnings`. The substitute matches the original's style (bold text gets the
bold face) and is subset to the characters actually used, so the file grows by a
few kilobytes. Each new subset gets its own unused resource name, also on
consecutive edits. Stale inline `ActualText` overrides around edited glyphs are
removed, so independent extractors see the replacement and not the old text.

The rest of the line shifts by the width difference, as in a PDF editor. If the
longer line would cross the column's right margin, the new text is compressed
horizontally (`Tz`, down to 80%). If that still isn't enough, a warning with the
page number is returned. Images, vector graphics, other text, annotations, links
and metadata are left unchanged. A digitally signed PDF can still be edited, with
a warning that the signature is no longer valid. Encrypted PDFs and scans without
a text layer are rejected.

### Editing DOCX packages

`replace` edits body text in the original `word/document.xml`, including matches
split across adjacent runs or hyperlinks. Matches don't cross paragraphs, tabs or
line breaks, and the replacement text keeps the first run's formatting. Other XML
markup and every unrelated ZIP part are kept, including compressed image
payloads, hyperlink relationships, headers, footnotes, comments and numbering.
Only the main document body is searched, so header, footer and note text is not
edited. If nothing matches, the whole input package is kept unchanged. ZIP64,
encrypted entries and unsupported package encodings return an error before any
output is written. In-place edits use the same locked, atomic file update as
other formats.

PDF content parsing is limited to 64 operand nesting levels and 100,000 operands
per content sequence. Before the library loads a PDF, an admission scan checks
object and xref counts, value nesting, declared stream lengths, filter chains and
object-stream declarations. Encoded streams and general-purpose decoded streams
are limited to 8 MiB each, filter chains to 32 layers, and the document's total
decoded stream size to 64 MiB. Decoding holds only one bounded stream at a time
and uses bounded codec primitives, eager object/xref streams included. Consumers
use the same decoding gateway. The object/value count is limited to 100,000. The
PDF loader runs sequentially with its optional Rayon feature disabled, so it
needs no global thread pool and works under memory and thread limits. JPEG, JPX
and CCITT image codecs are separate from the general-purpose stream decoder.

Form interpretation and graphics stacks have a depth limit of 32. Cyclic or
deeper Form invocations are skipped with `pdf.form_skipped` (`page`, `depth`),
which is not an error. PDF-to-DOCX also reports approximate bounds for `Do`.
Skipped content is not recovered through an OCR or text-extractor fallback.
Editing leaves unvisited Form bytes untouched and reports the omission. Expanded
Form instances on one page are limited to 64 MiB and 100,000 invocations. A
resource budget violation returns `pdf.content_limit` with `kind` and `limit`
before any output is written.

All Contents streams of a page share one text and graphics state and keep their
source ranges for edits. Form text uses the Form's local resources and Matrix,
also across repeated invocations. Editing creates private Form instances, so
`all:false` does not change another invocation. Page-tree surgery materializes
inherited Resources, MediaBox, CropBox and Rotate before moving pages. Local
Markdown images are scaled down proportionally to fit the default A4 printable
area, without splitting the raster. HTML image width/height attributes and inline
CSS dimensions take precedence over raster dimensions, and an omitted axis keeps
the raster aspect ratio. HTML image limits use the page size and margins selected
by the document's `@page` rule. Fallback text-extraction errors keep their
original cause in the library's `Error::source()` chain. JSON messages contain
only safe extraction context.

### Faithful conversions

- **DOCX → PDF** resolves `docDefaults`, `basedOn` paragraph/character styles
  and direct run formatting (size, bold, italic, underline, colour and family).
  Page size, margins, table grids, cell widths, borders and horizontal/vertical
  merges are read directly from OOXML. Cell alignment has a declared-width
  alignment box even when the source omits a table grid. Automatic line spacing
  uses the font's natural line box. Implicit cell side margins are 108 twips
  (5.4 pt). Installed document fonts are used, and unavailable families fall
  back to Noto Sans/Serif with `docx.missing_font`. Inline PNG/JPEG images and
  plain-text headers/footers are supported.
- **PDF → DOCX** groups text into lines and paragraphs by position. Reading
  order works per page band: full-width lines (title, intro) come first, then
  multi-column blocks column by column. Columns are detected from the vertical
  gutter in that band. Painted grid lines become tables in their own place. A
  grid ends at its connected vertical borders, not at unrelated equal-width rules
  or shaded blocks below it. Simple numeric tables without ruling lines need
  three aligned rows and report `pdfdocx.table_inferred`. Font changes on a
  baseline become separate styled runs in one paragraph, without duplicated
  spaces at the boundaries. Sizes, bold and italic, device RGB/gray/CMYK text
  colours, monospace code and rectangular paragraph shading are kept. Inferred
  headings use outline levels. Printed numeric and bullet prefixes become real
  nested Word numbering with measured hanging indents. URI annotations become
  hyperlinks that keep the painted text colour and underline instead of getting
  a default blue style. Uniform thin filled grid borders keep their measured
  colour and thickness. Row origins use the upper border edges, and row heights
  are rounded to whole Word twips. Legacy Word width balancing is turned off for
  this export to avoid extra gaps around face changes in LibreOffice. Connected
  vector paths outside the table, text backgrounds and underlines are cropped
  from a 216 DPI Poppler render and embedded in `word/media` at their original
  page coordinates and width, in text reading order. This requires `pdftoppm`
  and `bubblewrap`, and there is no unsandboxed fallback. Rendering has a
  60-second deadline and a 40-megapixel page limit (`pdfdocx.raster_limit`). A
  rendering failure returns `pdfdocx.raster_failed` before the destination is
  written. Text inside an illustration is rasterized
  (`pdfdocx.vector_text_rasterized`), not duplicated over the crop. Painted image
  XObjects are embedded into `word/media` too, resolving direct or inherited page
  resources and indirect XObject dictionaries. That includes images inside Forms,
  though their original placement is not reconstructed yet and an explicit
  warning says so. Each page keeps its size and independent ink-derived margins,
  not recovered document metadata. Supported text, simple paths and image bounds
  are measured. Unsupported painting, clipping, font metrics or page geometry
  reports `pdfdocx.bounds_approximate` with `page` and `operator` parameters (one
  warning per distinct operator on each page).
- **Markdown → DOCX** uses an 11 pt sans-serif default, bold 20/16/13 pt
  headings, 6 pt paragraph spacing, real nested Word numbering, repeating
  shaded table headers, column alignment, Quote style, shaded monospace code
  and clickable hyperlinks. Creation and conversion embed readable local PNG/JPEG
  images in both PDF and DOCX, including images with explicit HTML dimensions.
  Numbering IDs don't collide with the builder's defaults, and text replacement
  preserves the source numbering definitions. Markdown PDF uses separate regular,
  bold, italic and bold-italic faces and doesn't repeat nested list text.
- **HTML → PDF** uses browser-like heading, paragraph, list and table defaults,
  quote margins and preformatted monospace text, with author CSS applied after
  the defaults. Documents without author styles use whole-pixel default serif
  line boxes for paragraph, list and table spacing. Styled documents keep
  font-driven normal line heights. When there are no author box-spacing rules or
  inline styles, the collapsing top margin of the body's first list is
  suppressed at the page edge, matching Chromium's print layout.
  Plain `@page` rules support A4/Letter or explicit dimensions and margins.
  Without a rule, the print sheet is Letter with 1 cm margins.
  Default nested lists use vector disc/circle/square markers measured against
  Chromium, without enlarging the item text or changing literal bullet glyphs.
  Ordered lists support decimal, alphabetic and Roman markers, `start`,
  `reversed` (including its implicit item count) and item `value` resets.
  CSS table borders and local images are kept. This native renderer is not
  Chromium, and monospace font selection, font line-box rounding, collapsed
  borders and unsupported CSS can still differ. Complex list selectors and
  inside/RTL marker placement are not browser-complete. Named/pseudo page rules
  and external stylesheets are not supported.
- **HTML → DOCX** keeps supported text structure, hyperlinks and readable local
  images.
- **DOCX → DOCX** keeps the original package and relationships and limits
  oversized drawing extents to the source page's printable area. Other parts stay
  unchanged. This operation requires a distinct output file.

Conversions are not a Word-compatible rendering engine. PDF creation and
conversion to PDF report `convert.layout_approximate`. PDF → DOCX instead uses
positioned, editable paragraphs and tables and reports specific limitations with
the original document's page number, also when reading selected pages.
Paragraph positioning and shading keep their source fragment through layout,
without matching the paragraph text to earlier table cells.

Markdown/HTML/DOCX → PDF/DOCX conversions carry link destinations and raster
display dimensions through a common document model. PDF generation emits
`/Annots` URI annotations at the laid-out text positions, with separate regions
for wrapped lines and styled runs. Repeated labels keep distinct destinations,
and image-only hyperlinks use the image's resource identity and display bounds.
Layout anchors are removed before serialization. A link layout that can't be
represented returns `pdf.link_layout` before writing, instead of silently
dropping destinations. Existing URI annotations in PDF inputs stay available to
PDF readers and to PDF-to-DOCX.

Image dimensions use CSS pixels (96 px/in), and DOCX drawing extents use 9525
EMU/px. HTML width/height attributes and inline CSS override raster dimensions,
and if one axis is omitted it keeps the raster aspect ratio. Images are scaled
down proportionally to the printable page (including its margins). They are not
enlarged or split across pages. Markdown uses A4 with 20 mm margins, HTML PDF
uses its selected `@page` geometry, and DOCX PDF uses the source section
geometry. Local image reads follow the same path/type/size contract as document
reads. Missing or undecodable local images produce `image.unreadable`. Remote and
data images are not fetched and produce `image.remote_not_fetched`. The
historical catalog codes `pdf.links_omitted`, `convert.markdown_images_docx` and
`convert.html_images_docx` remain reserved for compatibility, but these producers
don't emit them.

PDF-to-DOCX warnings:

| Warning | Limitation |
|---|---|
| `pdfdocx.bounds_approximate` | Unsupported painting, clipping, font metrics or page geometry. `operator` identifies the case. |
| `pdfdocx.columns` | Reading order was reconstructed from a two-column gutter. |
| `pdfdocx.table_inferred` | A borderless numeric table was inferred from repeated column alignment. |
| `pdfdocx.font_substituted` | A PDF font family was replaced. The warning includes `font` and `substitute`. |
| `pdfdocx.annotation_unsupported` | A malformed or non-URI annotation could not be preserved. |
| `pdfdocx.link_extent` | A link covers only part of an extracted run, so the whole run is linked. |
| `pdfdocx.vector_geometry` | Vector placement is unsupported for a rotated/scaled/cropped or detached page. `feature` identifies the case. |
| `pdfdocx.vector_text_rasterized` | Text inside an illustration is no longer editable. |
| `pdfdocx.image_at_end` | A raster XObject was moved to the end of the page. |
| `pdfdocx.image_decode_failed` | A raster XObject could not be decoded. |
| `pdf.no_text_page` | The page has no extractable text. |

Arbitrary multi-table layouts, mixed-direction or rotated text, arbitrary list
marker alphabets and PDF font substitution are not a lossless round trip. Other
conversion warnings identify missing fonts or images and plain headers and
footers. Footnotes and captions without tagged source semantics stay positioned,
styled paragraphs, not native Word footnote fields.

## Acceptance scenarios

The acceptance suite uses one generated English quarterly report with H1–H3,
bold and italic text, a URI, bullets, numbering starting at 3, exact amounts,
percentages, dates, a long invoice ID, a sized chart and code. A separate page
checks Polish diacritics. Fixtures are generated, not committed as binaries.

| Scenario | Producer and consumer checks |
| --- | --- |
| S1 Markdown → DOCX | OOXML structure, emphasis, hyperlink relationship, list start, cells, drawing extents and native `read`. |
| S2 Markdown → PDF | Native text, URI annotations, image transform, one-page report and available Poppler text/URL checks. |
| S3 DOCX → PDF | Both the native DOCX and an independent LibreOffice DOCX from the same Markdown-derived HTML: text, numbers, URI and chart size. |
| S4 DOCX → Markdown | Both producers: headings, emphasis, list start, tables and source locators. |
| S5 text PDF → Markdown | Native and LibreOffice PDFs: report text, exact number lexemes and URLs. |
| S6 scans/PNG → text | External English single-column, two-column and Polish pages. PNG and three-page scanned PDF, reading order and confidence data. |

`tests/acceptance.rs` contains one test per scenario and checks that repeated
requests give identical JSON and bytes. Generated PDFs use epoch metadata, stable
first-use resource names and no random trailer IDs. DOCX picture and link IDs are
normalized within each generated package, and relationship and media order stays
stable even across process-counter digit boundaries. Imported package parts are
not normalized during surgical edits. `tests/invariants.rs` replays a fixed,
printed seed across 200 numeric documents and malformed-input classes. Numeric
lexemes are compared as strings, never through floating point. PDF
reconstruction audits its result against the native source text and identifies
omitted or changed numeric lexemes by page, text-line index and token
(`read.numbers_unverified`). Separately, the external 20-page OCR suite requires
exact numeric tokens whenever `ocr.low_confidence` is absent. Confidence is not
proof of correctness on unseen documents.

LibreOffice is required for S3–S5. Poppler text/URL checks are optional, and the
native checks are always active. S6 and the OCR numeric checks print `SKIP` when
models or the runtime aren't configured, and that run is no evidence of OCR
quality. To run them, set `DOCS_TEST_OCR_MODELS`, `DOCS_TEST_OCR_CORPUS` and
`ORT_DYLIB_PATH`.

## Integration

- `docs --schema` returns a form-friendly request schema, with a top-level
  `oneOf` selected by `op`, titles and descriptions from Rust doc comments, and
  no property-level unions. Optional fields are omitted in forms, but the parser
  still accepts legacy `null` selections. File fields carry `x-file` plus
  `x-accept` (input) or `x-formats` (output, primary format first), and array
  items carry the same metadata. Cross-field rules are validated at execution
  time.
- `docs --messages` returns `{"tool":"docs","version":"…","messages":[…]}`.
  Each entry has `code`, `kind` (`summary`, `warning`, `error` or `label`),
  `template` and the required parameter names in `params`, taken directly from
  the single message catalog the renderer uses.
- You can send the same JSON as one argument or on stdin, and the results are
  identical. In a library, `docs::parse_input` gives the same structured
  validation.
- Every result envelope includes `warnings`, even when it's empty. Failures put
  `ok:false`, `summary`, `summary_code`, `summary_params` and `warnings` at the
  top level, with exit status 1. There is no nested error envelope.
- Validation `summary_params.field` is an RFC 6901 JSON Pointer, for example
  `/max_chars`, `/inputs/1`, `/a~1b~0c`, or the empty pointer for the whole
  request. Enum failures include `allowed`, and malformed JSON also includes
  `line`/`column`.
- Explicitly missing model assets use `read.ocr_model_missing` with relative
  `asset`, `stage`, `reason` and the underlying cause category. A missing or
  unconfigured runtime uses `read.ocr_runtime_missing`. Integrity and inference
  failures keep `read.ocr_failed` with structured stage details. Worker failures
  keep a safe `cause_code` and context in the page warning. Library errors keep
  their original `Error::source()` chain.

## Known limits

The scenarios above define the supported acceptance boundary, not a general
Word- or browser-compatible layout engine. Outside that boundary:

- Multiple independent tables or complex vector layouts on a PDF page may be
  reconstructed as illustrations or left out of the Markdown. The existing
  `pdfdocx.vector_text_rasterized`/`read.pdf_structure` warnings describe this,
  and `read.numbers_unverified` also names each affected numeric source
  location. Check the source before you use those numbers.
- Browser and Word spacing and pagination can differ (`convert.layout_approximate`).
  Unsupported PDF geometry or painting uses the specific `pdfdocx.*` warnings
  listed above. Headers, footers and unsupported office objects can be flattened
  (`docx.headers_plain`, `read.flattened`).
- OCR is an estimate (`read.ocr`), with the low-confidence and failed-page
  warnings listed below. Handwriting, rotation and complex merged tables are not
  guaranteed. Remote images are never fetched (`image.remote_not_fetched`), and
  missing local images produce `image.unreadable`.

Markdown with nesting above 64 levels, or above 100,000 parser events/tags, is
rejected with `input.markdown_limit` before recursive layout. The existing PDF,
package and file budgets still apply. A coded warning is not a claim that the
conversion was lossless.

## Example

Input:

```json
{"op":"create","markdown":"# Report\n\nHello **world**.","output":"report.pdf"}
```

Output:

```json
{"ok":true,"output":"report.pdf","bytes":9123,"summary":"created report.pdf from markdown","summary_code":"create.done","summary_params":{"output":"report.pdf"},"warnings":[{"code":"convert.layout_approximate","message":"md -> pdf: layout is reconstructed; spacing, decoration and pagination may differ (margins are estimated for PDF input)","params":{"from":"md","to":"pdf"}}]}
```

Every message comes from one catalog with stable codes, so callers don't have to
parse English text. Besides the rendered `summary`, results carry `summary_code`
and `summary_params`, and `warnings` are objects of the form
`{"code":"pdf.substitute_font","message":"…","params":{"page":1}}`. Errors
expose the same `code()` and `params()`. `Options.locale` selects the
rendering language. Only `en` is built in today, and unknown locales fall
back to it.

`read` adds the extracted Markdown:

```json
{"op":"read","input":"notes.txt","max_chars":20000}
```

```json
{"ok":true,"output":"notes.txt","bytes":13,"summary":"read 2 word(s) from notes.txt","summary_code":"read.words","summary_params":{"words":2,"path":"notes.txt"},"text":"Hello world.\n","warnings":[]}
```

Failures are JSON on stdout with exit code 1, not plain text on stderr:

```json
{"ok":false,"summary":"cannot read missing.pdf","summary_code":"file.read","summary_params":{"field":"/input","path":"missing.pdf","kind":"NotFound"},"warnings":[]}
```

Unknown request fields use `input.unknown_field` with `field` and an `allowed`
array taken from the request schema. File I/O errors include the caller's
relative `path` and the Rust `ErrorKind` name in `kind`. These errors don't
expose the absolute base directory or operating-system error text. Invalid JSON
uses `input.json` with `line` and `column` for syntax errors. Type errors use the
same code with the schema's `field` path (including array indices) and `expected`
type, without echoing the invalid value. Missing required request fields use
`input.missing_field` with `field`. Library failures serialize to the same result
shape, and `Error::to_output(locale)` produces that envelope explicitly. Empty
`warnings` and `summary_params` are always present. Non-UTF-8 arguments return
`cli.argument_encoding` and extra arguments return `cli.arguments`, both with a
field name. `--help` also returns a JSON envelope, and `--schema` returns the
schema.

Diagnostic string parameters are previews capped at 160 Unicode characters,
with a `… [truncated]` suffix. `summary_params.truncated_fields` (or warning/error
`params.truncated_fields`) maps each clipped field to its original character
count. The text being searched or written is not shortened. Read contents and
chunk text still use the explicit `max_chars` limit, while locator metadata uses
the same 160-character previews and `truncated_fields` markers. If the preview
limit is applied again, the first original count is kept instead of counting the
already shortened preview. For example, a quoted 6000-character search has count
6002.

## CLI

```sh
docs '{"op":"read","input":"notes.txt"}'
echo '{"op":"convert","input":"notes.md","output":"notes.pdf"}' | docs
docs --schema    # print the JSON Schema of a request
docs --messages  # print the versioned message catalog
docs --help      # print usage
```

The base directory is the current working directory.

## Library

```rust
use docs::{run, Input, Options};

let options = Options::new(".");
let output = run(
    &Input::Read {
        input: "notes.txt".into(), max_chars: 20_000,
        locators: true, pages: None, range: None, max_rows: 1000,
    },
    &options,
)?;
println!("{}", output.summary);
# Ok::<(), docs::Error>(())
```

`Options` also carries `max_input_bytes` (default 100 MB), `locale` (default
`"en"`) and the `font_path` used to embed a TrueType font when creating or
editing PDFs. Without `font_path`, the first existing system font from a short
built-in list is used. Substitute fonts are picked per style (regular, bold,
italic, bold italic) from the same directories.

## Reading documents

```json
{"op":"read","input":"report.pdf","pages":"2-4","max_chars":12000,"locators":true}
```

With `locators: true`, `data.chunks` contains `{ "text": "…", "locator": {…} }`.
Concatenating the chunk texts reproduces `text` exactly, including truncation and
separators. Locators refer to the original source, not to positions in the output.
Without `locators`, `data` is omitted. Existing JSON requests still work. Rust
constructors need the new `Read` fields and `Options.ocr_models` (or struct defaults).

| Input | Markdown and locator | Selection |
|---|---|---|
| PDF | Inferred headings, paragraphs, lists, grid tables and URI links. Locator `{kind:"page", page:2}` | `pages` or `range`: `"2-4,7"` |
| DOCX | Headings, nested lists, tables, inline emphasis, hyperlinks, footnotes/endnotes and image descriptions. Locator `{kind:"paragraph", paragraph:3, paragraph_end:3, block:3}` | `range`: one-based body blocks (a table is one block). Paragraph numbers include table-cell paragraphs |
| DOC, RTF | Normalized to DOCX with LibreOffice, then the same reader | `range`: normalized body blocks |
| ODT | Headings, inherited bold/italic styles, numbered list starts/item overrides, tables, links, notes and descriptions. Paragraph/block locator | `range`: top-level body blocks |
| HTML, MHTML/MHT | HTML5 content with tables, links and captions, without scripts and styles. Block or MIME-part locator | `range`: Markdown blocks or HTML MIME parts |
| EPUB | EPUB 3 navigation or EPUB 2 NCX contents, then chapters in spine order. Locator `{kind:"chapter", chapter:2, href:"OPS/ch2.xhtml"}` | `range`: spine entries. Contents are retained |
| PPTX, ODP | Slides in presentation order, tables, lists, links, descriptions and speaker notes. Locator `{kind:"slide", slide:2}` | `pages` or `range`: slide numbers |
| XLSX, ODS, CSV | One Markdown table per sheet. Locator `{kind:"sheet", sheet:"Data", range:"B2:D20"}` | `range`: A1 cell rectangle, optionally `"Data!B2:D20"`. CSV uses sheet name `CSV` |
| XMind | JSON and legacy XML packages, nested topic lists and notes. Sheet locator | `range`: sheet numbers |
| TXT, MD | UTF-8 text/Markdown preserved. Block locator | `range`: blank-line-separated blocks |
| PNG, JPEG, WebP, TIFF | OCR text. Locator `{kind:"image", image:1}` | Single image. TIFF reads its first frame |

HTML headings use ATX Markdown (`#` through `######`), including headings that
begin with numbers. Literal paragraph list markers are escaped before Markdown
parsing. DOCX list reads resolve abstract numbering, per-level definitions, start
overrides and counters before applying a block selection.

Ranges are one-based and inclusive. Invalid or inapplicable selections return
`read.range`. Don't combine `pages` and `range`. `max_rows` includes the first
selected row, which becomes the Markdown table header. Empty cells keep their
coordinates, and trailing empty rows and columns are omitted. `max_rows` must be
1–100000, and expanded tables are bounded to one million cells and 100 MB of cell
text.

`max_chars` counts Unicode characters, not bytes. It limits the returned text,
and when nonempty text is cut, a final ellipsis may add one character. Truncation
parameters include `max_chars`, `total_chars`, `omitted_chars` and `returned_chars`.
Selection is applied before this limit. A zero limit returns empty text and chunks.
PDF summaries still report the source's total page count.

### OCR and local tools

OCR uses one engine, PP-OCRv5 mobile detection plus Latin mobile recognition,
through `ort` (CPU). Set `Options.ocr_models` or `DOCS_OCR_MODELS` to the trusted
model directory described below. No models are fetched at runtime or bundled in
Git. Without a configured directory you get `read.ocr_unavailable` and empty text
for images and scans. If a directory is explicitly configured but an asset is
missing, has the wrong size or doesn't match its SHA-256, the request fails with
`read.ocr_model_missing` (missing asset) or `read.ocr_failed` (integrity failure)
before any inference session is created. Files are read once and verified, and
those same bytes are passed to ORT.

Install both executables (`cargo build --release --bins`, or `cargo install
--path . --bins`). `docs` and its private `docs-ocr` helper must be siblings.
Library applications can specify `Options.ocr_worker` explicitly. Supply a trusted
ONNX Runtime shared library with API 24+ via `Options.ocr_runtime` or
`ORT_DYLIB_PATH` (tested with ONNX Runtime 1.31.0). Runtime libraries are
executable code, so never accept their paths or model directories from document
contents. The application does not download or install ONNX Runtime.

Each page runs in an isolated helper with one ORT inference thread. A local,
fallibly created thread pool runs `Options.ocr_threads` workers (default 2,
clamped to 1–4). If thread creation fails, the work falls back to the caller. The
default hard limits are 30 seconds and 2 GiB of virtual address space per worker,
covering model loading, image decoding, PDF rasterization and geometric layout.
Configure them with `ocr_timeout_ms` and `ocr_max_memory_bytes`. Linux `prlimit`
enforces the memory limit, and at the deadline the parent terminates and reaps
the worker's process group. Failed pages are omitted with a numbered warning, and
the other pages continue in their original order. A worker crash, an allocation
failure and a memory-limit exit all share `ocr.page_failed`, because the parent
can't reliably tell those causes apart.

Vertical whitespace separates sustained prose columns, which are read left column
first. Full-width headings separate column bands. Short aligned cells stay
row-major Markdown tables. Simple upright tables and lists are supported. Merged
cells, 90/180-degree rotation, handwriting and complex layouts are not
guaranteed. Scanned PDF→DOCX pages become editable OCR paragraphs and tables, and
native pages keep the existing layout conversion. OCR always carries `read.ocr`.
`data.ocr` includes the original pixel quadrilaterals and the confidence for each
detected line, even without `locators`. Its text previews are limited to 160
characters (`original_chars` reports truncation). Confidence is the mean
probability of the emitted CTC symbols. It is not a calibrated probability that
the line is correct. A page mean below 0.8 (an empty recognition included) gives
`ocr.low_confidence` with the source page number.

#### Reproducible model setup (Apache-2.0)

[`ocr-model-manifest.json`](ocr-model-manifest.json) pins upstream sources,
revisions, byte sizes, SHA-256 hashes and licenses for both weights and the
character dictionary. All three assets are Apache-2.0. Keep the upstream notices
when you redistribute them. To prepare a directory outside this repository (the
setup commands need `curl`, Python and PyYAML, inference does not):

```sh
export DOCS_OCR_MODELS=/path/to/trusted/models
mkdir -p "$DOCS_OCR_MODELS/v5-det" "$DOCS_OCR_MODELS/v5-rec"
DET=e6f4fa85f00e168c862bc462aebca69eef9b3d3d
REC=89d3a50e2c27e2e7cceeab0e944c25c807d5db4f
curl --fail --location "https://huggingface.co/PaddlePaddle/PP-OCRv5_mobile_det_onnx/resolve/$DET/inference.onnx" -o "$DOCS_OCR_MODELS/v5-det/inference.onnx"
curl --fail --location "https://huggingface.co/PaddlePaddle/latin_PP-OCRv5_mobile_rec_onnx/resolve/$REC/inference.onnx" -o "$DOCS_OCR_MODELS/v5-rec/inference.onnx"
curl --fail --location "https://huggingface.co/PaddlePaddle/latin_PP-OCRv5_mobile_rec_onnx/resolve/$REC/inference.yml" -o "$DOCS_OCR_MODELS/v5-rec/inference.yml"
python3 - <<'PY'
import os, pathlib, yaml
root = pathlib.Path(os.environ['DOCS_OCR_MODELS']) / 'v5-rec'
symbols = yaml.safe_load((root / 'inference.yml').read_text())['PostProcess']['character_dict']
(root / 'dict.txt').write_bytes(('\n'.join(symbols) + '\n').encode('utf-8'))
PY
export ORT_DYLIB_PATH=/path/to/trusted/libonnxruntime.so
```

Don't change the assets while requests are running. There is no engine-selection
setting and no alternate OCR backend.

Native readers don't execute macros, extract archive paths or fetch resources.
On Linux, these adapters also need local programs:

- **DOC/RTF:** LibreOffice (`soffice`).
- **Scanned PDF / PDF vector illustrations:** Poppler (`pdftoppm`) rasterizes selected pages.
- **WebP:** ImageMagick (`magick`) decodes the first image to PNG.

These programs run through bubblewrap with no network, private process and user
namespaces, only system libraries and fonts, and a private temporary working
directory. They have a 60-second decoder deadline and can't see the caller's
document directory. OCR subprocesses also inherit the stricter per-page deadline
and memory limit. Missing programs or unavailable user namespaces produce
`read.decoder`, or a numbered `ocr.page_failed` when isolated inside OCR. No
unsandboxed fallback is used. Temporary files are removed after each request.

### Read warnings and limits

| Warning code | When it appears |
|---|---|
| `read.truncated` | Character limit removed text. |
| `read.rows_truncated` | A worksheet contains populated rows past `max_rows`. |
| `read.ocr_unavailable` | No model directory is configured. |
| `read.ocr` | OCR was used. Spelling and structure can be inaccurate. |
| `ocr.page_failed` | A page's helper failed, panicked/crashed, or exceeded its address-space limit. `page`, `memory_bytes` and `timeout_ms` identify the omission and budgets. A parent-task panic reports `page`. |
| `ocr.time_limit` | A page's helper exceeded its deadline and was killed (`page`, `memory_bytes`, `timeout_ms`). |
| `ocr.low_confidence` | Page mean emitted-symbol confidence is below 0.8 (`page`, `confidence`, `threshold`). |
| `read.numbers_unverified` | PDF structural reconstruction changed or omitted a native numeric lexeme. `page`, one-based `line`, bounded `token` and `missing_tokens` identify the source. |
| `read.pdf_structure` | A text PDF page was reconstructed from positions. Semantic headings/footnotes and image descriptions may not be recoverable. URI links are retained, sometimes separately from their text. |
| `read.legacy` | DOC or RTF was normalized through LibreOffice. Layout and objects may change. |
| `read.flattened` | DOCX merged cells or unsupported document objects (headers, footers, charts, equations), spreadsheet formulas (cached values, never evaluation), or TIFF frame limitations cannot be fully represented. |

PDF and directly formatted office headings use font-size heuristics when semantic
styles are absent. Markdown is not a layout-preserving format, so charts,
equations, decorative graphics, slide animations/master styling, headers/footers
and exact numbering/typography are not reconstructed. Tables use a rectangular
Markdown grid, not merged cells. Spreadsheet dates and numbers use stored values
rather than Excel's formatting engine. HTML/EPUB and TXT/MD require UTF-8. MHTML
also supports ASCII, Latin-1 and Windows-1252, base64 and quoted-printable
transfer encoding. Package parts are read in memory with size, checksum and
XML-depth checks. ZIP64 and encrypted packages are not supported, and malformed
data returns `read.invalid`.

### Tests

Fixtures are generated, not checked in. The read suite requires the local tools
above, Python 3 for the sandbox boundary test, and a system TrueType font.
The default suite does not require OCR models. Model-backed tests run
automatically when `DOCS_TEST_OCR_MODELS` is set. Otherwise they print a clear
`SKIP` message (use `-- --nocapture` to see it). They also require
`ORT_DYLIB_PATH` and the installed or built helper. Twenty EN/PL reference pages
cover every corpus category, including two-column pages. Text ground truths,
image hashes and frozen reference predictions are in
`tests/fixtures/ocr-regression.json`, and the images stay external. The corpus
defaults to `../corpus` next to the model directory. Override it with
`DOCS_TEST_OCR_CORPUS`. The Unicode CER test merges whitespace without removing
Polish diacritics, and these fixtures are already NFC. Scores compare raw
recognized lines with the reference, and final-output column order has separate
assertions. Selected Polish single-column pages must have CER below 5%. A
four-page scan is processed twice with 1, 2 and 4 workers and compared as
complete JSON.

```sh
cargo build --bins
```

Then:

```sh
cargo test
DOCS_TEST_OCR_MODELS=/path/to/models ORT_DYLIB_PATH=/path/to/libonnxruntime.so cargo test -- --nocapture
cargo clippy --all-targets -- -D warnings
```

Run the complete local publication gate with `bash scripts/check.sh`. It covers
format, tests and Clippy, runs offline and uses two build jobs. Geometry and list
regressions also require LibreOffice, Poppler, `timeout`, Bash and `mkfifo` on
Linux. If a required renderer or output is unavailable, they fail instead of
silently skipping.

Tests cover every read format, English OCR on rendered images and a scanned PDF,
source locators, selections, Unicode truncation, missing or corrupt models,
malformed packages, repeated and concurrent legacy reads, and network and
filesystem isolation. The MD/HTML/DOCX × PDF/DOCX matrix checks URI destinations,
image payloads and both display dimensions (within 1 CSS pixel, including
page-bounded images). Generated PDFs are read by the native reader, by
`pdfinfo -url` when it's installed, and by Poppler rasterization. Geometry is
measured from the final PDF transforms, not from antialiased edge pixels.

## Development

Developed with AI assistance (Claude, GPT agents) under the maintainer's review.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
