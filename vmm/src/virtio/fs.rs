//! virtio-fs (virtio 1.x section 5.11): a FUSE passthrough server exposing a
//! host directory to the guest (`mount -t virtiofs <tag> <dir>`).
//!
//! Queues: 0 = hiprio (FORGET/INTERRUPT), 1..=N = request queues. Each
//! request queue has its own worker thread (Linux spreads requests across
//! queues per CPU), and the server only takes its locks around node/handle
//! table updates, so file I/O on different queues runs in parallel.
//!
//! Caching: entries/attributes are cacheable for `ORBIT_FS_TTL` seconds
//! (default 5); file data stays in the guest page cache across opens
//! (KEEP_CACHE) and is invalidated when the host's mtime/size change
//! (AUTO_INVAL_DATA). WRITEBACK_CACHE is deliberately off: with it the guest
//! kernel treats its own size/mtime as authoritative, so edits made on the Mac
//! never showed up in containers (and container writes reached the Mac late).
//! Directory listings return attributes too (READDIRPLUS).
//!
//! Nodes are tracked by host path; Linux and macOS share st_mode bits but
//! not open flags or errno values, which are translated here.

use std::collections::HashMap;
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::hostfs::{self, Access, Attr, Root};
use super::{Queue, VirtioDevice, VirtioInterrupt, VIRTIO_F_VERSION_1};
use crate::memory::GuestMemory;

pub const DEVICE_ID_FS: u32 = 26;

#[derive(Clone, Debug)]
pub struct Share {
    pub path: PathBuf,
    pub tag: String,
    /// Unix socket of the guest's fsnotify agent (vsock port 1026), if any:
    /// Mac-side file changes are replayed there as inotify events.
    pub notify: Option<PathBuf>,
    /// Only these directories (and the way down to them) are visible; empty = all.
    pub allow: Vec<PathBuf>,
}

// ---- FUSE protocol ---------------------------------------------------------

const FUSE_LOOKUP: u32 = 1;
const FUSE_FORGET: u32 = 2;
const FUSE_GETATTR: u32 = 3;
const FUSE_SETATTR: u32 = 4;
const FUSE_READLINK: u32 = 5;
const FUSE_SYMLINK: u32 = 6;
const FUSE_MKNOD: u32 = 8;
const FUSE_MKDIR: u32 = 9;
const FUSE_UNLINK: u32 = 10;
const FUSE_RMDIR: u32 = 11;
const FUSE_RENAME: u32 = 12;
const FUSE_LINK: u32 = 13;
const FUSE_OPEN: u32 = 14;
const FUSE_READ: u32 = 15;
const FUSE_WRITE: u32 = 16;
const FUSE_STATFS: u32 = 17;
const FUSE_RELEASE: u32 = 18;
const FUSE_FSYNC: u32 = 20;
const FUSE_FLUSH: u32 = 25;
const FUSE_INIT: u32 = 26;
const FUSE_OPENDIR: u32 = 27;
const FUSE_READDIR: u32 = 28;
const FUSE_RELEASEDIR: u32 = 29;
const FUSE_FSYNCDIR: u32 = 30;
const FUSE_ACCESS: u32 = 34;
const FUSE_CREATE: u32 = 35;
const FUSE_INTERRUPT: u32 = 36;
const FUSE_DESTROY: u32 = 38;
const FUSE_BATCH_FORGET: u32 = 42;
const FUSE_READDIRPLUS: u32 = 44;
const FUSE_RENAME2: u32 = 45;
const FUSE_SYNCFS: u32 = 50;

const FATTR_MODE: u32 = 1 << 0;
const FATTR_SIZE: u32 = 1 << 3;
const FATTR_ATIME: u32 = 1 << 4;
const FATTR_MTIME: u32 = 1 << 5;
const FATTR_FH: u32 = 1 << 6;
const FATTR_ATIME_NOW: u32 = 1 << 7;
const FATTR_MTIME_NOW: u32 = 1 << 8;

// FUSE_INIT flags
const FUSE_ASYNC_READ: u32 = 1 << 0;
const FUSE_BIG_WRITES: u32 = 1 << 5;
const FUSE_AUTO_INVAL_DATA: u32 = 1 << 12;
const FUSE_DO_READDIRPLUS: u32 = 1 << 13;
const FUSE_READDIRPLUS_AUTO: u32 = 1 << 14;
const FUSE_WRITEBACK_CACHE: u32 = 1 << 16;
const FUSE_PARALLEL_DIROPS: u32 = 1 << 18;
const FUSE_MAX_PAGES: u32 = 1 << 22;
const FUSE_CACHE_SYMLINKS: u32 = 1 << 23;
// fuse_open_out.open_flags
const FOPEN_KEEP_CACHE: u32 = 1 << 1;

const MAX_PAGES: u16 = 256; // 1 MiB requests
const MAX_WRITE: u32 = MAX_PAGES as u32 * 4096;
pub const REQUEST_QUEUES: usize = 4;

const IN_HDR: usize = 40;
const OUT_HDR: usize = 16;
const ATTR_LEN: usize = 88;
const ENTRY_LEN: usize = 40 + ATTR_LEN;
const ROOT: u64 = 1;
/// Group id of the guest's fsnotify agent (guest/etc/orbit/fsnotify.sh).
const NOTIFY_GID: u32 = 4_000_000_001;

// Linux errno values (the guest's).
const EPERM: i32 = 1;
const EACCES: i32 = 13;
const ENOENT: i32 = 2;
const EIO: i32 = 5;
const EBADF: i32 = 9;
const EEXIST: i32 = 17;
const EINVAL: i32 = 22;
const ENOSYS: i32 = 38;
const EMFILE: i32 = 24;

