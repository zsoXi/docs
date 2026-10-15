//! Model tests opt in automatically. Only text fixtures are stored in Git.
use docs::{Input, Options, run};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize)]
struct Fixture {
    path: String,
    kind: String,
    lang: String,
    text: String,
    sha256: String,
    pilot_text: String,
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "docs-ocr-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn options(dir: &Scratch) -> Option<Options> {
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        eprintln!("SKIP: set ORT_DYLIB_PATH to run OCR model tests");
        return None;
    }
    let Some(models) = std::env::var_os("DOCS_TEST_OCR_MODELS") else {
        eprintln!("SKIP: set DOCS_TEST_OCR_MODELS to run OCR model tests");
        return None;
    };
    Some(Options {
        ocr_models: Some(models.into()),
        ocr_worker: Some(env!("CARGO_BIN_EXE_docs-ocr").into()),
        ..Options::new(&dir.0)
    })
}
fn read(opts: &Options, name: &str) -> Result<docs::Output> {
    Ok(run(
        &Input::Read {
            input: name.into(),
            max_chars: 20000,
            locators: true,
            pages: None,
            range: None,
            max_rows: 1000,
        },
        opts,
    )?)
}
fn normalized(s: &str) -> Vec<char> {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect()
}
fn distance(a: &str, b: &str) -> usize {
    let a = normalized(a);
    let b = normalized(b);
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let old = row[j + 1];
            row[j + 1] = (row[j] + 1)
                .min(old + 1)
                .min(diagonal + usize::from(x != y));
            diagonal = old;
        }
    }
    row[b.len()]
}

