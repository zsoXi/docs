//! Private OCR helper. The parent owns deadlines, address-space limits and warnings.
fn main() {
    if let Err(error) = docs::ocr_worker() {
        let _ = serde_json::to_writer(std::io::stdout().lock(), &error);
        std::process::exit(1);
    }
}
