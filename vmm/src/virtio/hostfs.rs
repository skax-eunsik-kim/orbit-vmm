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
//! A revealed directory (`Root::reveal`) is visible the same way, but empty:
//! the guest mounts its own files over it.

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

/// The name and ".." components of `p`, in order.
fn parts(p: &Path) -> Vec<std::ffi::OsString> {
    p.components()
        .filter(|c| matches!(c, Component::Normal(_) | Component::ParentDir))
        .map(|c| c.as_os_str().to_owned())
        .collect()
}

/// Resolves `p` one component at a time and returns where each symlink it
/// passes through lives (with that location's parent already resolved).
fn symlinks_on_the_way(p: &Path) -> Result<Vec<PathBuf>, String> {
    let mut todo: std::collections::VecDeque<std::ffi::OsString> = parts(p).into();
    let mut cur = PathBuf::from("/");
    let mut links = Vec::new();
    while let Some(c) = todo.pop_front() {
        if c == ".." {
            cur.pop();
            continue;
        }
        let next = cur.join(&c);
        let meta = std::fs::symlink_metadata(&next).map_err(|e| format!("allowed path {}: {e}", p.display()))?;
        if !meta.file_type().is_symlink() {
            cur = next;
            continue;
        }
        if links.len() >= 40 {
            return Err(format!("allowed path {}: too many symlinks", p.display()));
        }
        let target = std::fs::read_link(&next).map_err(|e| e.to_string())?;
        links.push(next);
        if target.is_absolute() {
            cur = PathBuf::from("/");
        }
        let rest: Vec<_> = parts(&target);
        for c in rest.into_iter().rev() {
            todo.push_front(c);
        }
    }
    Ok(links)
}

fn lower(p: &Path) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(&p.as_os_str().as_bytes().to_ascii_lowercase()))
}

/// Containers can write to shared directories, so sharing these would let
/// them run code on the Mac (shell profiles, LaunchAgents, ~/.orbit/bin) or
/// read credentials (~/.ssh and other dot-directories).
/// unsafe_share for both spellings of the home directory (/Users/... and the
/// Data-volume firmlink /System/Volumes/Data/Users/...).
fn unsafe_share_any(home: &Path, dir: &Path) -> Option<&'static str> {
    let data = Path::new("/System/Volumes/Data").join(home.strip_prefix("/").unwrap_or(home));
    unsafe_share(home, dir).or_else(|| unsafe_share(&data, dir))
}

fn unsafe_share(home: &Path, dir: &Path) -> Option<&'static str> {
    if home.starts_with(dir) {
        return Some("it contains your home directory");
    }
    let first = dir.strip_prefix(home).ok()?.components().next()?;
    let name = first.as_os_str().as_bytes();
    if name.starts_with(b".") || name.eq_ignore_ascii_case(b"Library") {
        return Some("it is (inside) a hidden directory or ~/Library");
    }
    // Commonly on PATH or opened by the system: writable means code execution.
    if name.eq_ignore_ascii_case(b"bin") || name.eq_ignore_ascii_case(b"Applications") {
        return Some("it is (inside) ~/bin or ~/Applications");
    }
    None
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
    fd: std::sync::Arc<OwnedFd>,
    allow: Vec<PathBuf>,
    /// Directory fds from earlier walks, by path. Every one was reached with
    /// O_NOFOLLOW from the root, so using it is as safe as walking again; it
    /// just skips one openat per path component (tens of µs each on macOS).
    /// Entries live DIR_CACHE_TTL, so a directory moved on the Mac is
    /// re-walked soon; the guest's own renames and rmdirs drop them at once.
    dirs: std::sync::Mutex<std::collections::HashMap<PathBuf, (std::sync::Arc<OwnedFd>, std::time::Instant)>>,
    /// Directories that are only visible as empty, read-only mount points.
    reveal: Vec<PathBuf>,
}

const DIR_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);
const DIR_CACHE_MAX: usize = 1024;

