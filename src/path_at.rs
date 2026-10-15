//! Minimal Linux descriptor-relative filesystem operations. Keeping these
//! bindings here avoids path resolution through a directory that can be moved.
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

unsafe extern "C" {
    fn openat(fd: i32, path: *const std::ffi::c_char, flags: i32, mode: u32) -> i32;
    fn mkdirat(fd: i32, path: *const std::ffi::c_char, mode: u32) -> i32;
    fn renameat(
        from_fd: i32,
        from: *const std::ffi::c_char,
        to_fd: i32,
        to: *const std::ffi::c_char,
    ) -> i32;
    fn unlinkat(fd: i32, path: *const std::ffi::c_char, flags: i32) -> i32;
}

fn name(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| io::ErrorKind::InvalidInput.into())
}
fn status(result: i32) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
pub(super) fn open(directory: &File, path: &Path, flags: i32, mode: u32) -> io::Result<File> {
    let path = name(path)?;
    // SAFETY: directory is live, path is NUL terminated; openat does not retain
    // its pointer. O_CLOEXEC prevents leaking the owned descriptor to decoders.
    let fd = unsafe { openat(directory.as_raw_fd(), path.as_ptr(), flags | 0x80000, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a successful openat returned a new descriptor owned by this call.
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(super) fn mkdir(directory: &File, path: &Path) -> io::Result<()> {
    let path = name(path)?;
    // SAFETY: the live descriptor and CString remain valid throughout the call.
    status(unsafe { mkdirat(directory.as_raw_fd(), path.as_ptr(), 0o755) })
}
pub(super) fn rename(directory: &File, from: &Path, to: &Path) -> io::Result<()> {
    let from = name(from)?;
    let to = name(to)?;
    // SAFETY: both names and the same pinned directory outlive the call.
    status(unsafe {
        renameat(
            directory.as_raw_fd(),
            from.as_ptr(),
            directory.as_raw_fd(),
            to.as_ptr(),
        )
    })
}
pub(super) fn remove(directory: &File, path: &Path) -> io::Result<()> {
    let path = name(path)?;
    // SAFETY: name and directory are live; flags=0 unlinks only the entry.
    status(unsafe { unlinkat(directory.as_raw_fd(), path.as_ptr(), 0) })
}
