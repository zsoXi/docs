//! Hostile content streams must return structured errors, never abort the CLI.
use lopdf::{Document, dictionary};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Dir(std::path::PathBuf);
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn pdf(content: &[u8]) -> Result<Vec<u8>> {
    let mut doc = Document::with_version("1.5");
    let pages = doc.new_object_id();
    let stream = doc.add_object(lopdf::Stream::new(dictionary! {}, content.to_vec()));
    let page = doc.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),612.into(),792.into()],"Contents"=>stream,"Resources"=>dictionary!{}});
    doc.objects.insert(
        pages,
        dictionary! {"Type"=>"Pages","Count"=>1,"Kids"=>vec![page.into()]}.into(),
    );
    let root = doc.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
    doc.trailer.set("Root", root);
    let mut out = Vec::new();
    doc.save_to(&mut out)?;
    Ok(out)
}
fn check(content: &[u8], kind: &str) -> Result {
    let bytes = pdf(content)?;
    check_bytes(bytes.clone(), kind)?;
    let mut doc = Document::load_mem(&bytes)?;
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    let form = doc.add_object(lopdf::Stream::new(dictionary!{
        "Type"=>"XObject", "Subtype"=>"Form", "BBox"=>vec![0.into(),0.into(),612.into(),792.into()]
    }, content.to_vec()));
    let stream = doc.add_object(lopdf::Stream::new(dictionary! {}, b"/Form1 Do".to_vec()));
    doc.get_dictionary_mut(page)?.set("Contents", stream);
    doc.get_dictionary_mut(page)?.set(
        "Resources",
        dictionary! {"XObject"=>dictionary!{"Form1"=>form}},
    );
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)?;
    check_bytes(bytes, kind)
}
fn check_bytes(bytes: Vec<u8>, kind: &str) -> Result {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = Dir(std::env::temp_dir().join(format!(
        "docs-pdf-limits-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )));
    std::fs::create_dir(&dir.0)?;
    std::fs::write(dir.0.join("input.pdf"), &bytes)?;
    for request in [
        serde_json::json!({"op":"read","input":"input.pdf"}),
        serde_json::json!({"op":"convert","input":"input.pdf","output":"out.docx"}),
        serde_json::json!({"op":"convert","input":"input.pdf","output":"out.txt"}),
        serde_json::json!({"op":"replace","input":"input.pdf","output":"input.pdf","find":"a","replace":"b"}),
    ] {
        let out = Command::new("timeout")
            .args(["10", env!("CARGO_BIN_EXE_docs"), &request.to_string()])
            .current_dir(&dir.0)
            .output()?;
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(out.stderr.is_empty(), "{out:?}");
        let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        assert_eq!(value["ok"], false);
        assert_eq!(value["summary_code"], "pdf.content_limit", "{value}");
        assert_eq!(value["summary_params"]["kind"], kind);
        assert_eq!(std::fs::read(dir.0.join("input.pdf"))?, bytes);
        assert!(!dir.0.join("out.docx").exists());
        assert!(!dir.0.join("out.txt").exists());
    }
    Ok(())
}
#[test]
fn flate_bomb_returns_json_under_768_mib_without_touching_files() -> Result {
    let dir = Dir(std::env::temp_dir().join(format!("docs-flate-bomb-{}", std::process::id())));
    std::fs::create_dir(&dir.0)?;
    let out = Command::new("python3").args(["-c", "import zlib; c=zlib.compressobj(); f=open('payload','wb'); block=b' '*1048576\nfor _ in range(300): f.write(c.compress(block))\nf.write(c.flush())"]).current_dir(&dir.0).output()?;
    assert!(out.status.success(), "{out:?}");
    let mut doc = Document::load_mem(&pdf(b"q Q")?)?;
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    let payload = std::fs::read(dir.0.join("payload"))?;
    assert!(payload.len() < 400_000);
    let stream = doc.add_object(lopdf::Stream::new(
        dictionary! {"Filter"=>"FlateDecode"},
        payload,
    ));
    doc.get_dictionary_mut(page)?.set("Contents", stream);
    doc.save(dir.0.join("input.pdf"))?;
    let before = std::fs::read(dir.0.join("input.pdf"))?;
    for request in [
        serde_json::json!({"op":"read", "input":"input.pdf"}),
        serde_json::json!({"op":"convert", "input":"input.pdf", "output":"out.docx"}),
        serde_json::json!({"op":"replace", "input":"input.pdf", "output":"input.pdf", "find":"a", "replace":"b"}),
    ] {
        let out = Command::new("timeout")
            .args([
                "20",
                "bash",
                "-c",
                "ulimit -v 786432; exec \"$@\"",
                "bomb-test",
                env!("CARGO_BIN_EXE_docs"),
                &request.to_string(),
            ])
            .current_dir(&dir.0)
            .output()?;
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        assert!(out.stderr.is_empty(), "{out:?}");
        let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        assert_eq!(value["summary_code"], "pdf.content_limit", "{value}");
        assert_eq!(value["ok"], false);
        assert_eq!(std::fs::read(dir.0.join("input.pdf"))?, before);
        assert!(!dir.0.join("out.docx").exists());
    }
    Ok(())
}

#[test]
fn document_sum_of_small_streams_is_bounded() -> Result {
    let mut doc = Document::load_mem(&pdf(b"q Q")?)?;
    for _ in 0..9 {
        let mut stream = lopdf::Stream::new(dictionary! {}, vec![b' '; 8 * 1024 * 1024]);
        stream.compress()?;
        doc.add_object(stream);
    }
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)?;
    check_bytes(bytes, "decompressed_bytes")
}

#[test]
fn document_object_count_and_value_depth_are_bounded() -> Result {
    let mut doc = Document::load_mem(&pdf(b"q Q")?)?;
    let mut value = lopdf::Object::Null;
    for _ in 0..34 {
        value = lopdf::Object::Array(vec![value]);
    }
    doc.add_object(value);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)?;
    check_bytes(bytes, "object_depth")?;
    let mut doc = Document::load_mem(&pdf(b"q Q")?)?;
    for _ in 0..100_001 {
        doc.add_object(lopdf::Object::Null);
    }
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)?;
    check_bytes(bytes, "objects")
}

