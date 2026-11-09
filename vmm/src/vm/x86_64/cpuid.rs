//! CPUID as the guest sees it: the host's answers, minus what this VMM does
//! not virtualize (VMX, performance counters, MONITOR/MWAIT, TSC-deadline,
//! x2APIC, SGX, PT, ARCH_CAPABILITIES), plus a topology of `cpus`
//! single-threaded cores and the TSC/APIC-timer frequencies in leaf 0x15, so
//! Linux needs neither a PIT nor an HPET to calibrate.

#[derive(Clone, Copy, Debug)]
pub struct Ctx {
    pub index: u32,
    pub cpus: u32,
    pub tsc_hz: u64,
    /// Local APIC timer input clock (hv_vm_set_apic_bus_freq).
    pub bus_hz: u64,
    /// Guest CR4.OSXSAVE, mirrored into CPUID.1:ECX[27].
    pub osxsave: bool,
    pub rdtscp: bool,
    pub invpcid: bool,
}

pub const SIGNATURE: &[u8; 12] = b"OrbitVMMOrbt";
const HYPERVISOR_BASE: u32 = 0x4000_0000;

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn bits(list: &[u32]) -> u32 {
    list.iter().fold(0, |a, b| a | (1 << b))
}

/// The guest's view of `leaf`/`subleaf`, given the host's `[eax, ebx, ecx, edx]`.
pub fn filter(leaf: u32, subleaf: u32, host: [u32; 4], c: &Ctx) -> [u32; 4] {
    let [mut a, mut b, mut cx, mut d] = host;
    match leaf {
        0 => {}
        1 => {
            // ECX: drop DTES64, MONITOR, DS-CPL, VMX, SMX, EST, TM2, xTPR, PDCM,
            // x2APIC, TSC-deadline; OSXSAVE follows the guest's CR4; add HYPERVISOR.
            cx &= !bits(&[2, 3, 4, 5, 6, 7, 8, 14, 15, 21, 24, 27]);
            if c.osxsave {
                cx |= 1 << 27;
            }
            cx |= 1 << 31;
            // EDX: drop DS, ACPI (thermal), TM, PBE; HTT when there are siblings.
            d &= !bits(&[21, 22, 29, 31]);
            d = if c.cpus > 1 { d | (1 << 28) } else { d & !(1 << 28) };
            b = (b & 0xffff) | ((c.cpus.min(255)) << 16) | (c.index << 24);
        }
        4 => {
            if a & 0x1f != 0 {
                let level = (a >> 5) & 7;
                let sharing = if level >= 3 { c.cpus - 1 } else { 0 };
                a = (a & 0x3fff) | (sharing << 14) | ((c.cpus - 1) << 26);
            }
        }
        5 | 0xa | 0x10 | 0x12 | 0x14 | 0x1b | 0x1f => [a, b, cx, d] = [0; 4],
        6 => [a, b, cx, d] = [1 << 2, 0, 0, 0], // ARAT: the APIC timer keeps running in idle
        7 if subleaf == 0 => {
            // EBX: drop SGX, HLE, INVPCID (unless enabled), RTM, PQM, MPX, PQE, PT.
            b &= !bits(&[2, 4, 11, 12, 14, 15, 25]);
            if !c.invpcid {
                b &= !(1 << 10);
            }
            // ECX: drop WAITPKG, PKU/OSPKE, LA57, SGX-LC.
            cx &= !bits(&[3, 4, 5, 16, 30]);
            // EDX: SPEC_CTRL, PRED_CMD and FLUSH_CMD are native MSRs (vcpu.rs),
            // so IBRS/IBPB, STIBP, L1D_FLUSH and SSBD stay; ARCH_CAPABILITIES
            // would need the host's value, so it goes.
            d &= !(1 << 29);
        }
        7 => [a, b, cx, d] = [0; 4],
        0xb => {
            let shift = 32 - (c.cpus.max(1) - 1).leading_zeros();
            [a, b, cx] = match subleaf {
                0 => [0, 1, 0x100],
                1 => [shift, c.cpus, 0x200 | 1],
                n => [0, 0, n],
            };
            d = c.index;
        }
        0xd => match subleaf {
            // XCR0 limited to x87 | SSE | AVX; 832 bytes of XSAVE area.
            0 => [a, b, cx, d] = [a & 0x7, 0x340, 0x340, 0],
            1 => a &= !(1 << 3), // no XSAVES
            2 => {}
            _ => [a, b, cx, d] = [0; 4],
        },
        0x15 => {
            let g = gcd(c.tsc_hz, c.bus_hz);
            let (den, num) = (c.bus_hz / g, c.tsc_hz / g);
            if den <= u32::MAX as u64 && num <= u32::MAX as u64 {
                [a, b, cx, d] = [den as u32, num as u32, c.bus_hz as u32, 0];
            } else {
                [a, b, cx, d] = [0; 4];
            }
        }
        0x16 => {
            let mhz = (c.tsc_hz / 1_000_000) as u32;
            [a, b, cx, d] = [mhz, mhz, (c.bus_hz / 1_000_000) as u32, 0];
        }
        HYPERVISOR_BASE => {
            let s = SIGNATURE;
            let w = |i: usize| u32::from_le_bytes([s[i], s[i + 1], s[i + 2], s[i + 3]]);
            [a, b, cx, d] = [HYPERVISOR_BASE, w(0), w(4), w(8)];
        }
        0x4000_0001..=0x4fff_ffff => [a, b, cx, d] = [0; 4],
        0x8000_0001 => {
            if !c.rdtscp {
                d &= !(1 << 27);
            }
        }
        0x8000_0007 => [a, b, cx, d] = [0, 0, 0, d & (1 << 8)], // invariant TSC only
        0x8000_0008 => b = 0,                                   // no IBPB/IBRS/SSBD hints
        _ => {}
    }
    [a, b, cx, d]
}

