use super::*;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn line(text: &str, x: f32, y: f32, confidence: f32) -> Line {
    Line {
        text: text.into(),
        bbox: [[x, y], [x + 180., y], [x + 180., y + 20.], [x, y + 20.]],
        confidence,
    }
}

#[test]
fn ocr_confidence_warns_with_original_page_and_preserves_line_scores() {
    let lines = vec![line("Low confidence text", 0., 0., 0.2)];
    let page = page_result(
        Ok(Recognition {
            text: String::new(),
            lines: lines.clone(),
        }),
        7,
        &Options::default(),
    );
    assert_eq!(page.lines, lines);
    assert_eq!(page.warnings[0].code, MessageCode::OcrLowConfidence);
    assert_eq!(page.warnings[0].params["page"], 7);
    assert!(
        page_result(
            Ok(Recognition {
                text: String::new(),
                lines: vec![line("Good text", 0., 0., 0.99)]
            }),
            7,
            &Options::default()
        )
        .warnings
        .is_empty()
    );
}

#[test]
fn ocr_metadata_bounds_long_line_previews_and_reports_original_length() {
    let lines = vec![line(&"x".repeat(1000), 0., 0., 0.3)];
    let value = metadata(9, &lines);
    assert_eq!(value["page"], 9);
    assert_eq!(value["lines"][0]["text"].as_str().map(str::len), Some(160));
    assert_eq!(value["lines"][0]["original_chars"], 1000);
    assert!(
        value["lines"][0]["confidence"]
            .as_f64()
            .is_some_and(|p| (p - 0.3).abs() < 0.00001)
    );
}

#[test]
fn ocr_pool_survives_page_panic_twice_and_parallel_1_2_4() {
    let check = || {
        let mut expected = None;
        for _ in 0..2 {
            for threads in [1, 2, 4] {
                let results = pages(&[1, 2, 3, 4], threads, |n| {
                    if n == 2 {
                        std::panic::resume_unwind(Box::new("injected page panic"));
                    }
                    Page {
                        text: n.to_string(),
                        ..Page::default()
                    }
                });
                assert_eq!(results.len(), 4);
                assert_eq!(results[1].1.warnings[0].params["page"], 2);
                assert_eq!(results[1].1.warnings[0].code, MessageCode::OcrPageFailed);
                assert_eq!(results[3].1.text, "4");
                let value: Vec<_> = results
                    .into_iter()
                    .map(|(n, p)| (n, p.text, p.warnings))
                    .collect();
                if let Some(expected) = &expected {
                    assert_eq!(expected, &value);
                } else {
                    expected = Some(value);
                }
            }
        }
    };
    std::thread::scope(|s| {
        let other = s.spawn(check);
        check();
        assert!(other.join().is_ok());
    });
}

#[cfg(unix)]
fn fake_worker(script: &str) -> Result<(Scratch, Ocr)> {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new()?;
    scratch.write("worker", script.as_bytes())?;
    let worker = scratch.0.join("worker");
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700))?;
    let ocr = Ocr {
        models: PathBuf::new(),
        runtime: PathBuf::new(),
        worker,
    };
    Ok((scratch, ocr))
}

#[cfg(unix)]
#[test]
fn ocr_worker_deadline_kills_and_reaps_process() -> Result {
    let (_scratch, ocr) = fake_worker("#!/bin/sh\nexec sleep 0.2\n")?;
    let opts = Options {
        ocr_timeout_ms: 10,
        ..Options::default()
    };
    let result = ocr.recognize(b"ignored", &opts);
    assert_eq!(
        result.err().ok_or("missing timeout")?.code(),
        MessageCode::OcrTimeLimit
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn ocr_worker_address_space_limit_is_enforced() -> Result {
    let (_scratch, ocr) = fake_worker("#!/bin/sh\nprintf '{\"text\":\"\",\"lines\":[]}'\n")?;
    assert!(
        ocr.recognize(b"ignored", &Options::default())?
            .lines
            .is_empty()
    );
    let opts = Options {
        ocr_max_memory_bytes: 1,
        ..Options::default()
    };
    assert_eq!(
        ocr.recognize(b"ignored", &opts)
            .err()
            .ok_or("memory limit bypassed")?
            .code(),
        MessageCode::OcrPageFailed
    );
    Ok(())
}

#[test]
fn ocr_model_manifest_rejects_missing_short_oversized_and_changed_bytes() -> Result {
    let asset = Asset {
        path: "fixture".into(),
        bytes: 3,
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
    };
    verify_asset(b"abc", &asset)?;
    for bytes in [b"ab".as_slice(), b"abd", b"abcd"] {
        assert_eq!(
            verify_asset(bytes, &asset)
                .err()
                .ok_or("integrity check bypassed")?
                .code(),
            MessageCode::ReadOcrFailed
        );
    }
    let scratch = Scratch::new()?;
    assert_eq!(
        assets(&scratch.0).err().ok_or("missing accepted")?.code(),
        MessageCode::ReadOcrModelMissing
    );
    let manifest: Vec<Asset> = serde_json::from_str(include_str!("../../ocr-model-manifest.json"))?;
    for size in [
        manifest[0].bytes - 1,
        manifest[0].bytes,
        manifest[0].bytes + 1,
    ] {
        let path = crate::path::resolve(&scratch.0, &manifest[0].path)?;
        std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
        std::fs::write(path, vec![0; size as usize])?;
        assert_eq!(
            assets(&scratch.0)
                .err()
                .ok_or("wrong model accepted")?
                .code(),
            MessageCode::ReadOcrFailed
        );
    }
    Ok(())
}

#[test]
fn ocr_columns_read_left_then_right_but_cells_stay_rows() {
    let mut lines = Vec::new();
    for n in 0..4 {
        lines.push(line(
            &format!("Left paragraph {n} content"),
            10.,
            100. + n as f32 * 50.,
            1.,
        ));
        lines.push(line(
            &format!("Right paragraph {n} content"),
            500.,
            100. + n as f32 * 50.,
            1.,
        ));
    }
    let mut header = line("Full width heading across both columns", 0., 0., 1.);
    header.bbox[1][0] = 900.;
    header.bbox[2][0] = 900.;
    lines.push(header);
    let text = layout::markdown(&lines);
    assert!(text.starts_with("Full width heading"));
    assert!(
        text.find("Left paragraph 3")
            .is_some_and(|i| text.find("Right paragraph 0").is_some_and(|j| i < j)),
        "{text}"
    );
    let mut cells = Vec::new();
    for (n, (label, value)) in [("Name", "Value"), ("North", "42"), ("South", "56")]
        .into_iter()
        .enumerate()
    {
        cells.push(line(label, 10., n as f32 * 30., 1.));
        cells.push(line(value, 500., n as f32 * 30., 1.));
    }
    let text = layout::markdown(&cells);
    assert!(text.contains("| Name | Value |"), "{text}");
    assert!(text.contains("| North | 42 |"), "{text}");
}
