//! Host side of virtio-fs: every file operation the guest asks for, done
//! relative to the share's root directory fd without following symlinks.
//!
//! Paths are only bookkeeping. Each operation re-opens the parent directory by
//! walking from the root fd one component at a time with O_NOFOLLOW, then acts
//! on the final name with the *at() calls. A symlink anywhere in the path (one
//! the guest planted, or one swapped in mid-operation) makes the walk fail
//! instead of leading outside the share. Symlinks themselves still work for
//! the guest: it resolves them in its own namespace.
//!
//! An optional allowlist narrows the share further: only paths under an
//! allowed directory are visible; their ancestors can be traversed (and list
//! only the entries leading to an allowed directory) but not modified.

use std::ffi::{c_char, c_int, c_uint, CString, OsStr};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

extern "C" {
    fn renameatx_np(fromfd: c_int, from: *const c_char, tofd: c_int, to: *const c_char, flags: c_uint) -> c_int;
}

/// Errno from the last failed call, translated to the guest's (Linux) value.
fn err() -> i32 {
    super::fs::linux_errno(&std::io::Error::last_os_error())
}

fn check(rc: c_int) -> Result<(), i32> {
    if rc == 0 {
        Ok(())
    } else {
        Err(err())
    }
}

/// The fields of `struct stat` the FUSE attr reply needs.
#[derive(Clone, Copy, Debug, Default)]
pub struct Attr {
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: i64,
    pub atime_ns: u32,
    pub mtime: i64,
    pub mtime_ns: u32,
    pub ctime: i64,
    pub ctime_ns: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub blksize: u32,
}

impl Attr {
    fn from_stat(st: &libc::stat) -> Self {
        Attr {
            ino: st.st_ino,
            size: st.st_size as u64,
            blocks: st.st_blocks as u64,
            atime: st.st_atime,
            atime_ns: st.st_atime_nsec as u32,
            mtime: st.st_mtime,
            mtime_ns: st.st_mtime_nsec as u32,
            ctime: st.st_ctime,
            ctime_ns: st.st_ctime_nsec as u32,
            mode: st.st_mode as u32,
            nlink: st.st_nlink as u32,
            uid: st.st_uid,
            gid: st.st_gid,
            rdev: st.st_rdev as u32,
            blksize: st.st_blksize as u32,
        }
    }
}

fn cname(name: &OsStr) -> Result<CString, i32> {
    CString::new(name.as_bytes()).map_err(|_| libc::EINVAL)
}

fn openat(dir: RawFd, name: &CString, flags: c_int, mode: c_uint) -> Result<OwnedFd, i32> {
    let fd = unsafe { libc::openat(dir, name.as_ptr(), flags | libc::O_CLOEXEC, mode) };
    if fd < 0 {
        Err(err())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

pub fn fstatat(dir: RawFd, name: &CString) -> Result<Attr, i32> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) })?;
    Ok(Attr::from_stat(&st))
}

pub fn fstat(f: &impl AsRawFd) -> Result<Attr, i32> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    check(unsafe { libc::fstat(f.as_raw_fd(), &mut st) })?;
    Ok(Attr::from_stat(&st))
}

pub enum Access {
    /// Inside an allowed directory (or no allowlist): everything goes.
    Full,
    /// An ancestor of an allowed directory: may be looked up and listed.
    Traverse,
    Denied,
}

pub struct Root {
    path: PathBuf,
    fd: OwnedFd,
    allow: Vec<PathBuf>,
}

