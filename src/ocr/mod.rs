//! Verified PP-OCRv5 assets and bounded, per-page process isolation.
mod adapter;
mod layout;
#[cfg(test)]
mod tests;

use crate::messages::{Msg, Params, params};
use crate::read::raster::Scratch;
use crate::{Error, MessageCode, Options};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) use adapter::Line;

/// The one engine contract. Only the v5 Latin adapter implements it.
trait Engine {
    fn recognize(&mut self, image: &image::RgbImage) -> Result<Vec<Line>, adapter::Error>;
}
impl Engine for adapter::PaddleOcr {
    fn recognize(&mut self, image: &image::RgbImage) -> Result<Vec<Line>, adapter::Error> {
        adapter::PaddleOcr::recognize(self, image)
    }
}

#[derive(Deserialize)]
struct Asset {
    path: String,
    bytes: u64,
    sha256: String,
}

fn assets(dir: &Path) -> Result<Vec<Vec<u8>>, Error> {
    let manifest: Vec<Asset> = serde_json::from_str(include_str!("../../ocr-model-manifest.json"))?;
    manifest
        .iter()
        .map(|asset| {
            let path = crate::path::resolve(dir, &asset.path)?;
            // Read once, hash these bytes, and pass these same bytes to ORT.
            let bytes = crate::path::read_limited(&path, &asset.path, asset.bytes)
                .map_err(|e| {
                    let code=if e.params().get("kind").is_some_and(|v|v=="NotFound") { MessageCode::ReadOcrModelMissing } else { MessageCode::ReadOcrFailed };
                    Error::reader(code,e).with_params(params(&[
                        ("stage","model".into()), ("asset",asset.path.clone().into()),
                        ("reason","unreadable_or_oversized".into()),
                    ]))
                })?;
            verify_asset(&bytes, asset)?;
            Ok(bytes)
        })
        .collect()
}

fn verify_asset(bytes: &[u8], asset: &Asset) -> Result<(), Error> {
    if bytes.len() as u64 != asset.bytes || format!("{:x}", Sha256::digest(bytes)) != asset.sha256 {
        return Err(Error::reader(
            MessageCode::ReadOcrFailed,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                crate::messages::OCR_ASSET_FAILURE,
            ),
        ).with_params(params(&[("stage","model".into()),("asset",asset.path.clone().into()),("reason","integrity".into())])));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct WorkerRequest {
    models: PathBuf,
    runtime: PathBuf,
    image: PathBuf,
    pdf_page: Option<usize>,
}

fn decode_scratch(input: &Path) -> Result<Scratch, Error> {
    // Keep decoder files inside the parent's scratch tree. The parent can
    // remove them even after SIGKILL/abort, when the helper cannot run Drop.
    let root = input
        .parent()
        .ok_or_else(|| Error::invalid(MessageCode::ReadOcrFailed, Params::new()))?;
    let path = root.join("decode");
    std::fs::create_dir(&path).map_err(|e| Error::reader(MessageCode::ReadOcrFailed, e))?;
    Ok(Scratch(path))
}

/// Internal entry point for the installed `docs-ocr` helper, not a second engine.
#[doc(hidden)]
pub fn worker() -> Result<(), Error> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or_else(|| Error::invalid(MessageCode::ReadOcrFailed, Params::new()))?;
    let bytes = crate::path::read_limited(Path::new(&path), "request", 65536)?;
    let request: WorkerRequest = serde_json::from_slice(&bytes)?;
    let data = assets(&request.models)?;
    let dictionary =
        std::str::from_utf8(&data[2]).map_err(|e| Error::reader(MessageCode::ReadOcrFailed, e))?;
    let mut engine = adapter::PaddleOcr::load(&request.runtime, &data[0], &data[1], dictionary)
        .map_err(|e| {
            let stage=if matches!(e,adapter::Error::Runtime(_)) { "runtime" } else { "inference" };
            Error::reader(MessageCode::ReadOcrFailed, e).with_params(params(&[("stage",stage.into()),("reason","initialization".into())]))
        })?;
    let bytes = crate::path::read_limited(&request.image, "image", 100 * 1024 * 1024)?;
    let bytes = if let Some(page) = request.pdf_page {
        let scratch = decode_scratch(&request.image)?;
        scratch.write("in.pdf", &bytes)?;
        let number = page.to_string();
        crate::read::raster::decode(
            &scratch,
            "pdftoppm",
            &[
                "-f",
                &number,
                "-l",
                &number,
                "-singlefile",
                "-scale-to",
                "2400",
                "-png",
                "/work/in.pdf",
                "/work/page",
            ],
        )?;
        scratch.read("page.png", 100 * 1024 * 1024)?
    } else if bytes.get(..4) == Some(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        let scratch = decode_scratch(&request.image)?;
        scratch.write("in.webp", &bytes)?;
        crate::read::raster::decode(
            &scratch,
            "magick",
            &[
                "-limit",
                "memory",
                "256MiB",
                "-limit",
                "disk",
                "256MiB",
                "webp:/work/in.webp[0]",
                "/work/out.png",
            ],
        )?;
        scratch.read("out.png", 100 * 1024 * 1024)?
    } else {
        bytes
    };
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(10000);
    limits.max_image_height = Some(10000);
    limits.max_alloc = Some(160 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| Error::reader(MessageCode::ReadInvalid, e))?
        .into_rgb8();
    let lines = Engine::recognize(&mut engine, &image)
        .map_err(|e| Error::reader(MessageCode::ReadOcrFailed, e))?;
    let recognized = Recognition {
        text: layout::markdown(&lines),
        lines,
    };
    serde_json::to_writer(std::io::stdout().lock(), &recognized)?;
    Ok(())
}