/// Attribute cache lifetime in ms (ORBIT_FS_ATTR_TTL_MS, default 500). File
/// content changes made on the Mac are noticed when attributes are refreshed
/// (mtime/size → AUTO_INVAL_DATA), so this bounds how stale a container's view
/// of an edited file can be. Names (entries) keep the longer `ttl()`.
fn attr_ttl() -> (u64, u32) {
    static T: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    let ms = *T.get_or_init(|| std::env::var("ORBIT_FS_ATTR_TTL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(500));
    (ms / 1000, ((ms % 1000) * 1_000_000) as u32)
}

fn ttl() -> u64 {
    static T: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var("ORBIT_FS_TTL").ok().and_then(|v| v.parse().ok()).unwrap_or(5))
}

pub(super) fn linux_errno(e: &std::io::Error) -> i32 {
    match e.raw_os_error() {
        Some(libc::ENOTEMPTY) => 39,
        Some(libc::ENAMETOOLONG) => 36,
        Some(libc::ELOOP) => 40,
        Some(libc::ENOSYS) => 38,
        Some(libc::ENOTSUP) | Some(libc::EOPNOTSUPP) => 95,
        Some(libc::ENOATTR) => 61,
        Some(libc::EDQUOT) => 122,
        Some(libc::ETIMEDOUT) => 110,
        Some(n) if n < 35 => n, // 1..34 match between Darwin and Linux
        _ => match e.kind() {
            ErrorKind::NotFound => ENOENT,
            ErrorKind::PermissionDenied => 13,
            ErrorKind::AlreadyExists => EEXIST,
            _ => EIO,
        },
    }
}

/// Linux/arm64 open(2) flags → Darwin. arm64 uses the 32-bit ARM values for
/// O_DIRECTORY (0o40000) and O_NOFOLLOW (0o100000); O_DIRECT (0o200000) and
/// O_LARGEFILE (0o400000) have no Darwin equivalent and are dropped.
fn darwin_open_flags(linux: u32) -> i32 {
    let mut f = (linux & 3) as i32; // O_RDONLY/O_WRONLY/O_RDWR are the same
    let map = [
        (0o100, libc::O_CREAT),
        (0o200, libc::O_EXCL),
        (0o1000, libc::O_TRUNC),
        (0o2000, libc::O_APPEND),
        (0o4000, libc::O_NONBLOCK),
        (0o40000, libc::O_DIRECTORY),
        (0o100000, libc::O_NOFOLLOW),
    ];
    for (l, d) in map {
        if linux & l != 0 {
            f |= d;
        }
    }
    f
}

struct Reader<'a> {
    b: &'a [u8],
    off: usize,
}

impl<'a> Reader<'a> {
    fn u32(&mut self) -> u32 {
        let v = self.b.get(self.off..self.off + 4).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()));
        self.off += 4;
        v
    }
    fn u64(&mut self) -> u64 {
        let v = self.b.get(self.off..self.off + 8).map_or(0, |s| u64::from_le_bytes(s.try_into().unwrap()));
        self.off += 8;
        v
    }
    fn skip(&mut self, n: usize) {
        self.off += n;
    }
    /// NUL-terminated name.
    fn name(&mut self) -> &'a OsStr {
        let rest = self.b.get(self.off..).unwrap_or(&[]);
        let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
        self.off += end + 1;
        OsStr::from_bytes(&rest[..end])
    }
}

/// Guest view of a file: the host attributes with the emulated owner and mode.
#[derive(Clone, Copy)]
struct View {
    attr: Attr,
    uid: u32,
    gid: u32,
    mode: u32,
}

fn put_attr(out: &mut Vec<u8>, v: &View) {
    let m = &v.attr;
    for x in [m.ino, m.size, m.blocks, m.atime as u64, m.mtime as u64, m.ctime as u64] {
        out.extend_from_slice(&x.to_le_bytes());
    }
    for x in [m.atime_ns, m.mtime_ns, m.ctime_ns, v.mode, m.nlink, v.uid, v.gid, m.rdev, m.blksize, 0] {
        out.extend_from_slice(&x.to_le_bytes());
    }
}

/// fuse_entry_out; nodeid 0 means "no entry, don't cache" (used for . and ..).
fn put_entry(out: &mut Vec<u8>, nodeid: u64, v: Option<&View>) {
    let (t, (at, at_ns)) = if nodeid == 0 { (0, (0, 0)) } else { (ttl(), attr_ttl()) };
    for v in [nodeid, 0, t, at] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&0u32.to_le_bytes()); // entry_valid_nsec
    out.extend_from_slice(&at_ns.to_le_bytes()); // attr_valid_nsec
    match v {
        Some(v) => put_attr(out, v),
        None => out.resize(out.len() + ATTR_LEN, 0),
    }
}

