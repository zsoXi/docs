//! Component-checked paths, descriptor-checked reads and atomic writes.

use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::Error;
use crate::messages::{MessageCode, params};

#[cfg(target_os = "linux")]
#[path = "path_at.rs"]
mod at;

fn symlink(rel: &str) -> Error {
    Error::invalid(MessageCode::PathSymlink, params(&[("path", rel.into())]))
}
fn not_regular(rel: &str) -> Error {
    Error::invalid(MessageCode::FileNotRegular, params(&[("path", rel.into())]))
}

/// Check each component, including links whose targets do not exist. Do this
/// before lexical normalization so `link/../file` cannot hide a link.
fn reject_symlinks(path: &Path, rel: &str) -> Result<(), Error> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        prefix.push(component.as_os_str());
        match std::fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(symlink(rel)),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(Error::Read {
                    path: rel.into(),
                    source,
                });
            }
        }
    }
    Ok(())
}

/// Resolve an untrusted relative path without following symbolic links.
pub fn resolve(base: &Path, rel: &str) -> Result<PathBuf, Error> {
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(Error::AbsolutePath { path: rel.into() });
    }
    let base_abs = if base.is_absolute() {
        normalize(base)
    } else {
        let cwd = std::env::current_dir().map_err(|source| Error::Read {
            path: rel.into(),
            source,
        })?;
        normalize(&cwd.join(base))
    };
    let raw = base_abs.join(candidate);
    let joined = normalize(&raw);
    if !joined.starts_with(&base_abs) {
        return Err(Error::PathEscapes { path: rel.into() });
    }
    reject_symlinks(&raw, rel)?;
    Ok(joined)
}

fn check_file(meta: &std::fs::Metadata, rel: &str, max: u64) -> Result<(), Error> {
    if meta.file_type().is_symlink() {
        return Err(symlink(rel));
    }
    if !meta.is_file() {
        return Err(not_regular(rel));
    }
    if meta.len() > max {
        return Err(Error::TooLarge {
            path: rel.into(),
            size: meta.len(),
            limit: max,
        });
    }
    Ok(())
}

// Keep the destination directory alive for openat/renameat. The proc pathname
// is used only for metadata queries; mutations use the directory descriptor.
struct Parent {
    path: PathBuf,
    _directory: Option<std::fs::File>,
}

fn parent_handle(path: &Path, rel: &str, create: bool) -> Result<Parent, Error> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let failure = |source| {
        if create {
            Error::Write {
                path: rel.into(),
                source,
            }
        } else {
            Error::Read {
                path: rel.into(),
                source,
            }
        }
    };
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let mut directory =
            std::fs::File::open(if parent.is_absolute() { "/" } else { "." }).map_err(failure)?;
        for component in parent.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            let next = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
            if create {
                match at::mkdir(&directory, Path::new(name)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(failure(e)),
                }
            }
            // O_RDONLY | O_DIRECTORY | O_NOFOLLOW
            directory =
                at::open(&directory, Path::new(name), 0x10000 | 0x20000, 0).map_err(|e| {
                    if std::fs::symlink_metadata(&next).is_ok_and(|m| m.file_type().is_symlink()) {
                        symlink(rel)
                    } else {
                        failure(e)
                    }
                })?;
        }
        Ok(Parent {
            path: PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())),
            _directory: Some(directory),
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        if create {
            std::fs::create_dir_all(parent).map_err(failure)?;
        }
        Ok(Parent {
            path: parent.into(),
            _directory: None,
        })
    }
}

/// Refuse special files before opening them; verify the opened descriptor too.
/// Reads are bounded even if a regular file grows after its metadata check.
pub fn read_limited(path: &Path, rel: &str, max: u64) -> Result<Vec<u8>, Error> {
    reject_symlinks(path, rel)?;
    let parent = parent_handle(path, rel, false)?;
    let pinned = parent
        .path
        .join(path.file_name().ok_or_else(|| not_regular(rel))?);
    let path = pinned.as_path();
    let failure = |source| Error::Read {
        path: rel.into(),
        source,
    };
    let meta = std::fs::symlink_metadata(path).map_err(failure)?;
    check_file(&meta, rel, max)?;
    #[cfg(target_os = "linux")]
    let opened = {
        let directory = parent
            ._directory
            .as_ref()
            .ok_or_else(|| failure(std::io::ErrorKind::Unsupported.into()))?;
        // O_RDONLY | O_NONBLOCK | O_NOFOLLOW: do not wait on a raced-in FIFO.
        at::open(
            directory,
            Path::new(path.file_name().ok_or_else(|| not_regular(rel))?),
            0x800 | 0x20000,
            0,
        )
    };
    #[cfg(not(target_os = "linux"))]
    let opened = std::fs::OpenOptions::new().read(true).open(path);
    let file = opened.map_err(|e| {
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            symlink(rel)
        } else {
            failure(e)
        }
    })?;
    check_file(&file.metadata().map_err(failure)?, rel, max)?;
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() as u64 > max {
        return Err(Error::TooLarge {
            path: rel.into(),
            size: bytes.len() as u64,
            limit: max,
        });
    }
    Ok(bytes)
}