impl Root {
    pub fn open(path: &Path, allow: &[PathBuf]) -> Result<Self, String> {
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(format!("opening share {}: {}", path.display(), std::io::Error::last_os_error()));
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .and_then(|h| h.canonicalize().ok())
            .ok_or("HOME is not set or does not exist")?;
        // With no allowlist the whole share is writable, so the share itself
        // must pass the same test.
        if allow.is_empty() {
            let c = path.canonicalize().map_err(|e| format!("share {}: {e}", path.display()))?;
            if let Some(why) = unsafe_share_any(&home, &c) {
                return Err(format!("refusing to share {} without --share-allow: {why}", c.display()));
            }
        }
        let mut allowed = Vec::new();
        for a in allow {
            // Allowed directories must exist and lie inside the share; every check
            // below works on the real path (no symlinks, exact case).
            if !a.is_absolute() {
                return Err(format!("allowed path {} must be absolute", a.display()));
            }
            let c = a.canonicalize().map_err(|e| format!("allowed path {}: {e}", a.display()))?;
            if !c.starts_with(path) {
                return Err(format!("allowed path {} is outside the share {}", c.display(), path.display()));
            }
            if let Some(why) = unsafe_share_any(&home, &c) {
                return Err(format!("refusing to share {}: {why}", c.display()));
            }
            allowed.push(c);
        }
        // No allowed directory inside another: the outer one would make the
        // inner one's parents writable, and the guest could swap them for symlinks.
        for a in &allowed {
            if let Some(b) = allowed.iter().find(|b| *b != a && lower(b).starts_with(lower(a))) {
                return Err(format!(
                    "allowed paths {} and {} are nested; list only the outer one",
                    a.display(),
                    b.display()
                ));
            }
        }
        // How each path resolves must not depend on anything the guest can
        // change: a symlink inside a shared directory could be repointed by a
        // container, and the next start would share wherever it points.
        for a in allow {
            for link in symlinks_on_the_way(a)? {
                // Compare real paths with real paths: the walk keeps the spelling
                // it was given (Unicode normalisation, firmlinks), the allowlist
                // has the on-disk one.
                let parent = link.parent().unwrap_or(Path::new("/"));
                let parent = parent.canonicalize().map_err(|e| format!("allowed path {}: {e}", a.display()))?;
                if allowed.iter().any(|d| lower(&parent).starts_with(lower(d))) {
                    return Err(format!(
                        "allowed path {} resolves through {}, a symlink inside a shared directory",
                        a.display(),
                        link.display()
                    ));
                }
            }
        }
        Ok(Root {
            path: path.to_path_buf(),
            fd: std::sync::Arc::new(unsafe { OwnedFd::from_raw_fd(fd) }),
            allow: allowed,
            dirs: Default::default(),
            reveal: Vec::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Makes `dir` and the way down to it visible to the guest as traversable,
    /// read-only and empty (nothing of what is in it on the Mac), so the guest
    /// can mount something of its own there: the docker socket at the path
    /// clients like testcontainers' Ryuk ask the daemon to bind-mount.
    pub fn reveal(&mut self, dir: &Path) -> Result<(), String> {
        let c = dir.canonicalize().map_err(|e| format!("revealed path {}: {e}", dir.display()))?;
        if !c.starts_with(&self.path) || !c.is_dir() {
            return Err(format!("revealed path {} is not a directory in the share", c.display()));
        }
        if matches!(self.access(&c), Access::Denied) {
            self.reveal.push(c);
        }
        Ok(())
    }

    /// Whether `p` is one of the allowed directories or above one, compared
    /// case-insensitively like APFS. Renaming or removing such a path would
    /// change what gets shared on the next start.
    pub fn is_allowed_root(&self, p: &Path) -> bool {
        let p = lower(p);
        self.allow.iter().chain(&self.reveal).any(|a| lower(a).starts_with(&p))
    }

    pub fn access(&self, p: &Path) -> Access {
        if self.allow.is_empty() || self.allow.iter().any(|a| p.starts_with(a)) {
            Access::Full
        } else if self.allow.iter().chain(&self.reveal).any(|a| a.starts_with(p)) {
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

    /// A (shared) fd for the directory `p`, walking from the root with
    /// O_NOFOLLOW, through cached directories where possible. For *at() calls
    /// only: its file offset is shared, so never read entries through it.
    fn dir_fd(&self, p: &Path) -> Result<std::sync::Arc<OwnedFd>, i32> {
        let comps = self.components(p)?;
        let now = std::time::Instant::now();
        let fresh = |d: &std::collections::HashMap<PathBuf, (std::sync::Arc<OwnedFd>, std::time::Instant)>,
                     k: &Path| {
            d.get(k).filter(|(_, t)| now.duration_since(*t) < DIR_CACHE_TTL).map(|(fd, _)| fd.clone())
        };
        if comps.is_empty() {
            return Ok(self.fd.clone());
        }
        if let Some(fd) = fresh(&self.dirs.lock().unwrap(), p) {
            return Ok(fd);
        }
        let mut cur = self.fd.clone();
        let mut path = self.path.clone();
        for comp in comps {
            path.push(comp);
            if let Some(fd) = fresh(&self.dirs.lock().unwrap(), &path) {
                cur = fd;
                continue;
            }
            let fd = std::sync::Arc::new(openat(
                cur.as_raw_fd(),
                &cname(comp)?,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                0,
            )?);
            let mut d = self.dirs.lock().unwrap();
            if d.len() >= DIR_CACHE_MAX {
                d.clear();
            }
            d.insert(path.clone(), (fd.clone(), now));
            cur = fd;
        }
        Ok(cur)
    }

    /// Forgets cached directory fds at or below `p` (it was renamed or removed).
    pub fn forget_dirs(&self, p: &Path) {
        self.dirs.lock().unwrap().retain(|k, _| !k.starts_with(p));
    }

    /// Opens the directory `p` itself as a new fd of its own (for listing).
    pub fn open_dir(&self, p: &Path) -> Result<OwnedFd, i32> {
        let (dir, name) = self.parent(p)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW;
        if name.as_bytes() == b"." {
            return openat(dir.as_raw_fd(), &name, libc::O_RDONLY | libc::O_DIRECTORY, 0);
        }
        openat(dir.as_raw_fd(), &name, flags, 0)
    }

    /// The parent directory of `p` (opened symlink-safely) and `p`'s final name.
    /// For the share root itself: the root and ".".
    pub fn parent(&self, p: &Path) -> Result<(std::sync::Arc<OwnedFd>, CString), i32> {
        let comps = self.components(p)?;
        match comps.split_last() {
            None => Ok((self.fd.clone(), c".".to_owned())),
            Some((last, _)) => Ok((self.dir_fd(p.parent().ok_or(libc::EACCES)?)?, cname(last)?)),
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
        // Needs owner read access: Mac files we manage always keep it (host_mode).
        openat(dir.as_raw_fd(), &name, libc::O_RDONLY | libc::O_SYMLINK | libc::O_NONBLOCK, 0)
    }

    /// Several xattrs of `p` through one open.
    pub fn get_xattrs<const N: usize>(&self, p: &Path, keys: [&std::ffi::CStr; N]) -> [Option<Vec<u8>>; N] {
        let Ok(fd) = self.xattr_fd(p) else { return std::array::from_fn(|_| None) };
        keys.map(|key| {
            let mut buf = [0u8; 64];
            let n = unsafe { libc::fgetxattr(fd.as_raw_fd(), key.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, 0) };
            (n > 0).then(|| buf[..n as usize].to_vec())
        })
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
/// More entries than this in one directory is a guest trying to exhaust our memory.
const MAX_DIR_ENTRIES: usize = 2 << 20;

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
        if out.len() >= MAX_DIR_ENTRIES {
            unsafe { libc::closedir(d) };
            return Err(libc::ENOMEM);
        }
        out.push((e.d_ino, e.d_type as u32, name.to_vec()));
    }
    unsafe { libc::closedir(d) };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::unsafe_share;
    use std::path::Path;

    #[test]
    fn refuses_home_and_sensitive_dirs() {
        let h = Path::new("/Users/me");
        for bad in [
            "/",
            "/Users",
            "/Users/me",
            "/Users/me/.orbit/bin",
            "/Users/me/.ssh",
            "/Users/me/Library/LaunchAgents",
            "/Users/me/bin",
        ] {
            assert!(unsafe_share(h, Path::new(bad)).is_some(), "{bad}");
        }
        for ok in ["/Users/me/work", "/Users/me/src/a.b", "/Users/other"] {
            assert!(unsafe_share(h, Path::new(ok)).is_none(), "{ok}");
        }
    }
}
