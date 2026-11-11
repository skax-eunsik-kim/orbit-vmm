//! Linux arm64 boot protocol (Documentation/arch/arm64/booting.rst): place the
//! raw Image, initrd and DTB in guest RAM.

use std::fs;
use std::path::Path;

use crate::memory::{GuestMemory, RAM_BASE};

const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241; // "ARM\x64"
const MIB: u64 = 1 << 20;

pub struct KernelInfo {
    pub entry: u64,
    pub end: u64,
}

fn align_down(v: u64, a: u64) -> u64 {
    v & !(a - 1)
}

pub fn load_kernel(mem: &GuestMemory, path: &Path) -> Result<KernelInfo, String> {
    let image = fs::read(path).map_err(|e| format!("reading kernel {}: {e}", path.display()))?;
    if image.len() < 64 {
        return Err("kernel image too small".into());
    }
    let u64_at = |o: usize| u64::from_le_bytes(image[o..o + 8].try_into().unwrap());
    let magic = u32::from_le_bytes(image[56..60].try_into().unwrap());
    if magic != ARM64_IMAGE_MAGIC {
        return Err(format!("{} is not an uncompressed arm64 Image (bad magic 0x{magic:08x})", path.display()));
    }
    let mut text_offset = u64_at(8);
    let mut image_size = u64_at(16);
    if image_size == 0 {
        // Pre-3.17 kernels: text_offset is 0x80000 and image_size unknown.
        text_offset = 0x80000;
        image_size = image.len() as u64;
    }
    // RAM_BASE is 2 MiB aligned, as the protocol requires of the base.
    let entry = RAM_BASE + text_offset;
    let end = entry + image_size.max(image.len() as u64);
    if end > RAM_BASE + mem.size() {
        return Err("kernel does not fit in guest RAM".into());
    }
    if !mem.write(entry, &image) {
        return Err("kernel load failed".into());
    }
    Ok(KernelInfo { entry, end })
}

pub struct Placement {
    pub dtb: u64,
    pub initrd: Option<(u64, u64)>,
}

/// Choose where the DTB and initrd go: the DTB in the top 2 MiB of the first
/// 512 MiB of RAM (or of RAM, if smaller), the initrd right below it.
pub fn place(mem: &GuestMemory, kernel_end: u64, initrd_len: Option<u64>) -> Result<Placement, String> {
    let window_end = RAM_BASE + mem.size().min(512 * MIB);
    let dtb = window_end - 2 * MIB;
    let initrd = match initrd_len {
        None => None,
        Some(len) => {
            let mut start = align_down(dtb - len, 2 * MIB);
            if start < kernel_end {
                // Too big for the low window: put it at the very top of RAM.
                start = align_down(RAM_BASE + mem.size() - len, 2 * MIB);
                if start < dtb + 2 * MIB {
                    return Err("initrd does not fit in guest RAM".into());
                }
            }
            Some((start, start + len))
        }
    };
    if dtb < kernel_end {
        return Err("guest RAM too small for kernel + DTB".into());
    }
    Ok(Placement { dtb, initrd })
}

pub fn load_initrd(mem: &GuestMemory, gpa: u64, data: &[u8]) -> Result<(), String> {
    mem.write(gpa, data).then_some(()).ok_or_else(|| "initrd load failed".to_string())
}

pub fn load_dtb(mem: &GuestMemory, gpa: u64, dtb: &[u8]) -> Result<(), String> {
    if dtb.len() as u64 > 2 * MIB {
        return Err("DTB larger than 2 MiB".into());
    }
    mem.write(gpa, dtb).then_some(()).ok_or_else(|| "DTB load failed".to_string())
}