/// File identity, not spelling: hard links may have different extensions.
pub fn same_file(a: &Path, b: &Path, rel_a: &str, rel_b: &str) -> Result<bool, Error> {
    let left = std::fs::symlink_metadata(a);
    let right = std::fs::symlink_metadata(b);
    match (left, right) {
        (Ok(left), Ok(right)) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                Ok(left.dev() == right.dev() && left.ino() == right.ino())
            }
            #[cfg(not(unix))]
            {
                let _ = (left, right);
                Ok(a == b)
            }
        }
        (Err(e), _) | (_, Err(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        (Err(source), _) => Err(Error::Read {
            path: rel_a.into(),
            source,
        }),
        (_, Err(source)) => Err(Error::Read {
            path: rel_b.into(),
            source,
        }),
    }
}

/// Stage a complete result in the destination directory, sync it, then rename.
/// The old destination is never opened for writing, even through a hard link.
pub fn write(path: &Path, rel: &str, data: &[u8]) -> Result<(), Error> {
    write_using(path, rel, |file| file.write_all(data))
}

fn write_using(
    destination: &Path,
    rel: &str,
    write: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> Result<(), Error> {
    let path = destination;
    reject_symlinks(path, rel)?;
    let failure = |source| Error::Write {
        path: rel.into(),
        source,
    };
    let parent = parent_handle(path, rel, true)?;
    let pinned = parent
        .path
        .join(path.file_name().ok_or_else(|| not_regular(rel))?);
    let path = pinned.as_path();
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        check_file(&meta, rel, u64::MAX)?;
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (temp, mut file) = loop {
        let name = PathBuf::from(format!(
            ".docs-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        #[cfg(target_os = "linux")]
        let opened = {
            let directory = parent
                ._directory
                .as_ref()
                .ok_or_else(|| failure(std::io::ErrorKind::Unsupported.into()))?;
            // O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW
            at::open(directory, &name, 1 | 0x40 | 0x80 | 0x20000, 0o600)
        };
        #[cfg(not(target_os = "linux"))]
        let opened = {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            options.open(parent.path.join(&name))
        };
        match opened {
            Ok(file) => break (name, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(failure(e)),
        }
    };
    let result = (|| {
        write(&mut file).map_err(failure)?;
        file.sync_all().map_err(failure)?;
        if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(symlink(rel));
        }
        #[cfg(target_os = "linux")]
        {
            let directory = parent
                ._directory
                .as_ref()
                .ok_or_else(|| failure(std::io::ErrorKind::Unsupported.into()))?;
            at::rename(
                directory,
                &temp,
                Path::new(path.file_name().ok_or_else(|| not_regular(rel))?),
            )
            .map_err(failure)
        }
        #[cfg(not(target_os = "linux"))]
        std::fs::rename(parent.path.join(&temp), path).map_err(failure)
    })();
    if result.is_err() {
        #[cfg(target_os = "linux")]
        if let Some(directory) = &parent._directory {
            let _ = at::remove(directory, &temp);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = std::fs::remove_file(parent.path.join(&temp));
    }
    result
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::CurDir => {}
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn long_write_stays_in_pinned_directory_during_symlink_swap()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "docs-pinned-write-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let outside = root.join("outside");
            std::fs::create_dir(&outside)?;
            std::fs::write(outside.join("out.txt"), b"outside sentinel")?;
            for index in 0..2 {
                let target = root.join(format!("target{index}"));
                let moved = root.join(format!("moved{index}"));
                std::fs::create_dir(&target)?;
                std::fs::write(target.join("out.txt"), b"previous output")?;
                let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
                let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
                std::thread::scope(|scope| -> Result<(), Box<dyn std::error::Error>> {
                    let destination = target.join("out.txt");
                    let worker = scope.spawn(move || {
                        write_using(&destination, "out.txt", |file| {
                            let block = vec![b'X'; 64 * 1024];
                            file.write_all(&block)?;
                            ready_tx
                                .send(())
                                .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
                            resume_rx
                                .recv()
                                .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
                            for _ in 1..512 {
                                file.write_all(&block)?;
                            }
                            Ok(())
                        })
                    });
                    ready_rx.recv()?;
                    std::fs::rename(&target, &moved)?;
                    symlink(&outside, &target)?;
                    resume_tx.send(())?;
                    worker.join().map_err(|_| "writer thread failed")??;
                    Ok(())
                })?;
                let output = std::fs::read(moved.join("out.txt"))?;
                assert_eq!(output.len(), 32 * 1024 * 1024);
                assert!(output.iter().all(|b| *b == b'X'));
                assert_eq!(std::fs::read(outside.join("out.txt"))?, b"outside sentinel");
                assert_eq!(std::fs::read_dir(&moved)?.count(), 1);
                assert_eq!(std::fs::read_dir(&outside)?.count(), 1);
            }
            Ok(())
        })();
        std::fs::remove_dir_all(&root)?;
        result
    }

    #[test]
    fn rejects_parent_escape() {
        assert!(matches!(
            resolve(Path::new("/tmp/base"), "../secret.txt"),
            Err(Error::PathEscapes { .. })
        ));
    }
    #[test]
    fn rejects_absolute() {
        assert!(matches!(
            resolve(Path::new("/tmp/base"), "/etc/passwd"),
            Err(Error::AbsolutePath { .. })
        ));
    }
    #[test]
    fn accepts_nested_relative() {
        assert_eq!(
            resolve(Path::new("/tmp/base"), "sub/dir/file.txt").ok(),
            Some(PathBuf::from("/tmp/base/sub/dir/file.txt"))
        );
    }
    #[test]
    fn inner_parent_stays_inside() {
        assert_eq!(
            resolve(Path::new("/tmp/base"), "sub/../file.txt").ok(),
            Some(PathBuf::from("/tmp/base/file.txt"))
        );
    }
}