pub(crate) struct Ocr {
    models: PathBuf,
    runtime: PathBuf,
    worker: PathBuf,
}

impl Ocr {
    pub(crate) fn new(opts: &Options, warnings: &mut Vec<Msg>) -> Result<Option<Self>, Error> {
        let models = opts
            .ocr_models
            .clone()
            .or_else(|| std::env::var_os("DOCS_OCR_MODELS").map(PathBuf::from));
        let Some(models) = models else {
            warnings.push(Msg::new(MessageCode::ReadOcrUnavailable));
            return Ok(None);
        };
        assets(&models)?;
        let runtime = opts
            .ocr_runtime
            .clone()
            .or_else(|| std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from))
            .ok_or_else(|| Error::invalid(MessageCode::ReadOcrRuntimeMissing, params(&[("stage","runtime".into()),("reason","not_configured".into())])))?;
        std::fs::File::open(&runtime).map_err(|e| {
            let code=if e.kind()==std::io::ErrorKind::NotFound { MessageCode::ReadOcrRuntimeMissing } else { MessageCode::ReadOcrFailed };
            Error::reader(code,e).with_params(params(&[("stage","runtime".into()),("reason","unreadable".into())]))
        })?;
        let worker = match &opts.ocr_worker {
            Some(path) => path.clone(),
            None => {
                let exe = std::env::current_exe()
                    .map_err(|e| Error::reader(MessageCode::ReadOcrFailed, e))?;
                let parent = exe
                    .parent()
                    .ok_or_else(|| Error::invalid(MessageCode::ReadOcrFailed, Params::new()))?;
                let parent = if parent.file_name().is_some_and(|n| n == "deps") {
                    parent.parent().unwrap_or(parent)
                } else {
                    parent
                };
                parent.join("docs-ocr")
            }
        };
        warnings.push(Msg::new(MessageCode::ReadOcr));
        Ok(Some(Self {
            models,
            runtime,
            worker,
        }))
    }

    pub(crate) fn recognize(&self, bytes: &[u8], opts: &Options) -> Result<Recognition, Error> {
        self.recognize_input(bytes, None, opts)
    }

    pub(crate) fn recognize_pdf(
        &self,
        bytes: &[u8],
        page: usize,
        opts: &Options,
    ) -> Result<Recognition, Error> {
        self.recognize_input(bytes, Some(page), opts)
    }

    fn recognize_input(
        &self,
        bytes: &[u8],
        pdf_page: Option<usize>,
        opts: &Options,
    ) -> Result<Recognition, Error> {
        let scratch = Scratch::new()?;
        scratch.write("image", bytes)?;
        scratch.write(
            "request.json",
            &serde_json::to_vec(&WorkerRequest {
                models: self.models.clone(),
                runtime: self.runtime.clone(),
                image: scratch.0.join("image"),
                pdf_page,
            })?,
        )?;
        let output = std::fs::File::create(scratch.0.join("lines.json"))
            .map_err(|e| Error::reader(MessageCode::ReadOcrFailed, e))?;
        let mut command = Command::new("prlimit");
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .arg(format!("--as={}", opts.ocr_max_memory_bytes))
            .arg("--")
            .arg(&self.worker)
            .arg(scratch.0.join("request.json"))
            .env("RAYON_NUM_THREADS", "1")
            .stdin(Stdio::null())
            .stdout(output)
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::reader(MessageCode::OcrPageFailed, e))?;
        let start = Instant::now();
        loop {
            if start.elapsed() >= Duration::from_millis(opts.ocr_timeout_ms) {
                kill_worker(&mut child);
                return Err(Error::invalid(MessageCode::OcrTimeLimit, Params::new()));
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    kill_worker(&mut child);
                    if status.success() {
                        break;
                    }
                    // A normally exiting helper reports a safe error envelope.
                    // Crashes/limits can leave no JSON; keep that distinct fallback.
                    let diagnostic = scratch.read("lines.json", 8 * 1024 * 1024)?;
                    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&diagnostic)
                        && value["ok"] == false
                        && let Some(code) = MessageCode::ALL.iter().find(|c| Some(c.as_str()) == value["summary_code"].as_str())
                        && let Ok(context) = serde_json::from_value::<Params>(value["summary_params"].clone())
                    {
                        return Err(Error::reader(*code,std::io::Error::other(crate::messages::OCR_INFERENCE_FAILURE)).with_params(context));
                    }
                    return Err(Error::invalid(
                        MessageCode::OcrPageFailed,
                        params(&[("reason", "worker_exit_or_memory_limit".into())]),
                    ));
                }
                Ok(None) => {}
                Err(e) => {
                    kill_worker(&mut child);
                    return Err(Error::reader(MessageCode::OcrPageFailed, e));
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(serde_json::from_slice(
            &scratch.read("lines.json", 8 * 1024 * 1024)?,
        )?)
    }
}

