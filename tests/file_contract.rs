//! Process-level tests: special inputs must never block, failed writes are atomic.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Dir(PathBuf);
impl Dir {
    fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "docs-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn cli(&self, request: serde_json::Value) -> Result<serde_json::Value> {
        let out = Command::new("timeout")
            .args(["8", env!("CARGO_BIN_EXE_docs"), &request.to_string()])
            .current_dir(&self.0)
            .output()?;
        assert_ne!(out.status.code(), Some(124), "blocked on input");
        assert!(!out.status.success());
        assert!(out.stderr.is_empty(), "{:?}", out.stderr);
        Ok(serde_json::from_slice(&out.stdout)?)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn special_files_are_rejected_without_blocking_for_every_reader() -> Result {
    let dir = Dir::new()?;
    assert!(
        Command::new("mkfifo")
            .arg(dir.0.join("pipe.pdf"))
            .status()?
            .success()
    );
    std::fs::create_dir(dir.0.join("folder.pdf"))?;
    for path in ["pipe.pdf", "folder.pdf"] {
        for req in [
            serde_json::json!({"op":"read","input":path}),
            serde_json::json!({"op":"convert","input":path,"output":"out.docx"}),
            serde_json::json!({"op":"replace","input":path,"output":"out.pdf","find":"a","replace":"b"}),
            serde_json::json!({"op":"merge","inputs":[path],"output":"out.pdf"}),
            serde_json::json!({"op":"split","input":path,"output":"out.pdf","pages":"1"}),
        ] {
            let value = dir.cli(req)?;
            assert_eq!(value["ok"], false);
            assert_eq!(value["summary_code"], "file.not_regular");
            assert_eq!(value["summary_params"]["path"], path);
        }
    }
    Ok(())
}

#[test]
fn non_utf8_argument_is_a_structured_cli_error() -> Result {
    use std::os::unix::ffi::OsStringExt;
    let out = Command::new(env!("CARGO_BIN_EXE_docs"))
        .arg(std::ffi::OsString::from_vec(vec![0xff]))
        .output()?;
    assert!(!out.status.success());
    assert!(out.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(value["ok"], false);
    assert_eq!(value["summary_code"], "cli.argument_encoding");
    assert_eq!(value["summary_params"]["field"], "/arguments/0");
    Ok(())
}

#[test]
fn extra_cli_arguments_report_field_and_allowed_values() -> Result {
    let result = Command::new(env!("CARGO_BIN_EXE_docs"))
        .args(["--schema", "extra"])
        .output()?;
    assert!(!result.status.success());
    assert!(result.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout)?;
    assert_eq!(value["summary_code"], "cli.arguments");
    assert_eq!(value["summary_params"]["field"], "/arguments");
    assert_eq!(
        value["summary_params"]["allowed"],
        serde_json::json!(["json", "--schema", "--help", "--messages"])
    );
    Ok(())
}

#[test]
fn actual_write_failure_preserves_previous_output_and_cleans_temp() -> Result {
    let dir = Dir::new()?;
    std::fs::write(dir.0.join("input.txt"), "a".repeat(4096))?;
    std::fs::write(dir.0.join("output.txt"), b"previous output")?;
    let request = serde_json::json!({"op":"replace","input":"input.txt","output":"output.txt","find":"a","replace":"b"});
    let out = Command::new("bash")
        .args([
            "-c",
            "ulimit -f 1; trap '' XFSZ; exec \"$1\" \"$2\"",
            "docs-write-limit",
            env!("CARGO_BIN_EXE_docs"),
            &request.to_string(),
        ])
        .current_dir(&dir.0)
        .output()?;
    assert!(!out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(value["summary_code"], "file.write");
    assert_eq!(std::fs::read(dir.0.join("output.txt"))?, b"previous output");
    assert_eq!(
        std::fs::read_dir(&dir.0)?.count(),
        2,
        "temporary file leaked"
    );
    Ok(())
}

#[test]
fn parallel_cli_updates_preserve_both_changes() -> Result {
    let dir = Dir::new()?;
    for _ in 0..3 {
        std::fs::write(
            dir.0.join("state.txt"),
            format!("Client amount 100\n{}", "padding ".repeat(500_000)),
        )?;
        let mut children = Vec::new();
        for (find, replacement) in [("Client", "Vendor"), ("100", "200")] {
            let request = serde_json::json!({"op":"replace","input":"state.txt","output":"state.txt","find":find,"replace":replacement});
            children.push(
                Command::new("timeout")
                    .args(["10", env!("CARGO_BIN_EXE_docs"), &request.to_string()])
                    .current_dir(&dir.0)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()?,
            );
        }
        for child in children {
            let result = child.wait_with_output()?;
            assert!(result.status.success(), "{:?}", result);
            let value: serde_json::Value = serde_json::from_slice(&result.stdout)?;
            assert_eq!(value["summary_params"]["count"], 1);
        }
        assert!(std::fs::read_to_string(dir.0.join("state.txt"))?.starts_with("Vendor amount 200"));
    }
    Ok(())
}

#[test]
fn symlink_outputs_are_refused_by_all_writers() -> Result {
    use std::os::unix::fs::symlink;
    let dir = Dir::new()?;
    std::fs::write(dir.0.join("in.txt"), "source")?;
    symlink("absent.pdf", dir.0.join("link.pdf"))?;
    for req in [
        serde_json::json!({"op":"create","markdown":"Hello","output":"link.pdf"}),
        serde_json::json!({"op":"convert","input":"in.txt","output":"link.pdf"}),
        serde_json::json!({"op":"replace","input":"in.txt","output":"link.pdf","find":"source","replace":"new"}),
        serde_json::json!({"op":"merge","inputs":["missing.pdf"],"output":"link.pdf"}),
        serde_json::json!({"op":"split","input":"missing.pdf","output":"link.pdf","pages":"1"}),
    ] {
        assert_eq!(dir.cli(req)?["summary_code"], "path.symlink");
    }
    assert!(!dir.0.join("absent.pdf").exists());
    Ok(())
}