fn attr_out(v: &View) -> Vec<u8> {
    let mut out = Vec::with_capacity(104);
    let (at, at_ns) = attr_ttl();
    out.extend_from_slice(&at.to_le_bytes());
    out.extend_from_slice(&at_ns.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // dummy
    put_attr(&mut out, v);
    out
}

fn open_out(fh: u64, flags: u32) -> Vec<u8> {
    let mut out = fh.to_le_bytes().to_vec();
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out
}

// ---- ownership emulation ---------------------------------------------------
//
// Every file on the share really belongs to the Mac user running the VMM, and
// that can't change. Containers still expect chown to stick (postgres refuses a
// data directory it doesn't own; images chown app dirs to service users), so
// the guest-visible owner is kept in an xattr on the host file instead, as
// virtiofsd's xattr ownership mode does. Files without one show up as root,
// like on Docker Desktop. ORBIT_FS_OWNERSHIP=host reports the real host owner.

const OWNER_XATTR: &std::ffi::CStr = c"user.orbit.owner";

fn emulate_ownership() -> bool {
    static E: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *E.get_or_init(|| std::env::var("ORBIT_FS_OWNERSHIP").as_deref() != Ok("host"))
}

fn host_uid() -> u32 {
    unsafe { libc::getuid() }
}

/// How a host file looks to the guest. With an ownership record (the guest
/// chowned it, or a non-root process created it) the record and the real mode
/// are used. A plain Mac file shows up as root-owned with its owner permission
/// bits mirrored to group and other: otherwise only root in a container could
/// write the project you bind-mounted (images that run as `node`, `app`, uid
/// 1000 ... would get EACCES). ORBIT_FS_OWNERSHIP=host shows the real host owner/mode.
fn view(root: &Root, path: &Path, attr: Attr) -> View {
    if !emulate_ownership() {
        return View { attr, uid: attr.uid, gid: attr.gid, mode: attr.mode };
    }
    if let Some(v) = root.get_xattr(path, OWNER_XATTR) {
        if let Some((u, g)) = std::str::from_utf8(&v).ok().and_then(|t| t.split_once(':')) {
            if let (Ok(u), Ok(g)) = (u.parse(), g.parse()) {
                return View { attr, uid: u, gid: g, mode: attr.mode };
            }
        }
    }
    if attr.uid == host_uid() {
        let owner_bits = (attr.mode >> 6) & 0o7;
        let mode = attr.mode | (owner_bits << 3) | owner_bits;
        View { attr, uid: 0, gid: 0, mode }
    } else {
        View { attr, uid: attr.uid, gid: attr.gid, mode: attr.mode }
    }
}

/// Records a guest-visible owner for `path`. A record also means "the guest
/// manages this file": its real mode is shown, not the widened Mac one.
fn set_owner(root: &Root, path: &Path, owner: (u32, u32)) -> Result<(), i32> {
    if !emulate_ownership() {
        return Ok(());
    }
    root.set_xattr(path, OWNER_XATTR, Some(format!("{}:{}", owner.0, owner.1).as_bytes()))
}

// ---- passthrough filesystem -------------------------------------------------

enum Handle {
    File(File),
    Dir { path: PathBuf, fd: std::os::fd::OwnedFd, entries: Vec<(u64, u32, Vec<u8>)> }, // (ino, dirent type, name)
}

struct Node {
    path: PathBuf,
    lookups: u64,
}

#[derive(Default)]
struct Tables {
    nodes: HashMap<u64, Node>,
    by_path: HashMap<PathBuf, u64>,
    next_node: u64,
}

struct Server {
    root: Root,
    tables: Mutex<Tables>,
    handles: Mutex<HashMap<u64, Arc<Handle>>>,
    next_handle: AtomicU64,
    writeback: AtomicBool,
    /// Host path of each open file handle (for write bookkeeping).
    handle_paths: Mutex<HashMap<u64, PathBuf>>,
    /// Paths the guest changed recently: their FSEvents are our own echo.
    recent: Mutex<HashMap<PathBuf, std::time::Instant>>,
}

type Reply = Result<Vec<u8>, i32>;

impl Server {
    fn new(root: Root) -> Self {
        let mut t = Tables { next_node: 2, ..Default::default() };
        t.nodes.insert(ROOT, Node { path: root.path().to_path_buf(), lookups: 1 << 40 });
        t.by_path.insert(root.path().to_path_buf(), ROOT);
        Self {
            root,
            tables: Mutex::new(t),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            writeback: AtomicBool::new(false),
            handle_paths: Mutex::new(HashMap::new()),
            recent: Mutex::new(HashMap::new()),
        }
    }

    fn path(&self, node: u64) -> Result<PathBuf, i32> {
        self.tables.lock().unwrap().nodes.get(&node).map(|n| n.path.clone()).ok_or(ENOENT)
    }

    fn child(&self, parent: u64, name: &OsStr) -> Result<PathBuf, i32> {
        if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
            return Err(EINVAL);
        }
        Ok(self.path(parent)?.join(name))
    }

    /// Paths outside the allowlist don't exist as far as the guest can tell.
    fn visible(&self, p: &Path) -> Result<(), i32> {
        match self.root.access(p) {
            Access::Denied => Err(ENOENT),
            _ => Ok(()),
        }
    }

    /// Changing something needs a path inside an allowed directory.
    fn writable(&self, p: &Path) -> Result<(), i32> {
        match self.root.access(p) {
            Access::Full => Ok(()),
            Access::Traverse => Err(EACCES),
            Access::Denied => Err(ENOENT),
        }
    }

    fn view(&self, p: &Path) -> Result<View, i32> {
        Ok(view(&self.root, p, self.root.lstat(p)?))
    }

    /// Counts one guest lookup of `path` and returns its node id.
    fn remember(&self, path: PathBuf) -> u64 {
        let mut t = self.tables.lock().unwrap();
        let id = match t.by_path.get(&path) {
            Some(&id) => id,
            None => {
                let id = t.next_node;
                t.next_node += 1;
                t.by_path.insert(path.clone(), id);
                t.nodes.insert(id, Node { path, lookups: 0 });
                id
            }
        };
        t.nodes.get_mut(&id).unwrap().lookups += 1;
        id
    }

    /// Registers a lookup of `path` and returns the FUSE entry reply.
    fn entry(&self, path: PathBuf) -> Reply {
        self.visible(&path)?;
        let v = self.view(&path)?;
        let id = self.remember(path);
        let mut out = Vec::with_capacity(ENTRY_LEN);
        put_entry(&mut out, id, Some(&v));
        Ok(out)
    }

    fn forget(&self, node: u64, n: u64) {
        if node == ROOT {
            return;
        }
        let mut t = self.tables.lock().unwrap();
        if let Some(entry) = t.nodes.get_mut(&node) {
            entry.lookups = entry.lookups.saturating_sub(n);
            if entry.lookups == 0 {
                let path = t.nodes.remove(&node).unwrap().path;
                if t.by_path.get(&path) == Some(&node) {
                    t.by_path.remove(&path);
                }
            }
        }
    }

    /// Refuses new opens once the guest holds too many: each handle is a host
    /// fd, and running out of fds would also break docker.sock and the network.
    fn handle_room(&self) -> Result<(), i32> {
        static LIMIT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        let limit = *LIMIT.get_or_init(|| {
            let mut rl = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            let cur = if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) } == 0 { rl.rlim_cur } else { 256 };
            // Leave room for the VMM's own sockets, disk and listeners.
            (cur.min(65536) as usize).saturating_sub(512).max(64)
        });
        if self.handles.lock().unwrap().len() >= limit {
            return Err(EMFILE);
        }
        Ok(())
    }

    fn add_handle(&self, h: Handle) -> u64 {
        let id = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap().insert(id, Arc::new(h));
        id
    }

    fn get_handle(&self, fh: u64) -> Result<Arc<Handle>, i32> {
        self.handles.lock().unwrap().get(&fh).cloned().ok_or(EBADF)
    }

    fn with_file<T>(&self, fh: u64, f: impl FnOnce(&File) -> Result<T, i32>) -> Result<T, i32> {
        match &*self.get_handle(fh)? {
            Handle::File(file) => f(file),
            _ => Err(EBADF),
        }
    }

    /// Gives a file the creating process's owner (and so its exact mode).
    fn created(&self, path: &Path, caller: (u32, u32)) -> Result<(), i32> {
        self.touched(path);
        set_owner(&self.root, path, caller)
    }

    /// Notes that the guest itself changed `path`.
    fn touched(&self, path: &Path) {
        let mut r = self.recent.lock().unwrap();
        let now = std::time::Instant::now();
        if r.len() > 4096 {
            r.retain(|_, t| now.duration_since(*t).as_secs() < 2);
        }
        r.insert(path.to_path_buf(), now);
    }

    fn recently_touched(&self, path: &Path) -> bool {
        self.recent.lock().unwrap().get(path).is_some_and(|t| t.elapsed().as_secs() < 2)
    }

    /// Whether the guest knows this file or its directory (it can't be
    /// watching anything it never looked up).
    fn watched(&self, path: &Path) -> bool {
        let t = self.tables.lock().unwrap();
        t.by_path.contains_key(path) || path.parent().is_some_and(|d| t.by_path.contains_key(d))
    }

    fn handle(&self, opcode: u32, node: u64, caller: (u32, u32), body: &[u8], write_data: &[u8]) -> Option<Reply> {
        let mut r = Reader { b: body, off: 0 };
        let reply = match opcode {
            FUSE_INIT => {
                let major = r.u32();
                let _minor = r.u32();
                let _readahead = r.u32();
                let offered = r.u32();
                if major < 7 {
                    return Some(Err(EINVAL));
                }
                let wanted = FUSE_ASYNC_READ
                    | FUSE_BIG_WRITES
                    | FUSE_MAX_PAGES
                    | FUSE_AUTO_INVAL_DATA
                    | FUSE_DO_READDIRPLUS
                    | FUSE_READDIRPLUS_AUTO
                    | FUSE_PARALLEL_DIROPS
                    | FUSE_CACHE_SYMLINKS;
                let flags = offered & wanted;
                self.writeback.store(flags & FUSE_WRITEBACK_CACHE != 0, Ordering::Relaxed);
                let mut out = Vec::with_capacity(64);
                for v in [7u32, 31, 1 << 20, flags] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                out.extend_from_slice(&64u16.to_le_bytes()); // max_background
                out.extend_from_slice(&48u16.to_le_bytes()); // congestion_threshold
                out.extend_from_slice(&MAX_WRITE.to_le_bytes());
                out.extend_from_slice(&1u32.to_le_bytes()); // time_gran (ns)
                out.extend_from_slice(&MAX_PAGES.to_le_bytes());
                out.extend_from_slice(&0u16.to_le_bytes()); // map_alignment
                out.resize(64, 0);
                Ok(out)
            }
            FUSE_DESTROY => Ok(Vec::new()),
            FUSE_LOOKUP => {
                let name = r.name();
                self.child(node, name).and_then(|p| self.entry(p))
            }
            FUSE_FORGET => {
                self.forget(node, r.u64());
                return None;
            }
            FUSE_BATCH_FORGET => {
                let count = r.u32() as usize;
                r.skip(4);
                // Never trust the count beyond what the request actually carries.
                for _ in 0..count.min(body.len().saturating_sub(8) / 16) {
                    let id = r.u64();
                    let n = r.u64();
                    self.forget(id, n);
                }
                return None;
            }
            FUSE_INTERRUPT => return None,
            FUSE_GETATTR => {
                let flags = r.u32();
                r.skip(4);
                let fh = r.u64();
                self.path(node).and_then(|p| {
                    let attr = if flags & 1 != 0 { self.with_file(fh, hostfs::fstat)? } else { self.root.lstat(&p)? };
                    Ok(attr_out(&view(&self.root, &p, attr)))
                })
            }
            FUSE_SETATTR => self.setattr(node, caller, &mut r),
            FUSE_READLINK => self.path(node).and_then(|p| self.root.readlink(&p)),
            FUSE_SYMLINK => {
                let name = r.name().to_owned();
                let target = r.name().to_owned();
                self.child(node, &name).and_then(|p| {
                    self.writable(&p)?;
                    self.root.symlink(&target, &p)?;
                    self.created(&p, caller)?;
                    self.entry(p)
                })
            }
            FUSE_MKNOD => {
                let mode = r.u32();
                r.skip(12);
                let name = r.name();
                if mode & libc::S_IFMT as u32 != libc::S_IFREG as u32 {
                    Err(EPERM) // no device nodes / fifos on the host share
                } else {
                    self.child(node, name).and_then(|p| {
                        self.writable(&p)?;
                        self.root.open_file(&p, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, mode & 0o7777)?;
                        self.created(&p, caller)?;
                        self.entry(p)
                    })
                }
            }
            FUSE_MKDIR => {
                let mode = r.u32();
                let umask = r.u32();
                let name = r.name();
                self.child(node, name).and_then(|p| {
                    self.writable(&p)?;
                    self.root.mkdir(&p, mode & !umask & 0o7777)?;
                    self.created(&p, caller)?;
                    self.entry(p)
                })
            }
            FUSE_UNLINK => {
                let name = r.name();
                self.child(node, name)
                    .and_then(|p| {
                        self.writable(&p)?;
                        self.touched(&p);
                        self.root.unlink(&p, false)
                    })
                    .map(|_| Vec::new())
            }
            FUSE_RMDIR => {
                let name = r.name();
                self.child(node, name)
                    .and_then(|p| {
                        self.writable(&p)?;
                        self.root.unlink(&p, true)
                    })
                    .map(|_| Vec::new())
            }
            FUSE_RENAME | FUSE_RENAME2 => {
                let newdir = r.u64();
                let flags = if opcode == FUSE_RENAME2 {
                    let f = r.u32();
                    r.skip(4);
                    f
                } else {
                    0
                };
                let old = r.name().to_owned();
                let new = r.name().to_owned();
                self.rename(node, &old, newdir, &new, flags)
            }
            FUSE_LINK => {
                let old = r.u64();
                let name = r.name();
                self.path(old).and_then(|src| {
                    let dst = self.child(node, name)?;
                    self.writable(&src)?;
                    self.writable(&dst)?;
                    self.root.link(&src, &dst)?;
                    self.touched(&dst);
                    self.entry(dst)
                })
            }
            FUSE_OPEN => {
                let flags = r.u32();
                // A write-mode open drops the guest's cached pages; fsnotify
                // relies on that to make injected events see fresh content.
                let keep = if flags & 3 == 0 { FOPEN_KEEP_CACHE } else { 0 };
                self.path(node).and_then(|p| self.open(&p, flags, None)).map(|fh| open_out(fh, keep))
            }
            FUSE_CREATE => {
                let flags = r.u32();
                let mode = r.u32();
                let umask = r.u32();
                r.skip(4);
                let name = r.name();
                self.child(node, name).and_then(|p| {
                    let fh = self.open(&p, flags | 0o100, Some(mode & !umask & 0o7777))?;
                    self.created(&p, caller)?;
                    let mut out = self.entry(p)?;
                    out.extend_from_slice(&open_out(fh, 0));
                    Ok(out)
                })
            }
            FUSE_READ => {
                let fh = r.u64();
                let offset = r.u64();
                let size = r.u32().min(MAX_WRITE) as usize;
                self.with_file(fh, |f| {
                    let mut buf = vec![0u8; size];
                    let mut got = 0;
                    while got < size {
                        match f.read_at(&mut buf[got..], offset + got as u64) {
                            Ok(0) => break,
                            Ok(n) => got += n,
                            Err(e) if e.kind() == ErrorKind::Interrupted => {}
                            Err(e) => return Err(linux_errno(&e)),
                        }
                    }
                    buf.truncate(got);
                    Ok(buf)
                })
            }
            FUSE_WRITE => {
                let fh = r.u64();
                let offset = r.u64();
                let size = r.u32() as usize;
                let data = &write_data[..size.min(write_data.len())];
                if let Some(p) = self.handle_paths.lock().unwrap().get(&fh) {
                    self.touched(p);
                }
                self.with_file(fh, |f| {
                    f.write_all_at(data, offset).map_err(|e| linux_errno(&e))?;
                    let mut out = (data.len() as u32).to_le_bytes().to_vec();
                    out.extend_from_slice(&[0u8; 4]);
                    Ok(out)
                })
            }
            FUSE_FLUSH => Ok(Vec::new()),
            FUSE_FSYNC | FUSE_FSYNCDIR => {
                let fh = r.u64();
                match self.get_handle(fh).as_deref() {
                    Ok(Handle::File(f)) => f.sync_all().map(|_| Vec::new()).map_err(|e| linux_errno(&e)),
                    _ => Ok(Vec::new()),
                }
            }
            FUSE_SYNCFS => Ok(Vec::new()),
            FUSE_RELEASE | FUSE_RELEASEDIR => {
                let fh = r.u64();
                self.handles.lock().unwrap().remove(&fh);
                self.handle_paths.lock().unwrap().remove(&fh);
                Ok(Vec::new())
            }
            FUSE_OPENDIR => self.path(node).and_then(|p| self.opendir(p)).map(|fh| open_out(fh, 0)),
            FUSE_READDIR | FUSE_READDIRPLUS => {
                let fh = r.u64();
                let offset = r.u64() as usize;
                let size = r.u32() as usize;
                match self.get_handle(fh).as_deref() {
                    Ok(Handle::Dir { path, fd, entries }) if opcode == FUSE_READDIRPLUS => {
                        Ok(self.readdirplus_out(path, fd, entries, offset, size))
                    }
                    Ok(Handle::Dir { entries, .. }) => Ok(readdir_out(entries, offset, size)),
                    _ => Err(EBADF),
                }
            }
            FUSE_STATFS => self.root.statvfs().map(|s| statfs_out(&s)),
            FUSE_ACCESS => Ok(Vec::new()),
            _ => Err(ENOSYS), // xattrs, locks, lseek, fallocate, ...: the kernel copes
        };
        Some(reply)
    }

    fn open(&self, path: &Path, linux_flags: u32, create_mode: Option<u32>) -> Result<u64, i32> {
        let mut flags = darwin_open_flags(linux_flags) | libc::O_NOFOLLOW;
        if self.writeback.load(Ordering::Relaxed) {
            // With writeback caching the guest kernel may read pages of a file
            // it opened write-only, and it implements O_APPEND itself.
            if flags & libc::O_ACCMODE == libc::O_WRONLY {
                flags = (flags & !libc::O_ACCMODE) | libc::O_RDWR;
            }
            flags &= !libc::O_APPEND;
        }
        let write = flags & libc::O_ACCMODE != libc::O_RDONLY || create_mode.is_some() || flags & libc::O_TRUNC != 0;
        if write {
            self.writable(path)?
        } else {
            self.visible(path)?
        }
        let mode = create_mode.unwrap_or(0);
        let open = |flags: i32| self.root.open_file(path, flags & !libc::O_NOFOLLOW, mode);
        self.handle_room()?;
        let f = match open(flags) {
            // A write-only file (e.g. mode 0200) can't be upgraded to read-write.
            Err(e) if e == libc::EACCES && darwin_open_flags(linux_flags) & libc::O_ACCMODE == libc::O_WRONLY => {
                open((flags & !libc::O_ACCMODE) | libc::O_WRONLY)
            }
            other => other,
        }?;
        let fh = self.add_handle(Handle::File(f));
        self.handle_paths.lock().unwrap().insert(fh, path.to_path_buf());
        Ok(fh)
    }

    fn opendir(&self, path: PathBuf) -> Result<u64, i32> {
        self.visible(&path)?;
        self.handle_room()?;
        let fd = self.root.open_dir(&path)?;
        let self_ino = hostfs::fstat(&fd)?.ino;
        let mut entries =
            vec![(self_ino, libc::DT_DIR as u32, b".".to_vec()), (self_ino, libc::DT_DIR as u32, b"..".to_vec())];
        let traverse_only = matches!(self.root.access(&path), Access::Traverse);
        for (ino, kind, name) in hostfs::list_dir(&fd)? {
            // Above an allowed directory, list only the way down to it.
            if traverse_only && matches!(self.root.access(&path.join(OsStr::from_bytes(&name))), Access::Denied) {
                continue;
            }
            entries.push((ino, kind, name));
        }
        Ok(self.add_handle(Handle::Dir { path, fd, entries }))
    }

    /// fuse_direntplus records: an entry reply (which counts as a lookup) + dirent.
    fn readdirplus_out(
        &self,
        dir: &Path,
        dirfd: &std::os::fd::OwnedFd,
        entries: &[(u64, u32, Vec<u8>)],
        offset: usize,
        size: usize,
    ) -> Vec<u8> {
        use std::os::fd::AsRawFd;
        let mut out = Vec::new();
        for (i, (ino, kind, name)) in entries.iter().enumerate().skip(offset) {
            let rec = ENTRY_LEN + (24 + name.len()).next_multiple_of(8);
            if out.len() + rec > size {
                break;
            }
            let dot = name == b"." || name == b"..";
            let path = dir.join(OsStr::from_bytes(name));
            let attr = if dot {
                None
            } else {
                CString::new(name.clone()).ok().and_then(|c| hostfs::fstatat(dirfd.as_raw_fd(), &c).ok())
            };
            match attr {
                Some(a) => {
                    let v = view(&self.root, &path, a);
                    let id = self.remember(path);
                    put_entry(&mut out, id, Some(&v));
                }
                None => put_entry(&mut out, 0, None), // vanished or ./..: no lookup
            }
            put_dirent(&mut out, *ino, i + 1, *kind, name);
        }
        out
    }

    fn rename(&self, olddir: u64, old: &OsStr, newdir: u64, new: &OsStr, flags: u32) -> Reply {
        const RENAME_NOREPLACE: u32 = 1;
        const RENAME_EXCHANGE: u32 = 2;
        let from = self.child(olddir, old)?;
        let to = self.child(newdir, new)?;
        self.writable(&from)?;
        self.writable(&to)?;
        self.touched(&from);
        self.touched(&to);
        let darwin_flags = match flags {
            0 => 0,
            RENAME_NOREPLACE => libc::RENAME_EXCL,
            RENAME_EXCHANGE => libc::RENAME_SWAP,
            _ => return Err(EINVAL),
        };
        // Hold the table lock across the syscall so lookups never see a half-moved tree.
        let mut t = self.tables.lock().unwrap();
        self.root.rename(&from, &to, darwin_flags)?;
        if flags == RENAME_EXCHANGE {
            let tmp = from.with_extension("orbit-swap-tmp");
            repoint(&mut t, &from, &tmp);
            repoint(&mut t, &to, &from);
            repoint(&mut t, &tmp, &to);
        } else {
            // Whatever `to` was is gone; its nodes are stale.
            let stale: Vec<PathBuf> = t.by_path.keys().filter(|p| p.starts_with(&to)).cloned().collect();
            for p in stale {
                t.by_path.remove(&p);
            }
            repoint(&mut t, &from, &to);
        }
        Ok(Vec::new())
    }

    fn setattr(&self, node: u64, caller: (u32, u32), r: &mut Reader) -> Reply {
        // The guest's fsnotify agent truncates files to their own size under
        // NOTIFY_GID only to make the guest kernel emit IN_MODIFY for a change
        // made on the Mac. Never apply that to the Mac file.
        if caller.1 == NOTIFY_GID {
            let path = self.path(node)?;
            return Ok(attr_out(&self.view(&path)?));
        }
        let valid = r.u32();
        r.skip(4);
        let fh = r.u64();
        let size = r.u64();
        r.skip(8); // lock_owner
        let atime = r.u64();
        let mtime = r.u64();
        r.skip(8); // ctime
        let atimensec = r.u32();
        let mtimensec = r.u32();
        r.skip(4);
        let mode = r.u32();
        r.skip(4); // unused4
        let uid = r.u32();
        let gid = r.u32();
        let path = self.path(node)?;
        self.writable(&path)?;
        self.touched(&path);
        if valid & FATTR_MODE != 0 {
            // From now on the guest owns this file's mode: pin its owner record
            // so the exact mode it set is what it sees.
            if emulate_ownership() && self.root.get_xattr(&path, OWNER_XATTR).is_none() {
                let v = self.view(&path)?;
                set_owner(&self.root, &path, (v.uid, v.gid))?;
            }
            self.root.chmod(&path, mode & 0o7777)?;
        }
        if valid & FATTR_SIZE != 0 {
            let via_fh = if valid & FATTR_FH != 0 {
                self.with_file(fh, |f| f.set_len(size).map_err(|e| linux_errno(&e))).ok()
            } else {
                None
            };
            if via_fh.is_none() {
                self.root.truncate(&path, size)?;
            }
        }
        if valid & (FATTR_ATIME | FATTR_MTIME) != 0 {
            let ts = |set: bool, now: bool, s: u64, ns: u32| libc::timespec {
                tv_sec: s as libc::time_t,
                tv_nsec: if !set {
                    libc::UTIME_OMIT
                } else if now {
                    libc::UTIME_NOW
                } else {
                    ns as libc::c_long
                },
            };
            let times = [
                ts(valid & FATTR_ATIME != 0, valid & FATTR_ATIME_NOW != 0, atime, atimensec),
                ts(valid & FATTR_MTIME != 0, valid & FATTR_MTIME_NOW != 0, mtime, mtimensec),
            ];
            self.root.utimens(&path, &times)?;
        }
        const FATTR_UID: u32 = 1 << 1;
        const FATTR_GID: u32 = 1 << 2;
        let mut v = self.view(&path)?;
        if valid & (FATTR_UID | FATTR_GID) != 0 {
            if valid & FATTR_UID != 0 {
                v.uid = uid;
            }
            if valid & FATTR_GID != 0 {
                v.gid = gid;
            }
            set_owner(&self.root, &path, (v.uid, v.gid))?;
            v = self.view(&path)?;
        }
        Ok(attr_out(&v))
    }
}