fn kill_worker(child: &mut std::process::Child) {
    // The helper owns a process group, including its sandboxed rasterizer.
    // Terminate the whole group before reaping, never leave detached inference.
    #[cfg(unix)]
    {
        let _ = Command::new("/usr/bin/kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn pdf_pages(
    bytes: &[u8],
    indices: &[usize],
    opts: &Options,
    warnings: &mut Vec<Msg>,
) -> Result<Vec<(usize, Page)>, Error> {
    if indices.is_empty() {
        return Ok(Vec::new());
    }
    let Some(ocr) = Ocr::new(opts, warnings)? else {
        return Ok(Vec::new());
    };
    Ok(pages(indices, opts.ocr_threads, |index| {
        page_result(ocr.recognize_pdf(bytes, index, opts), index, opts)
    }))
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Recognition {
    pub text: String,
    pub lines: Vec<Line>,
}

#[derive(Default)]
pub(crate) struct Page {
    pub text: String,
    pub lines: Vec<Line>,
    pub warnings: Vec<Msg>,
}

pub(crate) fn metadata(page: usize, lines: &[Line]) -> serde_json::Value {
    let lines: Vec<_> = lines.iter().map(|line| {
        let mut value = serde_json::json!({"text":line.text.chars().take(160).collect::<String>(), "bbox":line.bbox, "confidence":line.confidence});
        let count = line.text.chars().count();
        if count > 160 { value["original_chars"] = count.into(); }
        value
    }).collect();
    serde_json::json!({"page":page,"lines":lines})
}

pub(crate) fn page_result(result: Result<Recognition, Error>, page: usize, opts: &Options) -> Page {
    match result {
        Ok(Recognition { text, lines }) => {
            let mut warnings = Vec::new();
            let mean = if lines.is_empty() {
                0.0
            } else {
                lines.iter().map(|l| l.confidence).sum::<f32>() / lines.len() as f32
            };
            if mean < 0.8 {
                warnings.push(Msg::with(
                    MessageCode::OcrLowConfidence,
                    params(&[
                        ("page", page.into()),
                        ("confidence", mean.into()),
                        ("threshold", 0.8.into()),
                    ]),
                ));
            }
            Page {
                text,
                lines,
                warnings,
            }
        }
        Err(error) => {
            let code = if error.code() == MessageCode::OcrTimeLimit {
                MessageCode::OcrTimeLimit
            } else {
                MessageCode::OcrPageFailed
            };
            let mut context = error.params();
            context.extend(params(&[
                ("page", page.into()),
                ("memory_bytes", opts.ocr_max_memory_bytes.into()),
                ("timeout_ms", opts.ocr_timeout_ms.into()),
                ("cause_code", error.code().as_str().into()),
            ]));
            Page {
                warnings: vec![Msg::with(code, context)],
                ..Page::default()
            }
        }
    }
}

/// Fallible local threads, no global pool. Each task is caught outside any lock.
/// Spawn failure falls back to the caller; the atomic index prevents duplicates.
pub(crate) fn pages<F>(indices: &[usize], threads: usize, task: F) -> Vec<(usize, Page)>
where
    F: Fn(usize) -> Page + Sync,
{
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::new());
    let work = || {
        while let Some(&page) = indices.get(next.fetch_add(1, Ordering::Relaxed)) {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| task(page)))
                .unwrap_or_else(|_| Page {
                    warnings: vec![Msg::with(
                        MessageCode::OcrPageFailed,
                        params(&[("page", page.into())]),
                    )],
                    ..Page::default()
                });
            // Only result insertion executes under the lock, never page processing.
            let mut results = results.lock().unwrap_or_else(|e| e.into_inner());
            results.push((page, result));
        }
    };
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 1..threads.clamp(1, 4) {
            match std::thread::Builder::new().spawn_scoped(scope, work) {
                Ok(handle) => handles.push(handle),
                Err(_) => break,
            }
        }
        work();
        for handle in handles {
            let _ = handle.join();
        }
    });
    // Only Vec::push runs under the lock; unwinding cannot invalidate existing entries.
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|(page, _)| *page);
    results
}