/// The host's CPUID.
pub fn host(leaf: u32, subleaf: u32) -> [u32; 4] {
    let r = std::arch::x86_64::__cpuid_count(leaf, subleaf);
    [r.eax, r.ebx, r.ecx, r.edx]
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTX: Ctx = Ctx {
        index: 2,
        cpus: 4,
        tsc_hz: 2_592_000_000,
        bus_hz: 24_000_000,
        osxsave: true,
        rdtscp: false,
        invpcid: false,
    };

    #[test]
    fn hides_what_is_not_virtualized_and_describes_the_vm() {
        let [_, b, c, d] = filter(1, 0, [0x906ea, 0x0010_0800, 0x7ffa_fbbf, 0xbfeb_fbff], &CTX);
        assert_eq!(c & (1 << 5), 0, "no VMX");
        assert_eq!(c & (1 << 21), 0, "no x2APIC");
        assert_ne!(c & (1 << 31), 0, "hypervisor present");
        assert_ne!(c & (1 << 27), 0, "OSXSAVE mirrors CR4");
        assert_eq!(b >> 24, 2, "APIC ID = vCPU index");
        assert_eq!((b >> 16) & 0xff, 4);
        assert_ne!(d & (1 << 28), 0);

        let [den, num, crystal, _] = filter(0x15, 0, [0; 4], &CTX);
        assert_eq!(crystal as u64 * num as u64 / den as u64, CTX.tsc_hz, "TSC = crystal * num / den");
        assert_eq!(crystal as u64, CTX.bus_hz, "Linux takes the APIC timer rate from the crystal");

        let [_, b, c, d] = filter(HYPERVISOR_BASE, 0, [0; 4], &CTX);
        let sig: Vec<u8> = [b, c, d].iter().flat_map(|w| w.to_le_bytes()).collect();
        assert_eq!(sig, SIGNATURE);

        assert_eq!(filter(0xb, 1, [0; 4], &CTX), [2, 4, 0x201, 2], "4 cores: 2-bit core ID shift");
        assert_eq!(filter(0x8000_0001, 0, [0, 0, 0, 1 << 27], &CTX)[3], 0, "RDTSCP hidden unless enabled");
        assert_eq!(filter(0xa, 0, [7, 7, 7, 7], &CTX), [0; 4], "no PMU");
    }
}