/// Re-points the node at `from` and everything below it to `to`.
fn repoint(t: &mut Tables, from: &Path, to: &Path) {
    let moved: Vec<(PathBuf, u64)> =
        t.by_path.iter().filter(|(p, _)| p.starts_with(from)).map(|(p, id)| (p.clone(), *id)).collect();
    for (old, id) in moved {
        // join("") would append a trailing slash and turn the file into a "directory".
        let rel = old.strip_prefix(from).unwrap();
        let new = if rel.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rel) };
        t.by_path.remove(&old);
        t.by_path.insert(new.clone(), id);
        if let Some(n) = t.nodes.get_mut(&id) {
            n.path = new;
        }
    }
}

fn put_dirent(out: &mut Vec<u8>, ino: u64, next: usize, kind: u32, name: &[u8]) {
    let rec = (24 + name.len()).next_multiple_of(8);
    out.extend_from_slice(&ino.to_le_bytes());
    out.extend_from_slice(&(next as u64).to_le_bytes()); // offset of the next entry
    out.extend_from_slice(&(name.len() as u32).to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(name);
    out.resize(out.len() + rec - 24 - name.len(), 0);
}

fn readdir_out(entries: &[(u64, u32, Vec<u8>)], offset: usize, size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, (ino, kind, name)) in entries.iter().enumerate().skip(offset) {
        if out.len() + (24 + name.len()).next_multiple_of(8) > size {
            break;
        }
        put_dirent(&mut out, *ino, i + 1, *kind, name);
    }
    out
}

