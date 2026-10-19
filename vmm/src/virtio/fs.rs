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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// Shown as an empty, read-only directory for the guest to mount over.
    pub reveal: Option<PathBuf>,
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
/// Requests under it never change anything on the Mac (see `notify_op`).
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
const EBUSY: i32 = 16;
const ENOMEM: i32 = 12;

/// Bounds on what the guest can make us hold: nodes it looked up and never
/// forgot, and directory listings cached in open directory handles.
const MAX_NODES: usize = 4 << 20;
/// Total bytes of node paths (each is stored twice). Paths below a share are
/// unbounded in depth (we walk them with openat), so this needs its own limit.
const MAX_NODE_PATH_BYTES: usize = 256 << 20;
/// Longest path the guest may use below the share, like Linux's PATH_MAX.
const MAX_PATH_BYTES: usize = 4096;
const ENAMETOOLONG: i32 = 36;
const MAX_DIR_CACHE_BYTES: usize = 512 << 20;

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

/// Attributes for something gone from the Mac that the guest still has to
/// remove (see `Server::notify_op`).
fn gone_view(dir: bool) -> View {
    let mode = if dir { DIR | 0o755 } else { REG | 0o644 };
    View { attr: Attr { mode, nlink: 1, ..Default::default() }, uid: 0, gid: 0, mode }
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
/// The mode the guest set, when the Mac file got a safer one (see host_mode).
const MODE_XATTR: &std::ffi::CStr = c"user.orbit.mode";

/// What a guest-requested mode becomes on the Mac file (`kind`: the file's
/// S_IFMT bits). There is no setuid/setgid and no group/other write, or a
/// container could hand other local Mac users a setuid-you binary or
/// world-writable project files. The owner always keeps access, which macOS
/// requires for reading the xattrs that hold the guest's view. The guest
/// still sees the mode it asked for (MODE_XATTR).
fn host_mode(mode: u32, kind: u32) -> u32 {
    (mode & 0o1755) | owner_access(kind)
}

const REG: u32 = libc::S_IFREG as u32;
const DIR: u32 = libc::S_IFDIR as u32;

/// Owner permission bits macOS needs before it lets us touch a file's xattrs.
fn owner_access(mode: u32) -> u32 {
    if mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32 {
        0o700
    } else {
        0o600
    }
}

/// Records the guest-visible mode of `path` when it differs from the Mac one.
fn set_mode(root: &Root, path: &Path, mode: u32) -> Result<(), i32> {
    if !emulate_ownership() {
        return Ok(());
    }
    let mode = mode & 0o7777;
    let v = format!("{mode:o}");
    let kind = root.lstat(path)?.mode;
    root.set_xattr(path, MODE_XATTR, (host_mode(mode, kind) != mode).then_some(v.as_bytes()))
}

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
                let recorded = root
                    .get_xattr(path, MODE_XATTR)
                    .and_then(|m| u32::from_str_radix(std::str::from_utf8(&m).ok()?, 8).ok());
                let mode = match recorded {
                    Some(m) => (attr.mode & libc::S_IFMT as u32) | (m & 0o7777),
                    None => attr.mode,
                };
                return View { attr, uid: u, gid: g, mode };
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
    Dir {
        path: PathBuf,
        fd: std::os::fd::OwnedFd,
        entries: Vec<(u64, u32, Vec<u8>)>, // (ino, dirent type, name)
        _charge: Charge,
    },
}

/// Bytes of cached directory listing, given back when the handle goes away.
struct Charge(Arc<AtomicUsize>, usize);

impl Drop for Charge {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Ordering::Relaxed);
    }
}

struct Node {
    path: PathBuf,
    lookups: u64,
    /// What the guest was last told it is (for replaying its removal).
    dir: bool,
}

#[derive(Default)]
struct Tables {
    nodes: HashMap<u64, Node>,
    by_path: HashMap<PathBuf, u64>,
    next_node: u64,
    /// Sum of `nodes[*].path` lengths.
    path_bytes: usize,
}

struct Server {
    root: Root,
    tables: Mutex<Tables>,
    handles: Mutex<HashMap<u64, Arc<Handle>>>,
    next_handle: AtomicU64,
    dir_cache: Arc<AtomicUsize>,
    writeback: AtomicBool,
    /// Host path of each open file handle (for write bookkeeping).
    handle_paths: Mutex<HashMap<u64, PathBuf>>,
    /// Paths the guest changed recently: their FSEvents are our own echo.
    recent: Mutex<HashMap<PathBuf, std::time::Instant>>,
    /// Mac-side changes the fsnotify agent is replaying right now.
    pending: Mutex<HashMap<PathBuf, Pending>>,
}

/// A Mac-side change being replayed in the guest (see `notify_op`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Pending {
    /// Exists on the Mac (created, or a rename's new name): the agent's
    /// lookup says "no such file" so its create/mkdir/rename really runs.
    Appeared,
    /// Gone from the Mac (deleted, or a rename's old name): the agent's lookup
    /// still finds it so its unlink/rmdir/rename really runs.
    Gone { dir: bool },
}

