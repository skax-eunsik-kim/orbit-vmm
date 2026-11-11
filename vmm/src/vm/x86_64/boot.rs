//! Linux x86 64-bit boot protocol (Documentation/arch/x86/boot.rst): load the
//! bzImage's protected-mode kernel at 1 MiB, fill the zero page (setup
//! header, command line, initrd, e820 map), and build what the 64-bit entry
//! point expects to find already set up: identity page tables for the low
//! 4 GiB and a GDT with __BOOT_CS = 0x10 / __BOOT_DS = 0x18.

use std::fs;
use std::path::Path;

use crate::memory::GuestMemory;

pub const ZERO_PAGE: u64 = 0x7000;
pub const GDT: u64 = 0x500;
pub const GDT_LIMIT: u64 = 5 * 8 - 1;
pub const PML4: u64 = 0x9000;
const PDPT: u64 = 0xa000;
const PD: u64 = 0xb000; // four tables, 0xb000..0xf000
const CMDLINE: u64 = 0x20000;
const KERNEL_LOAD: u64 = 0x10_0000;
/// Low memory the kernel must not use: our MP table (mptable.rs) and the
/// legacy BIOS area it lives in.
pub const BIOS_RESERVED: (u64, u64) = (0x9_fc00, 0x10_0000);

pub const BOOT_CS: u16 = 0x10;
pub const BOOT_DS: u16 = 0x18;
pub const BOOT_TR: u16 = 0x20;

const HDRS: u32 = 0x5372_6448; // "HdrS"
const XLF_KERNEL_64: u16 = 1 << 0;
const LOADED_HIGH: u8 = 1 << 0;
const E820_RAM: u32 = 1;
const E820_RESERVED: u32 = 2;
const MAX_E820: usize = 128;