fn statfs_out(s: &libc::statvfs) -> Vec<u8> {
    let mut out = Vec::with_capacity(80);
    for v in [s.f_blocks as u64, s.f_bfree as u64, s.f_bavail as u64, s.f_files as u64, s.f_ffree as u64] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for v in [s.f_bsize as u32, 255, s.f_frsize as u32, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.resize(80, 0);
    out
}

// ---- Mac-side change → guest inotify ------------------------------------------

/// Batches FSEvents for files the guest knows about and sends their paths to
/// the guest's fsnotify agent, which replays them as inotify events.
fn forward_events(
    server: Arc<Server>,
    root: PathBuf,
    tag: String,
    sock: PathBuf,
    rx: std::sync::mpsc::Receiver<(PathBuf, u32)>,
) {
    use crate::fsevents::*;
    use std::io::Write;
    use std::time::{Duration, Instant};
    loop {
        let Ok(first) = rx.recv() else { return };
        let mut batch = vec![first];
        let until = Instant::now() + Duration::from_millis(200);
        while let Ok(ev) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            batch.push(ev);
        }
        let mut rels = std::collections::BTreeSet::new();
        for (path, flags) in batch {
            let changed = flags & (ITEM_MODIFIED | ITEM_CREATED | ITEM_RENAMED | ITEM_INODE_META_MOD) != 0;
            if flags & ITEM_IS_FILE == 0 || !changed || !path.is_file() {
                continue; // directories, xattr-only updates (our ownership records), deletions
            }
            if server.recently_touched(&path)
                || !server.watched(&path)
                || !matches!(server.root.access(&path), Access::Full)
            {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(&root) {
                if let Some(rel) = rel.to_str().filter(|r| !r.contains('\n') && !r.contains('\t')) {
                    rels.insert(rel.to_string());
                }
            }
        }
        if rels.is_empty() {
            continue;
        }
        // Let attributes the guest cached before the change expire, so the
        // watcher that wakes up reads the new size/content.
        let (s, ns) = attr_ttl();
        std::thread::sleep(Duration::new(s, ns) + Duration::from_millis(100));
        if let Ok(mut conn) = std::os::unix::net::UnixStream::connect(&sock) {
            let mut msg = String::from("orbit-fsnotify 2\n");
            msg.extend(rels.iter().map(|r| format!("{tag}\t{r}\n")));
            let _ = conn.write_all(msg.as_bytes());
            let _ = conn.shutdown(std::net::Shutdown::Write);
        }
    }
}

// ---- virtio device + workers ------------------------------------------------

/// What one worker thread serves: queue 0 (hiprio) is folded into worker 0.
#[derive(Default)]
struct Slot {
    queues: Vec<(u16, Queue)>,
    kicked: bool,
    mem: Option<Arc<GuestMemory>>,
    interrupt: Option<Arc<dyn VirtioInterrupt>>,
}

type Worker = Arc<(Mutex<Slot>, Condvar)>;

pub struct Fs {
    tag: String,
    workers: Vec<Worker>,
}

impl Fs {
    pub fn new(share: Share) -> Result<Self, String> {
        if !share.path.is_dir() {
            return Err(format!("{} is not a directory", share.path.display()));
        }
        if share.tag.len() > 36 {
            return Err("virtiofs tag longer than 36 bytes".into());
        }
        let root = share.path.canonicalize().map_err(|e| e.to_string())?;
        let server = Arc::new(Server::new(Root::open(&root, &share.allow)?));
        if let Some(sock) = share.notify.clone() {
            let (tx, rx) = std::sync::mpsc::channel();
            crate::fsevents::watch(&root, tx)?;
            let (srv, tag) = (server.clone(), share.tag.clone());
            std::thread::Builder::new()
                .name(format!("fsevents-{}", share.tag))
                .spawn(move || forward_events(srv, root, tag, sock, rx))
                .map_err(|e| e.to_string())?;
        }
        let mut workers = Vec::new();
        for i in 0..REQUEST_QUEUES {
            let w: Worker = Arc::default();
            let (w2, s2) = (w.clone(), server.clone());
            std::thread::Builder::new()
                .name(format!("virtiofs-{}-{i}", share.tag))
                .spawn(move || worker(w2, s2))
                .map_err(|e| e.to_string())?;
            workers.push(w);
        }
        Ok(Self { tag: share.tag, workers })
    }

    /// Queue index → worker: hiprio (0) and request queue 1 go to worker 0.
    fn worker_for(&self, queue: u16) -> usize {
        (queue as usize).saturating_sub(1) % self.workers.len()
    }
}

fn worker(w: Worker, server: Arc<Server>) {
    let (lock, cond) = &*w;
    loop {
        let (mem, interrupt, mut queues) = {
            let mut s = lock.lock().unwrap();
            while !s.kicked {
                s = cond.wait(s).unwrap();
            }
            s.kicked = false;
            let (Some(mem), Some(irq)) = (s.mem.clone(), s.interrupt.clone()) else {
                continue;
            };
            (mem, irq, std::mem::take(&mut s.queues))
        };
        let mut used = false;
        for (_, q) in queues.iter_mut() {
            while let Some(chain) = q.pop(&mem) {
                let written = serve(&server, &mem, &chain.descs);
                q.add_used(&mem, chain.head, written);
                used = true;
            }
        }
        {
            let mut s = lock.lock().unwrap();
            if s.mem.is_some() && s.queues.is_empty() {
                s.queues = queues;
            }
        }
        if used {
            interrupt.signal_used_queue();
        }
    }
}

/// Runs one FUSE request; returns bytes written to the guest's reply buffers.
fn serve(server: &Server, mem: &GuestMemory, descs: &[super::Descriptor]) -> u32 {
    // Largest legitimate request: a MAX_WRITE write, or a rename/symlink with
    // two PATH_MAX names, plus headers.
    const MAX_REQUEST: usize = MAX_WRITE as usize + 64 * 1024;
    let req = match super::queue::read_readable(mem, descs, MAX_REQUEST) {
        Ok(r) => r,
        Err(_) => {
            // Answer EINVAL using just the header, without reading the rest.
            let mut hdr = vec![0u8; IN_HDR];
            match descs.iter().find(|d| !d.write_only) {
                Some(d) if d.len as usize >= IN_HDR => {
                    mem.read(d.addr, &mut hdr);
                }
                _ => return 0,
            }
            let unique = u64::from_le_bytes(hdr[8..16].try_into().unwrap());
            let mut out = (OUT_HDR as u32).to_le_bytes().to_vec();
            out.extend_from_slice(&(-EINVAL).to_le_bytes());
            out.extend_from_slice(&unique.to_le_bytes());
            return match descs.iter().find(|d| d.write_only && d.len as usize >= OUT_HDR) {
                Some(d) => {
                    mem.write(d.addr, &out);
                    OUT_HDR as u32
                }
                None => 0,
            };
        }
    };
    if req.len() < IN_HDR {
        return 0;
    }
    let mut h = Reader { b: &req, off: 0 };
    let len = h.u32() as usize;
    let opcode = h.u32();
    let unique = h.u64();
    let node = h.u64();
    let caller = (h.u32(), h.u32());
    let body = &req[IN_HDR..len.clamp(IN_HDR, req.len())];
    // WRITE carries its payload after the 40-byte fuse_write_in.
    let (args, data) = if opcode == FUSE_WRITE && body.len() >= 40 { body.split_at(40) } else { (body, &[][..]) };
    let Some(reply) = server.handle(opcode, node, caller, args, data) else {
        return 0;
    };
    if let Err(e) = &reply {
        if std::env::var_os("ORBIT_DEBUG").is_some() && *e != ENOENT && *e != ENOSYS {
            eprintln!("virtiofs: op={opcode} node={node} -> errno {e}");
        }
    }
    let (error, payload) = match reply {
        Ok(p) => (0i32, p),
        Err(e) => (-e, Vec::new()),
    };
    let mut out = Vec::with_capacity(OUT_HDR + payload.len());
    out.extend_from_slice(&((OUT_HDR + payload.len()) as u32).to_le_bytes());
    out.extend_from_slice(&error.to_le_bytes());
    out.extend_from_slice(&unique.to_le_bytes());
    out.extend_from_slice(&payload);
    let mut off = 0;
    for d in descs.iter().filter(|d| d.write_only) {
        if off >= out.len() {
            break;
        }
        let n = (d.len as usize).min(out.len() - off);
        mem.write(d.addr, &out[off..off + n]);
        off += n;
    }
    off as u32
}

impl VirtioDevice for Fs {
    fn device_id(&self) -> u32 {
        DEVICE_ID_FS
    }

    fn features(&self) -> u64 {
        VIRTIO_F_VERSION_1
    }

    fn num_queues(&self) -> usize {
        1 + REQUEST_QUEUES
    }

    fn queue_max_size(&self) -> u16 {
        1024
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        // struct virtio_fs_config { u8 tag[36]; le32 num_request_queues; }
        let mut cfg = [0u8; 40];
        cfg[..self.tag.len()].copy_from_slice(self.tag.as_bytes());
        cfg[36..40].copy_from_slice(&(REQUEST_QUEUES as u32).to_le_bytes());
        for (i, b) in data.iter_mut().enumerate() {
            *b = cfg.get(offset as usize + i).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn activate(
        &mut self,
        mem: Arc<GuestMemory>,
        queues: Vec<Queue>,
        interrupt: Arc<dyn VirtioInterrupt>,
        _acked: u64,
    ) {
        let mut per_worker: Vec<Vec<(u16, Queue)>> = (0..self.workers.len()).map(|_| Vec::new()).collect();
        for (i, q) in queues.into_iter().enumerate() {
            per_worker[self.worker_for(i as u16)].push((i as u16, q));
        }
        for (w, qs) in self.workers.iter().zip(per_worker) {
            let (lock, cond) = &**w;
            let mut s = lock.lock().unwrap();
            s.mem = Some(mem.clone());
            s.interrupt = Some(interrupt.clone());
            s.queues = qs;
            s.kicked = true;
            cond.notify_one();
        }
    }

    fn queue_notify(&mut self, index: u16) {
        let (lock, cond) = &*self.workers[self.worker_for(index)];
        lock.lock().unwrap().kicked = true;
        cond.notify_one();
    }

    fn reset(&mut self) {
        for w in &self.workers {
            *w.0.lock().unwrap() = Slot::default();
        }
    }
}