#[test]
fn nested_and_cyclic_forms_succeed_with_named_omissions() -> Result {
    let mut doc = Document::load_mem(&pdf(b"/Next Do")?)?;
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    let mut next = doc.add_object(lopdf::Stream::new(dictionary! {"Type"=>"XObject", "Subtype"=>"Form", "BBox"=>vec![0.into(),0.into(),600.into(),800.into()]}, b"q Q".to_vec()));
    for _ in 0..33 {
        next = doc.add_object(lopdf::Stream::new(dictionary! {"Type"=>"XObject", "Subtype"=>"Form", "BBox"=>vec![0.into(),0.into(),600.into(),800.into()], "Resources"=>dictionary! {"XObject"=>dictionary! {"Next"=>next}}}, b"/Next Do".to_vec()));
    }
    doc.get_dictionary_mut(page)?.set(
        "Resources",
        dictionary! {"XObject"=>dictionary! {"Next"=>next}},
    );
    let dir = Dir(std::env::temp_dir().join(format!("docs-form-omissions-{}", std::process::id())));
    std::fs::create_dir(&dir.0)?;
    for depth in [33, 2] {
        if depth == 2 {
            // One mutation of the fixture: the outer form now calls itself.
            doc.get_object_mut(next)?.as_stream_mut()?.dict.set(
                "Resources",
                dictionary! {"XObject"=>dictionary! {"Next"=>next}},
            );
        }
        doc.save(dir.0.join("input.pdf"))?;
        for request in [
            serde_json::json!({"op":"read","input":"input.pdf"}),
            serde_json::json!({"op":"convert","input":"input.pdf","output":"out.docx"}),
            serde_json::json!({"op":"convert","input":"input.pdf","output":"out.txt"}),
            serde_json::json!({"op":"replace","input":"input.pdf","output":"out.pdf","find":"a","replace":"b"}),
        ] {
            let out = Command::new("timeout")
                .args(["10", env!("CARGO_BIN_EXE_docs"), &request.to_string()])
                .current_dir(&dir.0)
                .output()?;
            assert_eq!(out.status.code(), Some(0), "{out:?}");
            assert!(out.stderr.is_empty(), "{out:?}");
            let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
            assert_eq!(value["ok"], true);
            let skipped: Vec<_> = value["warnings"]
                .as_array()
                .ok_or("warnings")?
                .iter()
                .filter(|w| w["code"] == "pdf.form_skipped")
                .collect();
            assert_eq!(skipped.len(), 1, "{value}");
            assert_eq!(
                skipped[0]["params"],
                serde_json::json!({"page":1,"depth":depth})
            );
        }
    }
    Ok(())
}

#[test]
fn admission_rejects_declarations_before_even_reading_xref() -> Result {
    // Deliberately no usable xref: these must fail at admission, not after a
    // library load has already allocated or swallowed a malformed object.
    for (bytes, kind) in [
        (b"%PDF-1.5\n1 0 obj << /Length 999999999 >> stream\nx\nendstream\nendobj".to_vec(), "encoded_bytes"),
        (format!("%PDF-1.5\n1 0 obj << /Length 1 /Filter [{}] >> stream\nx\nendstream\nendobj", "/FlateDecode ".repeat(33)).into_bytes(), "filters"),
        (b"%PDF-1.5\nxref\n0 999999999\n".to_vec(), "objects"),
        (b"%PDF-1.5\ntrailer << /Size 999999999 >>".to_vec(), "objects"),
        (b"%PDF-1.5\n1 0 obj << /Type /ObjStm /N 999999999 /Length 1 >> stream\nx\nendstream\nendobj".to_vec(), "objects"),
        (b"%PDF-1.5\n1 0 obj << /Type /XRef /Size 1 /Index [0 999999999] /Length 1 >> stream\nx\nendstream\nendobj".to_vec(), "objects"),
    ] {
        check_bytes(bytes, kind)?;
    }
    Ok(())
}