pub struct Kernel {
    /// 64-bit entry point (startup_64 of the decompressor).
    pub entry: u64,
    /// The zero page with the setup header copied in; the rest is filled by `write_zero_page`.
    zero_page: Vec<u8>,
    init_end: u64,
    initrd_addr_max: u64,
    cmdline_size: u64,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Parses `image` (a bzImage) and copies its protected-mode part to 1 MiB.
pub fn load_kernel_image(mem: &GuestMemory, image: &[u8]) -> Result<Kernel, String> {
    if image.len() < 0x268 || u32_at(image, 0x202) != HDRS {
        return Err("not a bzImage (no HdrS setup header)".into());
    }
    let version = u16_at(image, 0x206);
    if version < 0x020c || u16_at(image, 0x236) & XLF_KERNEL_64 == 0 {
        return Err(format!("kernel has no 64-bit entry point (boot protocol {version:#x})"));
    }
    let setup_sects = match image[0x1f1] {
        0 => 4,
        n => n as usize,
    };
    let pm_offset = (setup_sects + 1) * 512;
    let pm = image.get(pm_offset..).ok_or("bzImage truncated")?;
    let init_size = u32_at(image, 0x260) as u64;
    if !mem.write(KERNEL_LOAD, pm) {
        return Err("kernel does not fit in guest RAM".into());
    }
    let mut zero_page = vec![0u8; 4096];
    let header_end = 0x202 + image[0x201] as usize;
    zero_page[0x1f1..header_end].copy_from_slice(&image[0x1f1..header_end]);
    Ok(Kernel {
        entry: KERNEL_LOAD + 0x200,
        zero_page,
        init_end: KERNEL_LOAD + init_size.max(pm.len() as u64),
        initrd_addr_max: u32_at(image, 0x22c) as u64,
        cmdline_size: u32_at(image, 0x238) as u64,
    })
}

pub fn load_kernel(mem: &GuestMemory, path: &Path) -> Result<Kernel, String> {
    let image = fs::read(path).map_err(|e| format!("reading kernel {}: {e}", path.display()))?;
    load_kernel_image(mem, &image).map_err(|e| format!("{}: {e}", path.display()))
}

/// The e820 map for RAM regions `ram` (gpa, size), with the BIOS area carved out.
pub fn e820(ram: &[(u64, u64)]) -> Vec<(u64, u64, u32)> {
    let mut map = Vec::new();
    for &(start, size) in ram {
        let end = start + size;
        let (r0, r1) = BIOS_RESERVED;
        if start < r0 && end > r1 {
            map.push((start, r0 - start, E820_RAM));
            map.push((r0, r1 - r0, E820_RESERVED));
            map.push((r1, end - r1, E820_RAM));
        } else {
            map.push((start, size, E820_RAM));
        }
    }
    map
}

/// Puts the initrd as high as the kernel allows in the RAM below 4 GiB.
pub fn place_initrd(k: &Kernel, low_end: u64, len: u64) -> Result<u64, String> {
    let top = low_end.min(k.initrd_addr_max + 1);
    let start = top.checked_sub(len).ok_or("initrd larger than low RAM")? & !0xfff;
    if start < k.init_end {
        return Err("initrd does not fit in guest RAM below 4 GiB".into());
    }
    Ok(start)
}

/// Fills in and writes the zero page, command line, page tables and GDT.
pub fn write_zero_page(
    mem: &GuestMemory,
    k: &Kernel,
    cmdline: &str,
    initrd: Option<(u64, u64)>,
    ram: &[(u64, u64)],
) -> Result<(), String> {
    let mut zp = k.zero_page.clone();
    let mut put32 = |o: usize, v: u32| zp[o..o + 4].copy_from_slice(&v.to_le_bytes());
    if cmdline.len() as u64 >= k.cmdline_size.max(256) {
        return Err(format!("kernel command line longer than {} bytes", k.cmdline_size));
    }
    let mut c = cmdline.as_bytes().to_vec();
    c.push(0);
    if !mem.write(CMDLINE, &c) {
        return Err("command line load failed".into());
    }
    put32(0x228, CMDLINE as u32); // cmd_line_ptr
    if let Some((start, len)) = initrd {
        put32(0x218, start as u32); // ramdisk_image
        put32(0x21c, len as u32); // ramdisk_size
        put32(0x0c0, (start >> 32) as u32); // ext_ramdisk_image
        put32(0x0c4, (len >> 32) as u32); // ext_ramdisk_size
    }
    zp[0x210] = 0xff; // type_of_loader: undefined
    zp[0x211] |= LOADED_HIGH;
    let map = e820(ram);
    if map.len() > MAX_E820 {
        return Err("too many e820 entries".into());
    }
    zp[0x1e8] = map.len() as u8;
    for (i, (addr, size, kind)) in map.iter().enumerate() {
        let o = 0x2d0 + i * 20;
        zp[o..o + 8].copy_from_slice(&addr.to_le_bytes());
        zp[o + 8..o + 16].copy_from_slice(&size.to_le_bytes());
        zp[o + 16..o + 20].copy_from_slice(&kind.to_le_bytes());
    }
    if !mem.write(ZERO_PAGE, &zp) {
        return Err("zero page load failed".into());
    }

    // Identity map 0..4 GiB with 2 MiB pages: PML4[0] -> PDPT, PDPT[0..4] -> PD.
    let ok = mem.write_obj(PML4, PDPT | 0x3)
        && (0..4).all(|i| mem.write_obj(PDPT + i * 8, (PD + i * 0x1000) | 0x3))
        && (0..4 * 512u64).all(|i| mem.write_obj(PD + i * 8, (i << 21) | 0x83));
    // null, null, __BOOT_CS (64-bit code), __BOOT_DS, a TSS descriptor for TR.
    let gdt: [u64; 5] = [0, 0, 0x00af_9b00_0000_ffff, 0x00cf_9300_0000_ffff, 0x0000_8b00_0000_0067];
    let ok = ok && gdt.iter().enumerate().all(|(i, d)| mem.write_obj(GDT + i as u64 * 8, *d));
    ok.then_some(()).ok_or_else(|| "page table/GDT load failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bzimage() -> Vec<u8> {
        let mut img = vec![0u8; 5 * 512 + 0x1000];
        img[0x1f1] = 4; // setup_sects
        img[0x201] = 0x66; // header ends at 0x268
        img[0x202..0x206].copy_from_slice(&HDRS.to_le_bytes());
        img[0x206..0x208].copy_from_slice(&0x020fu16.to_le_bytes());
        img[0x22c..0x230].copy_from_slice(&0x7fff_ffffu32.to_le_bytes()); // initrd_addr_max
        img[0x236..0x238].copy_from_slice(&3u16.to_le_bytes()); // xloadflags: 64-bit entry
        img[0x238..0x23c].copy_from_slice(&2048u32.to_le_bytes()); // cmdline_size
        img[0x260..0x264].copy_from_slice(&0x40_0000u32.to_le_bytes()); // init_size
        img[5 * 512] = 0xe9; // first byte of the protected-mode kernel
        img
    }

    #[test]
    fn loads_a_bzimage_and_writes_the_zero_page() {
        let size = 64 << 20;
        let mut buf = vec![0u8; size];
        let mem = unsafe { GuestMemory::from_raw(buf.as_mut_ptr(), size as u64) };
        let k = load_kernel_image(&mem, &bzimage()).unwrap();
        assert_eq!(k.entry, 0x10_0200);
        assert_eq!(mem.read_obj::<u8>(0x10_0000), Some(0xe9));

        let ram = [(0, size as u64)];
        let initrd = place_initrd(&k, size as u64, 0x10_0000).unwrap();
        assert_eq!(initrd, size as u64 - 0x10_0000);
        write_zero_page(&mem, &k, "console=ttyS0", Some((initrd, 0x10_0000)), &ram).unwrap();
        assert_eq!(mem.read_obj::<u32>(ZERO_PAGE + 0x202), Some(HDRS), "setup header copied");
        assert_eq!(mem.read_obj::<u32>(ZERO_PAGE + 0x218), Some(initrd as u32));
        assert_eq!(mem.read_obj::<u8>(ZERO_PAGE + 0x1e8), Some(3), "e820: RAM, BIOS hole, RAM");
        let mut c = [0u8; 13];
        mem.read(CMDLINE, &mut c);
        assert_eq!(&c, b"console=ttyS0");
        assert_eq!(mem.read_obj::<u64>(PD + 8), Some(0x20_0083), "2 MiB identity map");

        assert!(place_initrd(&k, size as u64, size as u64 - 0x30_0000).is_err(), "would overlap the kernel");
        assert!(load_kernel_image(&mem, &[0u8; 4096]).is_err());
    }

    #[test]
    fn e820_splits_out_the_bios_area_and_keeps_high_ram() {
        let map = e820(&[(0, 0xc000_0000), (0x1_0000_0000, 0x4000_0000)]);
        assert_eq!(
            map,
            vec![
                (0, 0x9_fc00, E820_RAM),
                (0x9_fc00, 0x6_0400, E820_RESERVED),
                (0x10_0000, 0xbff0_0000, E820_RAM),
                (0x1_0000_0000, 0x4000_0000, E820_RAM),
            ]
        );
    }
}