impl Root {
    pub fn open(path: &Path, allow: &[PathBuf]) -> Result<Self, String> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("opening share {}: {}", path.display(), std::io::Error::last_os_error()));
        }
        let mut allowed = Vec::new();
        for a in allow {
            // Allowed directories must exist and lie inside the share.
            match a.canonicalize() {
                Ok(c) if c.starts_with(path) => allowed.push(c),
                Ok(c) => return Err(format!("allowed path {} is outside the share {}", c.display(), path.display())),
                Err(e) => return Err(format!("allowed path {}: {e}", a.display())),
            }
        }
        Ok(Root { path: path.to_path_buf(), fd: unsafe { OwnedFd::from_raw_fd(fd) }, allow: allowed })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn access(&self, p: &Path) -> Access {
        if self.allow.is_empty() || self.allow.iter().any(|a| p.starts_with(a)) {
            Access::Full
        } else if self.allow.iter().any(|a| a.starts_with(p)) {
            Access::Traverse
        } else {
            Access::Denied
        }
    }

    /// Components of `p` below the share root (no "..", no ".", no root).
    fn components<'a>(&self, p: &'a Path) -> Result<Vec<&'a OsStr>, i32> {
        let rel = p.strip_prefix(&self.path).map_err(|_| libc::EACCES)?;
        rel.components()
            .map(|c| match c {
                Component::Normal(n) => Ok(n),
                _ => Err(libc::EACCES),
            })
            .collect()
    }

    /// Opens the directory `p` itself, walking from the root with O_NOFOLLOW.
    pub fn open_dir(&self, p: &Path) -> Result<OwnedFd, i32> {
        let mut cur = openat(self.fd.as_raw_fd(), &c".".to_owned(), libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
        for comp in self.components(p)? {
            cur = openat(cur.as_raw_fd(), &cname(comp)?, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, 0)?;
        }
        Ok(cur)
    }

    /// The parent directory of `p` (opened symlink-safely) and `p`'s final name.
    /// For the share root itself: the root and ".".
    pub fn parent(&self, p: &Path) -> Result<(OwnedFd, CString), i32> {
        let comps = self.components(p)?;
        match comps.split_last() {
            None => Ok((self.open_dir(&self.path)?, c".".to_owned())),
            Some((last, _)) => Ok((self.open_dir(p.parent().ok_or(libc::EACCES)?)?, cname(last)?)),
        }
    }

    pub fn lstat(&self, p: &Path) -> Result<Attr, i32> {
        let (dir, name) = self.parent(p)?;
        fstatat(dir.as_raw_fd(), &name)
    }

    /// open(2) on the final component, never following a symlink there either.
    pub fn open_file(&self, p: &Path, flags: c_int, mode: u32) -> Result<File, i32> {
        let (dir, name) = self.parent(p)?;
        let fd = openat(dir.as_raw_fd(), &name, flags | libc::O_NOFOLLOW, mode)?;
        Ok(File::from(fd))
    }

    pub fn mkdir(&self, p: &Path, mode: u32) -> Result<(), i32> {
        let (dir, name) = self.parent(p)?;
        check(unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), mode as libc::mode_t) })
    }

    pub fn unlink(&self, p: &Path, dir_flag: bool) -> Result<(), i32> {
        let (dir, name) = self.parent(p)?;
        check(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), if dir_flag { libc::AT_REMOVEDIR } else { 0 }) })
    }

    pub fn symlink(&self, target: &OsStr, p: &Path) -> Result<(), i32> {
        let (dir, name) = self.parent(p)?;
        let t = cname(target)?;
        check(unsafe { libc::symlinkat(t.as_ptr(), dir.as_raw_fd(), name.as_ptr()) })
    }

    pub fn link(&self, src: &Path, dst: &Path) -> Result<(), i32> {
        let (sdir, sname) = self.parent(src)?;
        let (ddir, dname) = self.parent(dst)?;
        check(unsafe { libc::linkat(sdir.as_raw_fd(), sname.as_ptr(), ddir.as_raw_fd(), dname.as_ptr(), 0) })
    }

    pub fn readlink(&self, p: &Path) -> Result<Vec<u8>, i32> {
        let (dir, name) = self.parent(p)?;
        let mut buf = vec![0u8; libc::PATH_MAX as usize];
        let n = unsafe { libc::readlinkat(dir.as_raw_fd(), name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(err());
        }
        buf.truncate(n as usize);
        Ok(buf)
    }

    /// renameatx_np with Darwin flags (RENAME_EXCL / RENAME_SWAP).
    pub fn rename(&self, from: &Path, to: &Path, darwin_flags: u32) -> Result<(), i32> {
        let (fdir, fname) = self.parent(from)?;
        let (tdir, tname) = self.parent(to)?;
        check(unsafe { renameatx_np(fdir.as_raw_fd(), fname.as_ptr(), tdir.as_raw_fd(), tname.as_ptr(), darwin_flags) })
    }

    pub fn chmod(&self, p: &Path, mode: u32) -> Result<(), i32> {
        let (dir, name) = self.parent(p)?;
        check(unsafe {
            libc::fchmodat(dir.as_raw_fd(), name.as_ptr(), mode as libc::mode_t, libc::AT_SYMLINK_NOFOLLOW)
        })
    }

    pub fn utimens(&self, p: &Path, times: &[libc::timespec; 2]) -> Result<(), i32> {
        let (dir, name) = self.parent(p)?;
        check(unsafe { libc::utimensat(dir.as_raw_fd(), name.as_ptr(), times.as_ptr(), libc::AT_SYMLINK_NOFOLLOW) })
    }

    pub fn truncate(&self, p: &Path, size: u64) -> Result<(), i32> {
        self.open_file(p, libc::O_WRONLY, 0)?.set_len(size).map_err(|e| super::fs::linux_errno(&e))
    }

    pub fn statvfs(&self) -> Result<libc::statvfs, i32> {
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        check(unsafe { libc::fstatvfs(self.fd.as_raw_fd(), &mut s) })?;
        Ok(s)
    }

    /// An fd on the object itself for xattr calls (O_SYMLINK: a symlink is
    /// opened as itself, never followed).
    fn xattr_fd(&self, p: &Path) -> Result<OwnedFd, i32> {
        let (dir, name) = self.parent(p)?;
        openat(dir.as_raw_fd(), &name, libc::O_RDONLY | libc::O_SYMLINK | libc::O_NONBLOCK, 0)
    }

    pub fn get_xattr(&self, p: &Path, key: &std::ffi::CStr) -> Option<Vec<u8>> {
        let fd = self.xattr_fd(p).ok()?;
        let mut buf = [0u8; 64];
        let n = unsafe { libc::fgetxattr(fd.as_raw_fd(), key.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, 0) };
        (n > 0).then(|| buf[..n as usize].to_vec())
    }

    pub fn set_xattr(&self, p: &Path, key: &std::ffi::CStr, value: Option<&[u8]>) -> Result<(), i32> {
        let fd = self.xattr_fd(p)?;
        match value {
            Some(v) => {
                check(unsafe { libc::fsetxattr(fd.as_raw_fd(), key.as_ptr(), v.as_ptr().cast(), v.len(), 0, 0) })
            }
            None => {
                unsafe { libc::fremovexattr(fd.as_raw_fd(), key.as_ptr(), 0) };
                Ok(())
            }
        }
    }
}

/// Entries of an open directory: (ino, dirent type, name), without "." / "..".
pub fn list_dir(dir: &OwnedFd) -> Result<Vec<(u64, u32, Vec<u8>)>, i32> {
    // fdopendir takes ownership of the fd it is given, so hand it a duplicate.
    let dup = unsafe { libc::dup(dir.as_raw_fd()) };
    if dup < 0 {
        return Err(err());
    }
    let d = unsafe { libc::fdopendir(dup) };
    if d.is_null() {
        unsafe { libc::close(dup) };
        return Err(err());
    }
    unsafe { libc::rewinddir(d) };
    let mut out = Vec::new();
    loop {
        let e = unsafe { libc::readdir(d) };
        if e.is_null() {
            break;
        }
        let e = unsafe { &*e };
        let name = unsafe { std::slice::from_raw_parts(e.d_name.as_ptr() as *const u8, e.d_namlen as usize) };
        if name == b"." || name == b".." {
            continue;
        }
        out.push((e.d_ino, e.d_type as u32, name.to_vec()));
    }
    unsafe { libc::closedir(d) };
    Ok(out)
}