#[test]
fn predictor_metadata_is_bounded_before_codec_arithmetic_or_allocation() -> Result {
    for settings in [
        "/Predictor 12 /Columns 9223372036854775807 /Colors 3 /BitsPerComponent 16",
        "/Predictor 2 /Columns 1 /Colors 1000000000 /BitsPerComponent 1",
    ] {
        check_bytes(format!("%PDF-1.5\n1 0 obj << /Length 1 /Filter /FlateDecode /DecodeParms << {settings} >> >> stream\nx\nendstream\nendobj").into_bytes(), "predictor_bytes")?;
    }
    Ok(())
}

#[test]
fn ordinary_pdf_returns_identical_json_under_memory_and_thread_pressure() -> Result {
    let dir = Dir(std::env::temp_dir().join(format!("docs-no-global-pool-{}", std::process::id())));
    std::fs::create_dir(&dir.0)?;
    let mut doc = Document::load_mem(&pdf(b"BT /F1 12 Tf 50 700 Td (Ordinary text) Tj ET")?)?;
    let font =
        doc.add_object(dictionary! {"Type"=>"Font", "Subtype"=>"Type1", "BaseFont"=>"Helvetica"});
    let page = doc.get_pages().into_values().next().ok_or("page")?;
    doc.get_dictionary_mut(page)?
        .set("Resources", dictionary! {"Font"=>dictionary! {"F1"=>font}});
    doc.save(dir.0.join("input.pdf"))?;
    let request = r#"{"op":"read","input":"input.pdf"}"#;
    let normal = Command::new(env!("CARGO_BIN_EXE_docs"))
        .arg(request)
        .current_dir(&dir.0)
        .output()?;
    assert!(normal.status.success(), "{normal:?}");
    let classic: serde_json::Value = serde_json::from_slice(&normal.stdout)?;
    let mut modern = Vec::new();
    doc.save_modern(&mut modern)?;
    assert!(modern.windows(6).any(|w| w == b"ObjStm"));
    std::fs::write(dir.0.join("input.pdf"), &modern)?;
    let normal = Command::new(env!("CARGO_BIN_EXE_docs"))
        .arg(request)
        .current_dir(&dir.0)
        .output()?;
    assert!(normal.status.success(), "{normal:?}");
    let expected: serde_json::Value = serde_json::from_slice(&normal.stdout)?;
    assert_eq!(expected["text"], classic["text"]);
    assert_eq!(expected["warnings"], classic["warnings"]);
    for _ in 0..2 {
        let limited = Command::new("bash")
            .args([
                "-c",
                "ulimit -v 786432; ulimit -u 1; exec \"$@\"",
                "no-pool-test",
                env!("CARGO_BIN_EXE_docs"),
                request,
            ])
            .env("RAYON_NUM_THREADS", "100000")
            .current_dir(&dir.0)
            .output()?;
        assert!(limited.status.success(), "{limited:?}");
        assert!(limited.stderr.is_empty(), "{limited:?}");
        assert_eq!(limited.stdout, normal.stdout);
    }
    Ok(())
}

#[test]
fn nested_arrays_return_json_instead_of_stack_overflow() -> Result {
    check(
        format!("{}0{} TJ", "[".repeat(512), "]".repeat(512)).as_bytes(),
        "depth",
    )
}
#[test]
fn nested_dictionaries_and_mixed_objects_share_the_depth_budget() -> Result {
    check(
        format!("{}0{} BDC", "<< /K [".repeat(96), "] >>".repeat(96)).as_bytes(),
        "depth",
    )
}
#[test]
fn literal_string_nesting_is_bounded() -> Result {
    check(
        format!("{}x{} Tj", "(".repeat(512), ")".repeat(512)).as_bytes(),
        "depth",
    )
}
#[test]
fn flat_and_dictionary_operands_share_the_count_budget() -> Result {
    for content in [
        format!("[{}] TJ", "0 ".repeat(100_001)),
        format!("<< {} >> BDC", "/K 0 ".repeat(50_001)),
        "0 q ".repeat(100_001),
    ] {
        check(content.as_bytes(), "operands")?;
    }
    Ok(())
}
