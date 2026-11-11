//! Intel MultiProcessor Specification 1.4 table: how a guest without ACPI
//! learns the CPUs (local APIC IDs), the IOAPIC and which IOAPIC pin each
//! ISA IRQ is wired to. Linux finds the floating pointer by scanning the last
//! KiB of base memory (639 KiB).

use crate::memory::GuestMemory;

pub const ADDR: u64 = 639 * 1024;
const APIC_VERSION: u8 = 0x14;
const IOAPIC_VERSION: u8 = 0x11;

fn checksum(b: &[u8]) -> u8 {
    0u8.wrapping_sub(b.iter().fold(0u8, |a, &x| a.wrapping_add(x)))
}

/// The table for `cpus` vCPUs (APIC IDs 0..cpus), one IOAPIC with `pins`
/// inputs at `ioapic_addr` (ID `cpus`), ISA IRQ n -> pin n. `cpu_signature`
/// and `cpu_features` are CPUID.1 EAX and EDX.
pub fn build(cpus: u32, ioapic_addr: u32, pins: u8, cpu_signature: u32, cpu_features: u32) -> Vec<u8> {
    let mut entries = Vec::new();
    let mut count = 0u16;
    for id in 0..cpus {
        let flags = 1 | if id == 0 { 2 } else { 0 }; // enabled, bootstrap processor
        entries.extend_from_slice(&[0, id as u8, APIC_VERSION, flags]);
        entries.extend_from_slice(&cpu_signature.to_le_bytes());
        entries.extend_from_slice(&cpu_features.to_le_bytes());
        entries.extend_from_slice(&[0; 8]);
        count += 1;
    }
    entries.extend_from_slice(&[1, 0]); // bus 0
    entries.extend_from_slice(b"ISA   ");
    count += 1;
    let ioapic_id = cpus as u8;
    entries.extend_from_slice(&[2, ioapic_id, IOAPIC_VERSION, 1]);
    entries.extend_from_slice(&ioapic_addr.to_le_bytes());
    count += 1;
    for pin in 0..pins {
        // I/O interrupt: INT, conforming polarity/trigger (edge, active high for ISA).
        entries.extend_from_slice(&[3, 0, 0, 0, 0, pin, ioapic_id, pin]);
        count += 1;
    }
    // Local interrupts: LINT0 = ExtINT, LINT1 = NMI, to every local APIC.
    entries.extend_from_slice(&[4, 3, 0, 0, 0, 0, 0xff, 0]);
    entries.extend_from_slice(&[4, 1, 0, 0, 0, 0, 0xff, 1]);
    count += 2;

    let table_addr = ADDR as u32 + 16;
    let mut table = Vec::new();
    table.extend_from_slice(b"PCMP");
    table.extend_from_slice(&((44 + entries.len()) as u16).to_le_bytes());
    table.push(4); // spec revision 1.4
    table.push(0); // checksum, below
    table.extend_from_slice(b"ORBIT   ");
    table.extend_from_slice(b"ORBIT-VMM   ");
    table.extend_from_slice(&[0; 6]); // OEM table pointer, size
    table.extend_from_slice(&count.to_le_bytes());
    table.extend_from_slice(&0xfee0_0000u32.to_le_bytes()); // local APIC address
    table.extend_from_slice(&[0; 4]); // extended table length, checksum, reserved
    table.extend_from_slice(&entries);
    table[7] = checksum(&table);

    let mut fp = Vec::new();
    fp.extend_from_slice(b"_MP_");
    fp.extend_from_slice(&table_addr.to_le_bytes());
    fp.extend_from_slice(&[1, 4, 0, 0, 0, 0, 0, 0]); // length 16 bytes, rev 1.4, checksum, no default config
    fp[10] = checksum(&fp);

    fp.extend_from_slice(&table);
    fp
}

pub fn write(mem: &GuestMemory, table: &[u8]) -> Result<(), String> {
    if ADDR + table.len() as u64 > 0xa_0000 {
        return Err("MP table too large for the base-memory window".into());
    }
    mem.write(ADDR, table).then_some(()).ok_or_else(|| "MP table load failed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_and_counts() {
        let t = build(4, 0xfec0_0000, 24, 0x906ea, 0xbfeb_fbff);
        let sum = |b: &[u8]| b.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        assert_eq!(&t[..4], b"_MP_");
        assert_eq!(sum(&t[..16]), 0);
        let table = &t[16..];
        assert_eq!(&table[..4], b"PCMP");
        let len = u16::from_le_bytes([table[4], table[5]]) as usize;
        assert_eq!(len, table.len());
        assert_eq!(sum(table), 0);
        assert_eq!(u16::from_le_bytes([table[34], table[35]]), 4 + 1 + 1 + 24 + 2);
        assert_eq!(table[44 + 3], 3, "CPU 0 is enabled and the BSP");
        assert!(t.len() as u64 + ADDR <= 0xa_0000);
    }
}