#[test]
fn ocr_twenty_pilot_pages_do_not_regress_and_columns_read_in_order() -> Result {
    let dir = Scratch::new()?;
    let Some(opts) = options(&dir) else {
        return Ok(());
    };
    let root = opts
        .ocr_models
        .as_ref()
        .ok_or("models")?
        .parent()
        .ok_or("model parent")?;
    let corpus = std::env::var_os("DOCS_TEST_OCR_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("corpus"));
    let fixtures: Vec<Fixture> =
        serde_json::from_str(include_str!("fixtures/ocr-regression.json"))?;
    assert_eq!(fixtures.len(), 20);
    for fixture in fixtures {
        let name = PathBuf::from(&fixture.path)
            .file_name()
            .ok_or("image name")?
            .to_owned();
        let bytes = std::fs::read(corpus.join(&name))?;
        assert_eq!(
            format!("{:x}", Sha256::digest(&bytes)),
            fixture.sha256,
            "fixture bytes changed"
        );
        std::fs::write(dir.0.join(&name), bytes)?;
        let out = read(&opts, name.to_str().ok_or("name")?)?;
        assert!(
            !out.warnings
                .iter()
                .any(|w| matches!(w.code.as_str(), "ocr.page_failed" | "ocr.time_limit")),
            "{}: {:?}",
            fixture.path,
            out.warnings
        );
        let data = out.data.as_ref().ok_or("missing OCR data")?;
        let lines = data["ocr"][0]["lines"].as_array().ok_or("lines")?;
        let text = lines
            .iter()
            .map(|l| l["text"].as_str().ok_or("line text"))
            .collect::<std::result::Result<Vec<_>, _>>()?
            .join("\n");
        for line in lines {
            assert!(
                line["confidence"]
                    .as_f64()
                    .is_some_and(|p| (0.0..=1.0).contains(&p))
            );
        }
        let errors = distance(&fixture.text, &text);
        let baseline = distance(&fixture.text, &fixture.pilot_text);
        eprintln!(
            "{}: errors {errors}, pilot {baseline}, chars {}",
            fixture.path,
            normalized(&fixture.text).len()
        );
        assert!(
            errors <= baseline,
            "{}: CER regressed: {errors} > {baseline}",
            fixture.path
        );
        if fixture.lang == "pl" && fixture.kind != "columns" {
            assert!(
                (errors as f64 / normalized(&fixture.text).len() as f64) < 0.05,
                "Polish print CER must be below 5%"
            );
            if fixture.kind == "print" {
                assert!(text.contains("Łódź"));
            }
        }
        if fixture.kind == "columns" {
            let text = out.text.as_deref().ok_or("output text")?;
            let left_end = text.find("1.12").ok_or("last left paragraph")?;
            let right_start = text.find("2.1").ok_or("first right paragraph")?;
            assert!(
                left_end < right_start,
                "{}: columns were interleaved: {text}",
                fixture.path
            );
            let layout_errors = distance(&fixture.text, text);
            eprintln!(
                "{}: final column-order errors {layout_errors}, pilot {baseline}",
                fixture.path
            );
            assert!(
                layout_errors < baseline,
                "column reconstruction must improve the pilot's row-major CER"
            );
        }
    }
    Ok(())
}

#[test]
fn ocr_real_pdf_is_deterministic_twice_with_1_2_4_workers() -> Result {
    use lopdf::{Object, Stream, dictionary};
    let dir = Scratch::new()?;
    let Some(mut opts) = options(&dir) else {
        return Ok(());
    };
    let corpus = opts
        .ocr_models
        .as_ref()
        .ok_or("models")?
        .parent()
        .ok_or("parent")?
        .join("corpus");
    let corpus = std::env::var_os("DOCS_TEST_OCR_CORPUS")
        .map(PathBuf::from)
        .unwrap_or(corpus);
    let image = image::open(corpus.join("print-pl-01.png"))?.into_rgb8();
    let mut pdf = lopdf::Document::with_version("1.5");
    let pages = pdf.new_object_id();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95).encode_image(&image)?;
    let image_id = pdf.add_object(Stream::new(dictionary! {"Type"=>"XObject","Subtype"=>"Image","Width"=>image.width(),"Height"=>image.height(),"ColorSpace"=>"DeviceRGB","BitsPerComponent"=>8,"Filter"=>"DCTDecode"},jpeg));
    let content = pdf.add_object(Stream::new(
        dictionary! {},
        b"q 600 0 0 800 0 0 cm /Im Do Q".to_vec(),
    ));
    let mut ids = Vec::new();
    for _ in 0..4 {
        ids.push(Object::Reference(pdf.add_object(dictionary! {"Type"=>"Page","Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),600.into(),800.into()],"Resources"=>dictionary! {"XObject"=>dictionary! {"Im"=>image_id}},"Contents"=>content})));
    }
    pdf.objects.insert(
        pages,
        Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>ids,"Count"=>4}),
    );
    let catalog = pdf.add_object(dictionary! {"Type"=>"Catalog","Pages"=>pages});
    pdf.trailer.set("Root", catalog);
    pdf.compress();
    pdf.save(dir.0.join("scans.pdf"))?;
    let converted = run(
        &Input::Convert {
            input: "scans.pdf".into(),
            output: "editable.docx".into(),
        },
        &opts,
    )?;
    assert!(
        converted
            .data
            .as_ref()
            .is_some_and(|d| d["ocr"].as_array().is_some_and(|p| p.len() == 4))
    );
    assert!(
        !converted
            .warnings
            .iter()
            .any(|w| matches!(w.code.as_str(), "ocr.page_failed" | "ocr.time_limit"))
    );
    assert!(
        read(&opts, "editable.docx")?
            .text
            .as_deref()
            .is_some_and(|t| t.contains("Łódź")),
        "scan DOCX must contain editable recognized text"
    );
    let mut expected = None;
    for _ in 0..2 {
        for threads in [1, 2, 4] {
            opts.ocr_threads = threads;
            let out = read(&opts, "scans.pdf")?;
            assert!(
                !out.warnings
                    .iter()
                    .any(|w| matches!(w.code.as_str(), "ocr.page_failed" | "ocr.time_limit"))
            );
            let data = out.data.as_ref().ok_or("data")?;
            assert_eq!(data["ocr"].as_array().ok_or("ocr pages")?.len(), 4);
            let value = serde_json::to_value(out)?;
            if let Some(expected) = &expected {
                assert_eq!(expected, &value);
            } else {
                expected = Some(value);
            }
        }
    }
    // A failed middle page must not discard either neighboring page.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let helper = dir.0.join("fake-worker");
        std::fs::write(
            &helper,
            "#!/usr/bin/python3\nimport json,sys\np=json.load(open(sys.argv[1]))['pdf_page']\nif p==2: sys.exit(1)\ntext=f'Page {p}'+('x'*1000 if p==4 else '')\nprint(json.dumps(dict(text=text,lines=[dict(text=text,bbox=[[0,0],[100,0],[100,20],[0,20]],confidence=0.1 if p==3 else 1.0)])))\n",
        )?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
        opts.ocr_worker = Some(helper);
        let out = read(&opts, "scans.pdf")?;
        let text = out.text.as_deref().ok_or("remaining pages lost")?;
        assert!(text.contains("Page 1") && text.contains("Page 3") && text.contains("Page 4"));
        assert!(!text.contains("Page 2"));
        assert!(
            out.warnings
                .iter()
                .any(|w| w.code == "ocr.page_failed" && w.params["page"] == 2)
        );
        assert!(
            out.warnings
                .iter()
                .any(|w| w.code == "ocr.low_confidence" && w.params["page"] == 3)
        );
        assert!(out.data.as_ref().is_some_and(|d| {
            d["ocr"][2]["lines"][0]["confidence"]
                .as_f64()
                .is_some_and(|p| (p - 0.1).abs() < 0.00001)
        }));
        assert!(out.data.as_ref().is_some_and(|d| {
            let line = &d["ocr"][3]["lines"][0];
            line["text"]
                .as_str()
                .is_some_and(|text| text.chars().count() == 160)
                && line["original_chars"] == 1006
        }));
        opts.ocr_max_memory_bytes = 1;
        let out = read(&opts, "scans.pdf")?;
        assert_eq!(
            out.warnings
                .iter()
                .filter(|w| w.code == "ocr.page_failed")
                .count(),
            4
        );
        opts.ocr_max_memory_bytes = Options::default().ocr_max_memory_bytes;
        opts.ocr_timeout_ms = 0;
        let out = read(&opts, "scans.pdf")?;
        assert_eq!(
            out.warnings
                .iter()
                .filter(|w| w.code == "ocr.time_limit")
                .count(),
            4
        );
    }
    Ok(())
}
