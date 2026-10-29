//! File access through the frontend's virtual file system (`GET_VFS_INTERFACE`), so that content,
//! firmware, and saves work wherever the frontend can reach (e.g. Android storage access paths).
//! Falls back to `std::fs` if the frontend doesn't provide a VFS.

use crate::env;
use crate::ffi::{self, RetroVfsFileHandle, RetroVfsInterface};
use std::ffi::{CString, c_uint};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

static VFS: AtomicPtr<RetroVfsInterface> = AtomicPtr::new(ptr::null_mut());

/// Request the frontend's VFS. libretro asks for this as early as possible, so call it from
/// `retro_set_environment`.
pub fn init() {
    let mut info = ffi::RetroVfsInterfaceInfo {
        required_interface_version: ffi::RETRO_VFS_INTERFACE_VERSION_3,
        iface: ptr::null(),
    };
    // SAFETY: GET_VFS_INTERFACE takes a retro_vfs_interface_info*
    let ok = unsafe {
        env::environment(ffi::RETRO_ENVIRONMENT_GET_VFS_INTERFACE, (&raw mut info).cast())
    };
    let iface = if ok { info.iface.cast_mut() } else { ptr::null_mut() };
    VFS.store(iface, Ordering::Release);
}

fn interface() -> Option<&'static RetroVfsInterface> {
    // SAFETY: The frontend owns the interface and keeps it valid while the core is loaded
    unsafe { VFS.load(Ordering::Acquire).as_ref() }
}

#[must_use]
pub fn is_available() -> bool {
    interface().is_some()
}

fn required<T>(f: Option<T>) -> io::Result<T> {
    f.ok_or_else(|| {
        io::Error::new(io::ErrorKind::Unsupported, "frontend VFS is missing a function")
    })
}

/// libretro paths are UTF-8, NUL-terminated strings
fn c_path(path: &Path) -> io::Result<CString> {
    let path = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not valid UTF-8"))?;
    CString::new(path)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
}

fn vfs_error(action: &str, path: &Path) -> io::Error {
    io::Error::other(format!("unable to {action} '{}'", path.display()))
}

struct VfsFile {
    iface: &'static RetroVfsInterface,
    handle: *mut RetroVfsFileHandle,
}

impl VfsFile {
    fn open(iface: &'static RetroVfsInterface, path: &Path, mode: c_uint) -> io::Result<Self> {
        let open = required(iface.open)?;
        let c_path = c_path(path)?;
        // SAFETY: c_path is a valid NUL-terminated string for the duration of the call
        let handle = unsafe { open(c_path.as_ptr(), mode, ffi::RETRO_VFS_FILE_ACCESS_HINT_NONE) };
        if handle.is_null() {
            return Err(vfs_error("open", path));
        }
        Ok(Self { iface, handle })
    }

    fn size(&self) -> io::Result<u64> {
        let size = required(self.iface.size)?;
        // SAFETY: handle is open until drop
        u64::try_from(unsafe { size(self.handle) }).map_err(|_| io::Error::other("VFS size failed"))
    }
}

impl Read for VfsFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = required(self.iface.read)?;
        // SAFETY: handle is open until drop; buf is valid for buf.len() bytes
        let len = unsafe { read(self.handle, buf.as_mut_ptr().cast(), buf.len() as u64) };
        usize::try_from(len).map_err(|_| io::Error::other("VFS read failed"))
    }
}

impl Write for VfsFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let write = required(self.iface.write)?;
        // SAFETY: handle is open until drop; buf is valid for buf.len() bytes
        let len = unsafe { write(self.handle, buf.as_ptr().cast(), buf.len() as u64) };
        usize::try_from(len).map_err(|_| io::Error::other("VFS write failed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        let flush = required(self.iface.flush)?;
        // SAFETY: handle is open until drop
        match unsafe { flush(self.handle) } {
            0 => Ok(()),
            _ => Err(io::Error::other("VFS flush failed")),
        }
    }
}

impl Seek for VfsFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let seek = required(self.iface.seek)?;
        let (offset, whence) = match pos {
            SeekFrom::Start(offset) => (
                i64::try_from(offset).map_err(|_| io::Error::other("seek offset too large"))?,
                ffi::RETRO_VFS_SEEK_POSITION_START,
            ),
            SeekFrom::Current(offset) => (offset, ffi::RETRO_VFS_SEEK_POSITION_CURRENT),
            SeekFrom::End(offset) => (offset, ffi::RETRO_VFS_SEEK_POSITION_END),
        };
        // SAFETY: handle is open until drop
        let position = unsafe { seek(self.handle, offset, whence) };
        u64::try_from(position).map_err(|_| io::Error::other("VFS seek failed"))
    }
}

