//! Minimal macOS FSEvents binding: recursive, per-file change notifications
//! for a directory tree, delivered on a private dispatch queue.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

#[repr(C)]
struct FSEventStreamContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

type Callback = extern "C" fn(*const c_void, *mut c_void, usize, *const *const c_char, *const u32, *const u64);

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeArrayCallBacks: c_void;
    fn CFStringCreateWithCString(alloc: *const c_void, s: *const c_char, encoding: u32) -> *const c_void;
    fn CFArrayCreate(
        alloc: *const c_void,
        values: *const *const c_void,
        n: isize,
        callbacks: *const c_void,
    ) -> *const c_void;
}

#[link(name = "CoreServices", kind = "framework")]
extern "C" {
    fn FSEventStreamCreate(
        alloc: *const c_void,
        callback: Callback,
        context: *const FSEventStreamContext,
        paths: *const c_void,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
    fn FSEventStreamStart(stream: *mut c_void) -> bool;
}

extern "C" {
    fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> *mut c_void;
}

const UTF8: u32 = 0x0800_0100;
const SINCE_NOW: u64 = u64::MAX;
const FLAG_NO_DEFER: u32 = 0x02;
const FLAG_FILE_EVENTS: u32 = 0x10;

pub const ITEM_IS_FILE: u32 = 0x10000;
pub const ITEM_INODE_META_MOD: u32 = 0x400;
pub const ITEM_CREATED: u32 = 0x100;
pub const ITEM_MODIFIED: u32 = 0x1000;
pub const ITEM_RENAMED: u32 = 0x800;

extern "C" fn on_events(
    _stream: *const c_void,
    info: *mut c_void,
    n: usize,
    paths: *const *const c_char,
    flags: *const u32,
    _ids: *const u64,
) {
    let tx = unsafe { &*(info as *const Sender<(PathBuf, u32)>) };
    for i in 0..n {
        let path = unsafe { CStr::from_ptr(*paths.add(i)) };
        let flag = unsafe { *flags.add(i) };
        let _ = tx.send((PathBuf::from(path.to_string_lossy().into_owned()), flag));
    }
}

/// Starts watching `root` recursively; events arrive as (path, FSEvents flags).
/// The stream lives for the rest of the process.
pub fn watch(root: &Path, tx: Sender<(PathBuf, u32)>) -> Result<(), String> {
    let c_root = CString::new(root.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    let info = Box::into_raw(Box::new(tx)) as *mut c_void;
    unsafe {
        let cf_path = CFStringCreateWithCString(std::ptr::null(), c_root.as_ptr(), UTF8);
        let paths = CFArrayCreate(std::ptr::null(), &cf_path, 1, &kCFTypeArrayCallBacks as *const c_void);
        let ctx = FSEventStreamContext {
            version: 0,
            info,
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        };
        let stream = FSEventStreamCreate(
            std::ptr::null(),
            on_events,
            &ctx,
            paths,
            SINCE_NOW,
            0.02,
            FLAG_NO_DEFER | FLAG_FILE_EVENTS,
        );
        if stream.is_null() {
            return Err("FSEventStreamCreate failed".into());
        }
        let queue = dispatch_queue_create(c"orbit.fsevents".as_ptr(), std::ptr::null());
        FSEventStreamSetDispatchQueue(stream, queue);
        if !FSEventStreamStart(stream) {
            return Err("FSEventStreamStart failed".into());
        }
    }
    Ok(())
}
