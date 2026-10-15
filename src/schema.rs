//! Form-friendly presentation of the parser's request schema.
use serde_json::{Value, json};

/// Plain-language label and help text for one operation, as shown in a form.
fn variant_text(op: &str) -> Option<(&'static str, &'static str)> {
    Some(match op {
        "read" => (
            "Read a document",
            "Get the text of a document, with headings, lists and tables kept as simple Markdown formatting.",
        ),
        "convert" => (
            "Convert a file",
            "Save a document in another format. The format comes from the file name ending, such as .pdf or .docx.",
        ),
        "replace" => (
            "Replace text",
            "Find and replace text in a PDF, Word (.docx), Markdown, text or HTML file.",
        ),
        "create" => (
            "Create a document",
            "Make a new Word (.docx) or PDF file from text written in Markdown.",
        ),
        "merge" => ("Merge PDFs", "Combine several PDF files into one."),
        "split" => (
            "Extract pages",
            "Copy chosen pages from a PDF into a new file.",
        ),
        _ => return None,
    })
}

/// Plain-language label and help text for one form field of an operation.
fn field_text(op: &str, name: &str) -> Option<(&'static str, &'static str)> {
    Some(match (op, name) {
        (_, "op") => return variant_text(op),
        ("read", "input") => ("Document", "The file to read."),
        ("convert", "input") => ("Document", "The file to convert."),
        ("replace", "input") => ("Document", "The file to search in."),
        ("split", "input") => ("PDF file", "The PDF to take pages from."),
        ("merge", "inputs") => (
            "PDF files",
            "The PDF files to combine, in the order they should appear.",
        ),
        ("convert", "output") => (
            "Save as",
            "Name for the new file. Its ending, such as .pdf or .docx, sets the format.",
        ),
        ("replace", "output") => ("Save as", "Name for the file with the new text."),
        ("create", "output") => (
            "Save as",
            "Name for the new file. End it with .docx or .pdf.",
        ),
        ("merge", "output") => ("Save as", "Name for the combined PDF."),
        ("split", "output") => ("Save as", "Name for the new PDF."),
        ("read", "max_chars") => (
            "Text limit",
            "Most characters to return. Longer text is cut off and you get a warning.",
        ),
        ("read", "locators") => (
            "Show sources",
            "Split the text into passages and show where each one came from, such as a page or slide.",
        ),
        ("read", "pages") => (
            "Pages",
            "PDF pages or presentation slides to read, such as 1-3,5. Numbering starts at 1.",
        ),
        ("read", "range") => (
            "Part to read",
            "Parts of the document to read, counted from 1, or a spreadsheet range such as Sheet1!A1:C20.",
        ),
        ("read", "max_rows") => (
            "Row limit",
            "Most rows to read from each spreadsheet sheet, counting the header row. Use 1 to 100,000.",
        ),
        ("replace", "find") => ("Find", "The text to look for."),
        ("replace", "replace") => ("Replace with", "The new text."),
        ("replace", "all") => (
            "Replace all",
            "On by default, this replaces every match. Turn it off to replace only the first one.",
        ),
        ("create", "markdown") => (
            "Document text",
            "The text for the new document, written in Markdown. For example, start a line with # to make a heading.",
        ),
        ("split", "pages") => ("Pages", "Pages to copy, such as 1-3,5."),
        _ => return None,
    })
}

pub(crate) fn build() -> Value {
    let mut schema = schemars::schema_for!(crate::Input).to_value();
    schema["title"] = "Document task".into();
    schema["description"] = "Choose what to do with your documents.".into();
    if let Some(variants) = schema["oneOf"].as_array_mut() {
        for variant in variants {
            let op = variant["properties"]["op"]["const"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let description = variant["description"].as_str().unwrap_or("").to_string();
            match variant_text(&op) {
                Some((title, help)) => {
                    variant["title"] = title.into();
                    variant["description"] = help.into();
                }
                None => variant["title"] = description.clone().into(),
            }
            if let Some(properties) = variant["properties"].as_object_mut() {
                for (name, property) in properties {
                    let object = match property.as_object_mut() {
                        Some(o) => o,
                        None => continue,
                    };
                    // Optional fields are omitted, not represented by nullable form unions.
                    if let Some(types) = object.get("type").and_then(Value::as_array) {
                        let kind = types.iter().find(|v| **v != "null").cloned();
                        if let Some(kind) = kind {
                            object.insert("type".into(), kind);
                        }
                    }
                    if object.get("default").is_some_and(Value::is_null) {
                        object.remove("default");
                    }
                    object.remove("format"); // Rust integer storage widths are not UI formats.
                    let doc = object
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or(&description)
                        .to_string();
                    let (title, help) = field_text(&op, name)
                        .map(|(t, d)| (t.to_string(), d.to_string()))
                        .unwrap_or_else(|| (doc.clone(), doc));
                    object.insert("title".into(), title.into());
                    object.insert("description".into(), help.into());
                    if name == "inputs" {
                        property["x-file"] = "input".into();
                        property["x-accept"] = json!(["pdf"]);
                    }
                    let target = if name == "inputs" {
                        property["items"]["title"] = "PDF file".into();
                        property["items"]["description"] = "One PDF to combine.".into();
                        &mut property["items"]
                    } else {
                        property
                    };
                    match name.as_str() {
                        "input" | "inputs" => {
                            target["x-file"] = "input".into();
                            target["x-accept"] = match op.as_str() {
                                "merge" | "split" => json!(["pdf"]),
                                "convert" | "replace" => {
                                    json!(["pdf", "docx", "md", "txt", "html"])
                                }
                                _ => json!([
                                    "pdf", "docx", "md", "txt", "html", "doc", "rtf", "odt", "odp",
                                    "pptx", "xlsx", "ods", "csv", "epub", "xmind", "mhtml", "png",
                                    "jpg", "webp", "tiff"
                                ]),
                            };
                        }
                        "output" => {
                            target["x-file"] = "output".into();
                            target["x-formats"] = match op.as_str() {
                                "merge" | "split" => json!(["pdf"]),
                                "create" => json!(["pdf", "docx"]),
                                "replace" => json!(["pdf", "docx", "md", "txt", "html"]),
                                _ => json!(["pdf", "docx", "md", "txt"]),
                            };
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    schema
}