type Reply = Result<Vec<u8>, i32>;

impl Server {
    fn new(root: Root) -> Self {
        let mut t = Tables { next_node: 2, ..Default::default() };
        t.nodes.insert(ROOT, Node { path: root.path().to_path_buf(), lookups: 1 << 40, dir: true });
        t.by_path.insert(root.path().to_path_buf(), ROOT);
        Self {
            root,
            tables: Mutex::new(t),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            dir_cache: Arc::new(AtomicUsize::new(0)),
            writeback: AtomicBool::new(false),
            handle_paths: Mutex::new(HashMap::new()),
            recent: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
        }
    }

    fn path(&self, node: u64) -> Result<PathBuf, i32> {
        self.tables.lock().unwrap().nodes.get(&node).map(|n| n.path.clone()).ok_or(ENOENT)
    }

    fn child(&self, parent: u64, name: &OsStr) -> Result<PathBuf, i32> {
        if name.is_empty() || name == "." || name == ".." || name.as_bytes().contains(&b'/') {
            return Err(EINVAL);
        }
        let p = self.path(parent)?.join(name);
        if p.as_os_str().len() > self.root.path().as_os_str().len() + MAX_PATH_BYTES {
            return Err(ENAMETOOLONG);
        }
        Ok(p)
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

    /// Creating, removing or renaming the entry `p` also needs `p` not to be an
    /// allowed directory itself: those are like mount points. Otherwise the
    /// guest could swap `~/work` for a symlink to somewhere else and get that
    /// shared on the next start.
    fn replaceable(&self, p: &Path) -> Result<(), i32> {
        self.writable(p)?;
        if self.root.is_allowed_root(p) {
            return Err(EBUSY);
        }
        Ok(())
    }

    fn view(&self, p: &Path) -> Result<View, i32> {
        Ok(view(&self.root, p, self.root.lstat(p)?))
    }

    /// Counts one guest lookup of `path` and returns its node id (None when the
    /// table is full: the guest is looking things up without ever forgetting).
    fn remember(&self, path: PathBuf, mode: u32) -> Option<u64> {
        let mut t = self.tables.lock().unwrap();
        let id = match t.by_path.get(&path) {
            Some(&id) => id,
            None if t.nodes.len() >= MAX_NODES => return None,
            None if t.path_bytes + path.as_os_str().len() > MAX_NODE_PATH_BYTES => return None,
            None => {
                let id = t.next_node;
                t.next_node += 1;
                t.path_bytes += path.as_os_str().len();
                t.by_path.insert(path.clone(), id);
                t.nodes.insert(id, Node { path, lookups: 0, dir: false });
                id
            }
        };
        let n = t.nodes.get_mut(&id).unwrap();
        n.lookups += 1;
        n.dir = mode & libc::S_IFMT as u32 == DIR;
        Some(id)
    }

    /// Registers a lookup of `path` and returns the FUSE entry reply.
    fn entry(&self, path: PathBuf) -> Reply {
        self.visible(&path)?;
        let v = self.view(&path)?;
        let id = self.remember(path.clone(), v.attr.mode).ok_or(ENOMEM)?;
        let mut out = Vec::with_capacity(ENTRY_LEN);
        put_entry(&mut out, id, Some(&v));
        self.uncache_pending(&mut out, &path);
        Ok(out)
    }

    /// Until the fsnotify agent has replayed a Mac-side creation, the guest
    /// must not cache the new name (a watcher's scan would otherwise make the
    /// agent's create find it and fail): zero the timeouts of the entry that
    /// `out` ends with.
    fn uncache_pending(&self, out: &mut [u8], path: &Path) {
        let p = self.pending.lock().unwrap();
        if !p.is_empty() && p.get(path) == Some(&Pending::Appeared) {
            let start = out.len() - ENTRY_LEN;
            out[start + 16..start + 40].fill(0);
        }
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
                t.path_bytes -= path.as_os_str().len();
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
    /// Bookkeeping for something the guest just created (with owner access,
    /// so its xattrs can be written); then gives it its final Mac mode.
    fn created(&self, path: &Path, caller: (u32, u32), mode: Option<u32>) -> Result<(), i32> {
        self.touched(path);
        set_owner(&self.root, path, caller)?;
        if let Some(m) = mode {
            set_mode(&self.root, path, m)?;
            let real = self.root.lstat(path)?.mode;
            if real & 0o7777 != host_mode(m, real) {
                self.root.chmod(path, host_mode(m, real))?;
            }
        }
        Ok(())
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

    fn pending(&self, p: &Path) -> Option<Pending> {
        self.pending.lock().unwrap().get(p).copied()
    }

    fn on_mac(&self, p: &Path) -> Result<Option<Attr>, i32> {
        match self.root.lstat(p) {
            Ok(a) => Ok(Some(a)),
            Err(libc::ENOENT) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Requests from the guest's fsnotify agent (gid NOTIFY_GID). It replays a
    /// change already made on the Mac by doing the same thing in the guest, so
    /// the guest kernel emits the inotify events watchers expect (IN_CREATE,
    /// IN_DELETE, IN_MOVED_FROM/TO). None of it reaches the Mac: each
    /// operation only succeeds, as a no-op, for a path forward_events queued
    /// and only while the Mac still matches; everything else that would
    /// change the Mac is refused. None = handle the request normally.
    fn notify_op(&self, opcode: u32, node: u64, r: &mut Reader) -> Option<Reply> {
        let kind = |a: &Attr| a.mode & libc::S_IFMT as u32;
        let reply = match opcode {
            FUSE_LOOKUP => {
                let p = self.child(node, r.name()).ok()?;
                match (self.pending(&p)?, self.on_mac(&p).ok()?) {
                    (Pending::Appeared, Some(_)) => Err(ENOENT),
                    (Pending::Gone { dir }, None) => {
                        // Not cached: any other lookup sees the Mac again.
                        let v = gone_view(dir);
                        let id = self.remember(p.clone(), v.mode)?;
                        let mut out = Vec::with_capacity(ENTRY_LEN);
                        put_entry(&mut out, id, Some(&v));
                        out[16..40].fill(0);
                        Ok(out)
                    }
                    _ => return None,
                }
            }
            FUSE_GETATTR => {
                // A cached inode for something gone from the Mac: keep it
                // alive for the agent's unlink/rmdir/rename.
                let p = self.path(node).ok()?;
                match (self.pending(&p)?, self.on_mac(&p).ok()?) {
                    (Pending::Gone { dir }, None) => {
                        let mut out = attr_out(&gone_view(dir));
                        out[..12].fill(0);
                        Ok(out)
                    }
                    _ => return None,
                }
            }
            FUSE_CREATE | FUSE_MKDIR => {
                let dir = opcode == FUSE_MKDIR;
                r.skip(if dir { 8 } else { 16 });
                self.child(node, r.name()).and_then(|p| {
                    self.writable(&p)?;
                    let want = if dir { DIR } else { REG };
                    match (self.pending(&p), self.on_mac(&p)?) {
                        (Some(Pending::Appeared), Some(a)) if kind(&a) == want => {}
                        (_, Some(_)) => return Err(EEXIST),
                        _ => return Err(ENOENT),
                    }
                    self.pending.lock().unwrap().remove(&p); // replayed
                    if dir {
                        return self.entry(p);
                    }
                    let fh = self.open(&p, 0, None)?; // read-only on the Mac
                    match self.entry(p) {
                        Ok(mut out) => {
                            out.extend_from_slice(&open_out(fh, 0));
                            Ok(out)
                        }
                        Err(e) => {
                            self.handles.lock().unwrap().remove(&fh);
                            self.handle_paths.lock().unwrap().remove(&fh);
                            Err(e)
                        }
                    }
                })
            }
            FUSE_UNLINK | FUSE_RMDIR => self.child(node, r.name()).and_then(|p| {
                self.writable(&p)?;
                match (self.pending(&p), self.on_mac(&p)?) {
                    // Either call: the guest knows best what kind it was.
                    (Some(Pending::Gone { .. }), None) => {
                        self.pending.lock().unwrap().remove(&p);
                        Ok(Vec::new())
                    }
                    _ => Err(EPERM),
                }
            }),
            FUSE_RENAME | FUSE_RENAME2 => {
                let newdir = r.u64();
                if opcode == FUSE_RENAME2 {
                    if r.u32() & !1 != 0 {
                        return Some(Err(EINVAL)); // only RENAME_NOREPLACE
                    }
                    r.skip(4);
                }
                let old = r.name().to_owned();
                let new = r.name().to_owned();
                (|| {
                    let from = self.child(node, &old)?;
                    let to = self.child(newdir, &new)?;
                    self.writable(&from)?;
                    self.writable(&to)?;
                    let gone =
                        matches!(self.pending(&from), Some(Pending::Gone { .. })) && self.on_mac(&from)?.is_none();
                    let there = self.pending(&to) == Some(Pending::Appeared) && self.on_mac(&to)?.is_some();
                    if !gone || !there {
                        return Err(EPERM);
                    }
                    let mut t = self.tables.lock().unwrap();
                    let moves = plan_moves(&t, &[(&from, &to)]);
                    let stale: Vec<PathBuf> = t.by_path.keys().filter(|p| p.starts_with(&to)).cloned().collect();
                    for p in stale {
                        t.by_path.remove(&p);
                    }
                    apply_moves(&mut t, moves);
                    let mut p = self.pending.lock().unwrap();
                    p.remove(&from);
                    p.remove(&to);
                    Ok(Vec::new())
                })()
            }
            FUSE_OPEN => {
                // Never open the Mac file for writing.
                let flags = r.u32();
                let keep = if flags & 3 == 0 { FOPEN_KEEP_CACHE } else { 0 };
                self.path(node).and_then(|p| self.open(&p, 0, None)).map(|fh| open_out(fh, keep))
            }
            FUSE_MKNOD | FUSE_SYMLINK | FUSE_LINK | FUSE_WRITE => Err(EPERM),
            _ => return None,
        };
        Some(reply)
    }

    fn handle(&self, opcode: u32, node: u64, caller: (u32, u32), body: &[u8], write_data: &[u8]) -> Option<Reply> {
        let mut r = Reader { b: body, off: 0 };
        if caller.1 == NOTIFY_GID {
            if let Some(reply) = self.notify_op(opcode, node, &mut Reader { b: body, off: 0 }) {
                return Some(reply);
            }
        }
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
                    self.replaceable(&p)?;
                    self.root.symlink(&target, &p)?;
                    self.created(&p, caller, None)?;
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
                        self.replaceable(&p)?;
                        self.root.open_file(&p, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, host_mode(mode, REG))?;
                        self.created(&p, caller, Some(mode))?;
                        self.entry(p)
                    })
                }
            }
            FUSE_MKDIR => {
                let mode = r.u32();
                let umask = r.u32();
                let name = r.name();
                self.child(node, name).and_then(|p| {
                    self.replaceable(&p)?;
                    self.root.mkdir(&p, host_mode(mode & !umask, DIR))?;
                    self.created(&p, caller, Some(mode & !umask))?;
                    self.entry(p)
                })
            }
            FUSE_UNLINK => {
                let name = r.name();
                self.child(node, name)
                    .and_then(|p| {
                        self.replaceable(&p)?;
                        self.touched(&p);
                        self.root.unlink(&p, false)
                    })
                    .map(|_| Vec::new())
            }
            FUSE_RMDIR => {
                let name = r.name();
                self.child(node, name)
                    .and_then(|p| {
                        self.replaceable(&p)?;
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
                    self.replaceable(&dst)?;
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
                // OPEN never creates (that is CREATE): drop O_CREAT/O_EXCL.
                self.path(node).and_then(|p| self.open(&p, flags & !0o300, None)).map(|fh| open_out(fh, keep))
            }
            FUSE_CREATE => {
                let flags = r.u32();
                let mode = r.u32();
                let umask = r.u32();
                r.skip(4);
                let name = r.name();
                self.child(node, name).and_then(|p| {
                    self.replaceable(&p)?;
                    // Only a file this call really created gets the guest's owner
                    // and mode; opening an existing one leaves them alone.
                    let host = Some(host_mode(mode & !umask, REG));
                    let (fh, new) = match self.open(&p, flags | 0o300, host) {
                        Ok(fh) => (fh, true),
                        Err(EEXIST) if flags & 0o200 == 0 => (self.open(&p, flags & !0o100, None)?, false),
                        Err(e) => return Err(e),
                    };
                    let done = if new {
                        self.created(&p, caller, Some(mode & !umask)).and_then(|_| self.entry(p))
                    } else {
                        self.entry(p)
                    };
                    match done {
                        Ok(mut out) => {
                            out.extend_from_slice(&open_out(fh, 0));
                            Ok(out)
                        }
                        Err(e) => {
                            // The guest never learns this fh, so it will never release it.
                            self.handles.lock().unwrap().remove(&fh);
                            self.handle_paths.lock().unwrap().remove(&fh);
                            Err(e)
                        }
                    }
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
                // The guest's buffer can't be bigger than what we let it read.
                let size = r.u32().min(MAX_WRITE) as usize;
                match self.get_handle(fh).as_deref() {
                    Ok(Handle::Dir { path, fd, entries, .. }) if opcode == FUSE_READDIRPLUS => {
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
        let write = flags & libc::O_ACCMODE != libc::O_RDONLY
            || create_mode.is_some()
            || flags & (libc::O_TRUNC | libc::O_CREAT) != 0;
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
        let bytes: usize = entries.iter().map(|e| e.2.len() + 40).sum();
        if self.dir_cache.fetch_add(bytes, Ordering::Relaxed) + bytes > MAX_DIR_CACHE_BYTES {
            self.dir_cache.fetch_sub(bytes, Ordering::Relaxed);
            return Err(ENOMEM);
        }
        let charge = Charge(self.dir_cache.clone(), bytes);
        Ok(self.add_handle(Handle::Dir { path, fd, entries, _charge: charge }))
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
                    match self.remember(path.clone(), a.mode) {
                        Some(id) => {
                            put_entry(&mut out, id, Some(&v));
                            self.uncache_pending(&mut out, &path);
                        }
                        None => put_entry(&mut out, 0, None), // node table full: no lookup
                    }
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
        self.replaceable(&from)?;
        self.replaceable(&to)?;
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
        let pairs: &[(&Path, &Path)] =
            if flags == RENAME_EXCHANGE { &[(&from, &to), (&to, &from)] } else { &[(&from, &to)] };
        let moves = plan_moves(&t, pairs);
        // Moving a tree under a longer name lengthens every remembered path below
        // it: keep that within the same limits as new lookups.
        let max_path = self.root.path().as_os_str().len() + MAX_PATH_BYTES;
        let mut grown = t.path_bytes as i128;
        for (old, new, _) in &moves {
            if new.as_os_str().len() > max_path {
                return Err(ENAMETOOLONG);
            }
            grown += new.as_os_str().len() as i128 - old.as_os_str().len() as i128;
        }
        if grown > MAX_NODE_PATH_BYTES as i128 {
            return Err(ENOMEM);
        }
        self.root.rename(&from, &to, darwin_flags)?;
        if flags != RENAME_EXCHANGE {
            // Whatever `to` was is gone; its nodes are stale.
            let stale: Vec<PathBuf> = t.by_path.keys().filter(|p| p.starts_with(&to)).cloned().collect();
            for p in stale {
                t.by_path.remove(&p);
            }
        }
        apply_moves(&mut t, moves);
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
            let root_dir = self.root.is_allowed_root(&path);
            // macOS refuses xattr access to a file its owner can't read/write
            // (e.g. after a guest chmod 000), so open it up for the bookkeeping
            // and set the final mode last.
            let real = self.root.lstat(&path)?.mode;
            let unlock = owner_access(real);
            if !root_dir && emulate_ownership() && real & unlock != unlock {
                self.root.chmod(&path, host_mode(real, real))?;
            }
            // From now on the guest owns this file's mode: pin its owner record
            // so the exact mode it set is what it sees.
            if emulate_ownership() && self.root.get_xattr(&path, OWNER_XATTR).is_none() {
                let v = self.view(&path)?;
                set_owner(&self.root, &path, (v.uid, v.gid))?;
            }
            // Allowed directories keep their Mac mode (they are like mount points).
            if root_dir {
                if emulate_ownership() {
                    self.root.set_xattr(&path, MODE_XATTR, Some(format!("{:o}", mode & 0o7777).as_bytes()))?;
                }
            } else {
                set_mode(&self.root, &path, mode)?;
                self.root.chmod(&path, host_mode(mode, real))?;
            }
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

/// For each (from, to): the node at `from` and everything below it, with its
/// path under `to`. All moves are computed first, so an exchange is both pairs.
fn plan_moves(t: &Tables, pairs: &[(&Path, &Path)]) -> Vec<(PathBuf, PathBuf, u64)> {
    let mut moved = Vec::new();
    for &(from, to) in pairs {
        for (old, id) in t.by_path.iter().filter(|(p, _)| p.starts_with(from)) {
            // join("") would append a trailing slash and turn the file into a "directory".
            let rel = old.strip_prefix(from).unwrap();
            let new = if rel.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rel) };
            moved.push((old.clone(), new, *id));
        }
    }
    moved
}

fn apply_moves(t: &mut Tables, moved: Vec<(PathBuf, PathBuf, u64)>) {
    for (old, _, _) in &moved {
        t.by_path.remove(old);
    }
    for (_, new, id) in moved {
        t.by_path.insert(new.clone(), id);
        if let Some(n) = t.nodes.get_mut(&id) {
            t.path_bytes = t.path_bytes - n.path.as_os_str().len() + new.as_os_str().len();
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

/// One replayed change, as a line for the guest's fsnotify agent (protocol 3).
#[derive(Debug, PartialEq)]
enum Replay {
    Modify(PathBuf),
    Create {
        path: PathBuf,
        dir: bool,
    },
    Remove {
        path: PathBuf,
        dir: bool,
    },
    Move {
        from: PathBuf,
        to: PathBuf,
        dir: bool,
    },
    /// Give recursive watchers time to watch the directories just created
    /// before anything is created inside them.
    Settle,
}

/// Most creations replayed per batch: each costs the guest a few processes.
const MAX_CREATES: usize = 2_000;
/// A new directory tree with more entries than this (a new git worktree, an
/// npm install) is announced by its top directory only: recursive watchers
/// scan a new directory when they see it, and nothing is watching most of these.
const MAX_NEW_TREE: usize = 256;

/// Turns a batch of FSEvents into what the guest should see, from the Mac's
/// current state and what the guest has looked up (`known`). FSEvents flags
/// accumulate over a file's recent history, so existence decides: a path that
/// exists and the guest doesn't know was created (if its directory is known
/// or itself new), one the guest knows that is gone was removed, and a rename
/// is an old name that is gone directly followed by a new name that exists.
/// A removed directory often comes without events for what was in it, so
/// whatever the guest knows below it (`known_below`) is removed too.
fn plan_replay(
    batch: &[crate::fsevents::Event],
    known_as: impl Fn(&Path) -> Option<bool>,
    known_below: impl Fn(&Path) -> Vec<(PathBuf, bool)>,
    on_mac: impl Fn(&Path) -> Option<Attr>,
) -> Vec<Replay> {
    use crate::fsevents::*;
    let known = |p: &Path| known_as(p).is_some();
    let is_dir = |a: &Attr| a.mode & libc::S_IFMT as u32 == DIR;
    let is_file = |a: &Attr| a.mode & libc::S_IFMT as u32 == REG;
    let changed = ITEM_MODIFIED | ITEM_CREATED | ITEM_RENAMED | ITEM_INODE_META_MOD;
    let mut moves = Vec::new();
    let mut paired = std::collections::HashSet::new();
    for w in batch.windows(2) {
        let ((from, f1, id1), (to, f2, id2)) = (&w[0], &w[1]);
        if f1 & f2 & ITEM_RENAMED == 0 || *id2 != id1 + 1 || paired.contains(from) || !known(from) {
            continue;
        }
        if on_mac(from).is_some() || from == to {
            continue;
        }
        if let Some(a) = on_mac(to).filter(|a| is_dir(a) || is_file(a)) {
            moves.push(Replay::Move { from: from.clone(), to: to.clone(), dir: is_dir(&a) });
            paired.insert(from.clone());
            paired.insert(to.clone());
        }
    }
    // Flags per path, in first-seen order, for paths in or below something the
    // guest knows (checked cheaply here: the rest of the Mac is busy too).
    let in_batch: std::collections::HashSet<&Path> = batch.iter().map(|(p, _, _)| p.as_path()).collect();
    let mut flags: Vec<(&PathBuf, u32)> = Vec::new();
    let mut at: HashMap<&PathBuf, usize> = HashMap::new();
    for (p, f, _) in batch {
        let near = known(p) || p.parent().is_some_and(|d| known(d) || in_batch.contains(d));
        if paired.contains(p) || !near {
            continue;
        }
        match at.get(p) {
            Some(&i) => flags[i].1 |= f,
            None => {
                at.insert(p, flags.len());
                flags.push((p, *f));
            }
        }
    }
    let mut created = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    for (p, f) in flags {
        let parent_known = p.parent().is_some_and(&known);
        match on_mac(p) {
            Some(a) if !is_dir(&a) && !is_file(&a) => {} // symlinks etc.
            Some(a) if known(p) => {
                if is_file(&a) && f & changed != 0 {
                    modified.push(p.clone());
                }
            }
            Some(a) if f & (ITEM_CREATED | ITEM_RENAMED) != 0 => created.push((p.clone(), is_dir(&a))),
            Some(a) => {
                if is_file(&a) && parent_known && f & changed != 0 {
                    modified.push(p.clone());
                }
            }
            None if known(p) || (parent_known && f & ITEM_CREATED == 0) => {
                let dir = known_as(p).unwrap_or(f & ITEM_IS_DIR != 0);
                if dir {
                    for (c, dir) in known_below(p) {
                        removed.push(Replay::Remove { path: c, dir });
                    }
                }
                removed.push(Replay::Remove { path: p.clone(), dir })
            }
            None => {}
        }
    }
    // A creation only matters below a directory the guest knows or sees
    // being created; shallowest first, settling after each level of new dirs.
    created.sort_by_key(|(p, _)| p.components().count());
    // Each accepted creation with the top of its new tree.
    let mut tree_of: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut tree_size: HashMap<PathBuf, usize> = HashMap::new();
    let mut accepted = Vec::new();
    for (p, dir) in created {
        let parent = p.parent().unwrap_or(&p);
        let top = if known(parent) {
            p.clone()
        } else if let Some(t) = tree_of.get(parent) {
            t.clone()
        } else {
            continue;
        };
        *tree_size.entry(top.clone()).or_default() += 1;
        if dir {
            tree_of.insert(p.clone(), top.clone());
        }
        accepted.push((p, dir, top));
    }
    let mut creates = Vec::new();
    let mut level = 0;
    let mut level_has_dirs = false;
    let mut n = 0;
    for (p, dir, top) in accepted {
        if (p != top && tree_size[&top] > MAX_NEW_TREE) || n == MAX_CREATES {
            continue;
        }
        n += 1;
        let depth = p.components().count();
        if depth != level {
            if level_has_dirs {
                creates.push(Replay::Settle);
            }
            level = depth;
            level_has_dirs = false;
        }
        level_has_dirs |= dir;
        creates.push(Replay::Create { path: p, dir });
    }
    let mut seen = std::collections::HashSet::new();
    removed.retain(|r| match r {
        Replay::Remove { path, .. } => seen.insert(path.clone()),
        _ => true,
    });
    removed.sort_by_key(|r| match r {
        Replay::Remove { path, .. } => std::cmp::Reverse(path.components().count()),
        _ => std::cmp::Reverse(0),
    });
    let mut out = moves;
    out.extend(removed);
    out.extend(creates);
    out.extend(modified.into_iter().map(Replay::Modify));
    out
}

/// Batches FSEvents for paths the guest can see and sends them to the guest's
/// fsnotify agent, which replays them as inotify events (see `notify_op`).
fn forward_events(
    server: Arc<Server>,
    root: PathBuf,
    tag: String,
    sock: PathBuf,
    rx: std::sync::mpsc::Receiver<crate::fsevents::Event>,
) {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};
    loop {
        let Ok(first) = rx.recv() else { return };
        let mut batch = vec![first];
        let until = Instant::now() + Duration::from_millis(200);
        while let Ok(ev) = rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
            batch.push(ev);
        }
        // Only paths a line can carry, inside an allowed directory, and not
        // the echo of something the guest did itself.
        batch.retain(|(p, _, _)| {
            p.strip_prefix(&root)
                .ok()
                .and_then(|r| r.to_str())
                .is_some_and(|r| !r.is_empty() && !r.contains('\n') && !r.contains('\t'))
                && matches!(server.root.access(p), Access::Full)
                && !server.recently_touched(p)
        });
        // Look at the Mac without holding the node table (FUSE requests wait on it).
        let near: std::collections::HashSet<PathBuf> = {
            let t = server.tables.lock().unwrap();
            let in_batch: std::collections::HashSet<&Path> = batch.iter().map(|(p, _, _)| p.as_path()).collect();
            let known = |p: &Path| t.by_path.contains_key(p);
            let near = |p: &Path| known(p) || p.parent().is_some_and(|d| known(d) || in_batch.contains(d));
            batch.iter().map(|(p, _, _)| p).filter(|p| near(p)).cloned().collect()
        };
        let mac: HashMap<&Path, Option<Attr>> = near.iter().map(|p| (p.as_path(), server.root.lstat(p).ok())).collect();
        let ops = {
            let t = server.tables.lock().unwrap();
            plan_replay(
                &batch,
                |p| t.by_path.get(p).and_then(|id| t.nodes.get(id)).map(|n| n.dir),
                |d| {
                    let below = t.by_path.iter().filter(|(p, _)| p.starts_with(d) && *p != d);
                    below.filter_map(|(p, id)| Some((p.clone(), t.nodes.get(id)?.dir))).collect()
                },
                |p| mac.get(p).copied().flatten(),
            )
        };
        if std::env::var_os("ORBIT_DEBUG").is_some() {
            eprintln!("fsnotify: {batch:x?} -> {ops:?}");
        }
        if ops.is_empty() {
            continue;
        }
        let rel = |p: &Path| p.strip_prefix(&root).unwrap().to_str().unwrap().to_string();
        let mut msg = String::from("orbit-fsnotify 3\n");
        let mut queued = Vec::new();
        for op in &ops {
            let line = match op {
                Replay::Modify(p) => format!("M\t{tag}\t{}", rel(p)),
                Replay::Create { path, dir } => {
                    queued.push((path.clone(), Pending::Appeared));
                    format!("{}\t{tag}\t{}", if *dir { "D" } else { "C" }, rel(path))
                }
                Replay::Remove { path, dir } => {
                    queued.push((path.clone(), Pending::Gone { dir: *dir }));
                    format!("{}\t{tag}\t{}", if *dir { "X" } else { "R" }, rel(path))
                }
                Replay::Move { from, to, dir } => {
                    queued.push((from.clone(), Pending::Gone { dir: *dir }));
                    queued.push((to.clone(), Pending::Appeared));
                    format!("V\t{tag}\t{}\t{}", rel(from), rel(to))
                }
                Replay::Settle => "S".to_string(),
            };
            msg.push_str(&line);
            msg.push('\n');
        }
        // Let attributes the guest cached before the change expire, so the
        // watcher that wakes up reads the new size/content.
        let (s, ns) = attr_ttl();
        std::thread::sleep(Duration::new(s, ns) + Duration::from_millis(100));
        server.pending.lock().unwrap().extend(queued.iter().cloned());
        if let Ok(mut conn) = std::os::unix::net::UnixStream::connect(&sock) {
            let _ = conn.write_all(msg.as_bytes());
            let _ = conn.shutdown(std::net::Shutdown::Write);
            // The agent closes the connection when it is done.
            let _ = conn.set_read_timeout(Some(Duration::from_secs(60)));
            let _ = conn.read_to_end(&mut Vec::new());
        }
        let mut pending = server.pending.lock().unwrap();
        for (p, _) in queued {
            pending.remove(&p);
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
        let mut fs_root = Root::open(&root, &share.allow)?;
        if let Some(dir) = share.reveal.as_ref().filter(|d| d.starts_with(&root)) {
            if let Err(e) = fs_root.reveal(dir) {
                eprintln!("orbit-vmm: virtiofs: {e}");
            }
        }
        let server = Arc::new(Server::new(fs_root));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsevents::*;

    fn plan(batch: &[(&str, u32, u64)], known: &[&str], mac: &[(&str, bool)]) -> Vec<Replay> {
        let batch: Vec<Event> = batch.iter().map(|(p, f, i)| (PathBuf::from(p), *f, *i)).collect();
        let attr = |dir: bool| Attr { mode: if dir { DIR | 0o755 } else { REG | 0o644 }, ..Default::default() };
        plan_replay(
            &batch,
            |p| known.iter().find(|k| Path::new(k) == p).map(|k| !k.contains('.')),
            |d| {
                let below = known.iter().filter(|k| Path::new(k).starts_with(d) && Path::new(k) != d);
                below.map(|k| (pb(k), !k.contains('.'))).collect()
            },
            |p| mac.iter().find(|(m, _)| Path::new(m) == p).map(|(_, d)| attr(*d)),
        )
    }

    fn pb(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    #[test]
    fn new_file_and_new_subdir_contents() {
        let ops = plan(
            &[
                ("/r/w/sub/a.py", ITEM_CREATED, 3),
                ("/r/w/sub", ITEM_CREATED | ITEM_IS_DIR, 2),
                ("/r/w/b.py", ITEM_CREATED, 4),
            ],
            &["/r/w"],
            &[("/r/w/sub", true), ("/r/w/sub/a.py", false), ("/r/w/b.py", false)],
        );
        assert_eq!(
            ops,
            vec![
                Replay::Create { path: pb("/r/w/sub"), dir: true },
                Replay::Create { path: pb("/r/w/b.py"), dir: false },
                Replay::Settle,
                Replay::Create { path: pb("/r/w/sub/a.py"), dir: false },
            ]
        );
    }

    #[test]
    fn rename_delete_modify() {
        let ops = plan(
            &[
                ("/r/w/old.py", ITEM_RENAMED, 10),
                ("/r/w/new.py", ITEM_RENAMED, 11),
                ("/r/w/gone.py", ITEM_REMOVED, 12),
                ("/r/w/edit.py", ITEM_MODIFIED | ITEM_CREATED, 13),
            ],
            &["/r/w", "/r/w/old.py", "/r/w/gone.py", "/r/w/edit.py"],
            &[("/r/w/new.py", false), ("/r/w/edit.py", false)],
        );
        assert_eq!(
            ops,
            vec![
                Replay::Move { from: pb("/r/w/old.py"), to: pb("/r/w/new.py"), dir: false },
                Replay::Remove { path: pb("/r/w/gone.py"), dir: false },
                Replay::Modify(pb("/r/w/edit.py")),
            ]
        );
    }

    #[test]
    fn ignores_unrelated_and_transient() {
        // Unknown directory, an existing dir the guest never looked at, and an
        // editor's temp file that came and went.
        let ops = plan(
            &[
                ("/r/x/a.py", ITEM_MODIFIED, 1),
                ("/r/w/old", ITEM_MODIFIED | ITEM_IS_DIR, 2),
                ("/r/w/.tmp", ITEM_CREATED | ITEM_REMOVED, 3),
            ],
            &["/r/w"],
            &[("/r/x/a.py", false), ("/r/w/old", true)],
        );
        assert_eq!(ops, vec![]);
    }

    #[test]
    fn removed_dir_takes_known_contents() {
        let ops = plan(
            &[("/r/w/d", ITEM_REMOVED | ITEM_IS_DIR, 1)],
            &["/r/w", "/r/w/d", "/r/w/d/s", "/r/w/d/s/f.txt", "/r/w/d/g.txt"],
            &[],
        );
        assert_eq!(ops.len(), 4);
        assert_eq!(ops[0], Replay::Remove { path: pb("/r/w/d/s/f.txt"), dir: false });
        assert_eq!(ops[3], Replay::Remove { path: pb("/r/w/d"), dir: true });
        assert!(ops.contains(&Replay::Remove { path: pb("/r/w/d/s"), dir: true }));
    }

    #[test]
    fn big_new_tree_is_only_its_top() {
        let mut batch = vec![("/r/w/big".to_string(), ITEM_CREATED | ITEM_IS_DIR, 0)];
        let mut mac = vec![("/r/w/big".to_string(), true)];
        for i in 0..=MAX_NEW_TREE {
            batch.push((format!("/r/w/big/f{i}"), ITEM_CREATED, i as u64 + 1));
            mac.push((format!("/r/w/big/f{i}"), false));
        }
        let batch: Vec<(&str, u32, u64)> = batch.iter().map(|(p, f, i)| (p.as_str(), *f, *i)).collect();
        let mac: Vec<(&str, bool)> = mac.iter().map(|(p, d)| (p.as_str(), *d)).collect();
        assert_eq!(plan(&batch, &["/r/w"], &mac), vec![Replay::Create { path: pb("/r/w/big"), dir: true }]);
    }

    #[test]
    fn atomic_save_is_a_modify() {
        // write temp, rename over the original: the guest never saw the temp.
        let ops = plan(
            &[("/r/w/.a.py.tmp", ITEM_CREATED | ITEM_RENAMED, 1), ("/r/w/a.py", ITEM_RENAMED, 2)],
            &["/r/w", "/r/w/a.py"],
            &[("/r/w/a.py", false)],
        );
        assert_eq!(ops, vec![Replay::Modify(pb("/r/w/a.py"))]);
    }
}