impl Drop for VfsFile {
    fn drop(&mut self) {
        if let Some(close) = self.iface.close {
            // SAFETY: handle is open and is not used after this
            unsafe { close(self.handle) };
        }
    }
}

/// Whether `path` is an existing file or directory, according to the VFS.
fn vfs_exists(iface: &RetroVfsInterface, path: &Path) -> io::Result<bool> {
    let stat = required(iface.stat)?;
    let c_path = c_path(path)?;
    // SAFETY: c_path is a valid NUL-terminated string; size may be NULL
    let flags = unsafe { stat(c_path.as_ptr(), ptr::null_mut()) };
    Ok(flags & ffi::RETRO_VFS_STAT_IS_VALID != 0)
}

/// Whether `path` names an existing file or directory.
#[must_use]
pub fn exists(path: &Path) -> bool {
    match interface() {
        Some(iface) => vfs_exists(iface, path).unwrap_or(false),
        None => path.exists(),
    }
}

/// Open an existing file for reading. Checks first, so that a missing file (e.g. no save yet)
/// doesn't make the frontend log an open failure.
fn open_existing(iface: &'static RetroVfsInterface, path: &Path) -> io::Result<VfsFile> {
    if !vfs_exists(iface, path)? {
        return Err(io::Error::new(io::ErrorKind::NotFound, "file not found"));
    }
    VfsFile::open(iface, path, ffi::RETRO_VFS_FILE_ACCESS_READ)
}

pub fn read(path: &Path) -> io::Result<Vec<u8>> {
    let Some(iface) = interface() else { return fs::read(path) };

    let mut file = open_existing(iface, path)?;
    let mut bytes = Vec::with_capacity(file.size().unwrap_or(0).try_into().unwrap_or(0));
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Size of the file at `path`, in bytes.
pub fn file_size(path: &Path) -> io::Result<u64> {
    let Some(iface) = interface() else { return Ok(fs::metadata(path)?.len()) };
    open_existing(iface, path)?.size()
}

pub fn read_to_string(path: &Path) -> io::Result<String> {
    String::from_utf8(read(path)?).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    let Some(iface) = interface() else { return fs::create_dir_all(dir) };
    let mkdir = required(iface.mkdir)?;

    // Create from the outermost missing ancestor inwards
    for dir in dir.ancestors().collect::<Vec<_>>().into_iter().rev() {
        if dir.as_os_str().is_empty() || vfs_exists(iface, dir)? {
            continue;
        }
        let c_dir = c_path(dir)?;
        // SAFETY: c_dir is a valid NUL-terminated string
        match unsafe { mkdir(c_dir.as_ptr()) } {
            // Created, or already exists
            0 | -2 => {}
            _ => return Err(vfs_error("create directory", dir)),
        }
    }
    Ok(())
}

/// Replace the contents of `path` by writing `tmp_path` and renaming it over `path`, so that a
/// failed write never leaves a truncated file behind.
pub fn write_replace(path: &Path, tmp_path: &Path, bytes: &[u8]) -> io::Result<()> {
    let Some(iface) = interface() else {
        fs::write(tmp_path, bytes)?;
        return fs::rename(tmp_path, path);
    };

    {
        let mut file = VfsFile::open(iface, tmp_path, ffi::RETRO_VFS_FILE_ACCESS_WRITE)?;
        file.write_all(bytes)?;
        file.flush()?;
    }

    // VFS rename requires that the destination doesn't exist
    let dest = c_path(path)?;
    let source = c_path(tmp_path)?;
    if vfs_exists(iface, path)? {
        let remove = required(iface.remove)?;
        // SAFETY: dest is a valid NUL-terminated string
        if unsafe { remove(dest.as_ptr()) } != 0 {
            return Err(vfs_error("replace", path));
        }
    }

    let rename = required(iface.rename)?;
    // SAFETY: Both paths are valid NUL-terminated strings
    match unsafe { rename(source.as_ptr(), dest.as_ptr()) } {
        0 => Ok(()),
        _ => Err(vfs_error("rename to", path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_fs_fallback_round_trips() {
        assert!(!is_available());

        let dir = std::env::temp_dir()
            .join(format!("jgenesis-libretro-vfs-{}", std::process::id()))
            .join("nested/saves");
        create_dir_all(&dir).unwrap();

        let path = dir.join("game.sav");
        write_replace(&path, &dir.join("game.tmp"), b"first").unwrap();
        write_replace(&path, &dir.join("game.tmp"), b"second").unwrap();
        assert_eq!(read(&path).unwrap(), b"second");
        assert!(!dir.join("game.tmp").exists());
        assert_eq!(read(&dir.join("missing")).unwrap_err().kind(), io::ErrorKind::NotFound);

        fs::remove_dir_all(dir.parent().unwrap().parent().unwrap()).ok();
    }
}
