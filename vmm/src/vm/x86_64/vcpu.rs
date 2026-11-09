//! vCPU threads: VMCS setup, the hv_vcpu_run_until loop, and the exits the
//! kernel leaves to us: CPUID, MSRs, control registers, port I/O, device MMIO
//! (EPT violations, decoded with decode.rs), local APIC accesses (forwarded
//! to the in-kernel APIC), INIT/SIPI, and HLT (idle.rs).

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::decode::{self, Access};
use super::hvf::{self, hv, hv_vcpuid_t};
use super::idle::{self, Kicker};
use super::{boot, cpuid, layout, rtc, serial};
use crate::devices::MmioBus;
use crate::memory::GuestMemory;

/// Process exit status when the guest requests a reset.
pub const EXIT_RESET: i32 = 3;

const CR0_PE: u64 = 1 << 0;
const CR0_ET: u64 = 1 << 4;
const CR0_NE: u64 = 1 << 5;
const CR0_NW: u64 = 1 << 29;
const CR0_CD: u64 = 1 << 30;
const CR0_PG: u64 = 1 << 31;
const CR4_PAE: u64 = 1 << 5;
const CR4_VMXE: u64 = 1 << 13;
const CR4_OSXSAVE: u64 = 1 << 18;
const EFER_LME: u64 = 1 << 8;
const EFER_LMA: u64 = 1 << 10;
const EFER_NXE: u64 = 1 << 11;
const RFLAGS_IF: u64 = 1 << 9;
const ACTIVITY_HLT: u64 = 1;

/// General registers in ModRM order (RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8..R15).
const GPRS: [u32; 16] = [
    hvf::HV_X86_RAX,
    hvf::HV_X86_RCX,
    hvf::HV_X86_RDX,
    hvf::HV_X86_RBX,
    hvf::HV_X86_RSP,
    hvf::HV_X86_RBP,
    hvf::HV_X86_RSI,
    hvf::HV_X86_RDI,
    hvf::HV_X86_R8,
    hvf::HV_X86_R9,
    hvf::HV_X86_R10,
    hvf::HV_X86_R11,
    hvf::HV_X86_R12,
    hvf::HV_X86_R13,
    hvf::HV_X86_R14,
    hvf::HV_X86_R15,
];

/// The guest's MSRs that hardware or the kernel can't hold for us.
const NATIVE_MSRS: [u32; 12] = [
    0xc000_0081, // STAR
    0xc000_0082, // LSTAR
    0xc000_0083, // CSTAR
    0xc000_0084, // SFMASK
    0xc000_0100, // FS base
    0xc000_0101, // GS base
    0xc000_0102, // kernel GS base
    0xc000_0103, // TSC_AUX
    0x0000_0010, // TSC
    0x0000_0174, // SYSENTER_CS
    0x0000_0175, // SYSENTER_ESP
    0x0000_0176, // SYSENTER_EIP
];

enum Start {
    /// vCPU 0: the kernel's 64-bit entry point.
    Boot { entry: u64 },
    /// An AP woken by INIT/SIPI, in real mode at this address.
    Sipi { rip: u64 },
}

#[derive(Default)]
struct Slot {
    start: Mutex<Option<Start>>,
    cond: Condvar,
    running: AtomicBool,
    /// Halted with interrupts off (Linux's halt/poweroff end state), as of the
    /// last time this vCPU was made to exit.
    dead: AtomicBool,
}

pub struct Machine {
    pub bus: Arc<MmioBus>,
    pub mem: Arc<GuestMemory>,
    serial: serial::Serial,
    rtc: rtc::Rtc,
    kicker: Arc<Kicker>,
    tsc_hz: u64,
    slots: Vec<Slot>,
    /// hv_vcpuid_t of each vCPU, by index (None until created).
    ids: Mutex<Vec<Option<hv_vcpuid_t>>>,
    created: (Mutex<usize>, Condvar),
    stop: AtomicBool,
    exit_code: AtomicI32,
}

impl Machine {
    pub fn new(
        bus: Arc<MmioBus>,
        mem: Arc<GuestMemory>,
        serial: serial::Serial,
        kicker: Arc<Kicker>,
        tsc_hz: u64,
        cpus: u32,
        entry: u64,
    ) -> Self {
        let slots: Vec<Slot> = (0..cpus).map(|_| Slot::default()).collect();
        *slots[0].start.lock().unwrap() = Some(Start::Boot { entry });
        Self {
            bus,
            mem,
            serial,
            rtc: rtc::Rtc::default(),
            kicker,
            tsc_hz,
            slots,
            ids: Mutex::new(vec![None; cpus as usize]),
            created: (Mutex::new(0), Condvar::new()),
            stop: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
        }
    }

    fn kick(&self, which: impl Fn(usize) -> bool) {
        let mut ids: Vec<hv_vcpuid_t> =
            self.ids.lock().unwrap().iter().enumerate().filter(|(i, _)| which(*i)).filter_map(|(_, id)| *id).collect();
        if !ids.is_empty() {
            let _ = hv!(hv_vcpu_interrupt(ids.as_mut_ptr(), ids.len() as u32));
        }
    }

    pub fn power_off(&self, code: i32) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        self.exit_code.store(code, Ordering::SeqCst);
        for slot in &self.slots {
            let _g = slot.start.lock().unwrap();
            slot.cond.notify_all();
        }
        self.kicker.kick();
        self.kick(|_| true);
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn index_of(&self, id: hv_vcpuid_t) -> Option<usize> {
        self.ids.lock().unwrap().iter().position(|x| *x == Some(id))
    }

    /// Without ACPI, Linux "powers off" by halting every CPU with interrupts
    /// disabled. A vCPU notices it is halted only when it exits, so once a
    /// second the running ones are made to exit (`idle_until_interrupt` then
    /// records the state); when every vCPU is in it, the guest is off.
    fn watch_for_halt(self: &Arc<Self>) {
        let m = self.clone();
        std::thread::Builder::new()
            .name("halt-watch".into())
            .spawn(move || {
                while !m.stopped() {
                    std::thread::sleep(Duration::from_secs(1));
                    let all_dead =
                        m.slots.iter().all(|s| !s.running.load(Ordering::SeqCst) || s.dead.load(Ordering::SeqCst));
                    if all_dead {
                        m.power_off(0);
                        return;
                    }
                    m.kick(|i| m.slots[i].running.load(Ordering::SeqCst));
                }
            })
            .expect("spawn halt watcher");
    }
}

/// Start all vCPU threads; vCPU 0 boots the kernel. Returns the exit code
/// once the guest powers off or resets.
pub fn run_all(machine: Arc<Machine>) -> i32 {
    let threads: Vec<_> = (0..machine.slots.len())
        .map(|i| {
            let m = machine.clone();
            std::thread::Builder::new()
                .name(format!("vcpu{i}"))
                .spawn(move || {
                    if let Err(e) = vcpu_thread(&m, i) {
                        eprintln!("orbit-vmm: vcpu{i}: {e}");
                        m.power_off(1);
                    }
                })
                .expect("spawn vcpu thread")
        })
        .collect();
    machine.watch_for_halt();
    for t in threads {
        let _ = t.join();
    }
    machine.exit_code.load(Ordering::SeqCst)
}

fn cap2ctrl(cap: u64, want: u64) -> u64 {
    (want | (cap & 0xffff_ffff)) & (cap >> 32)
}

fn capability(field: u32) -> u64 {
    let mut v = 0;
    let _ = hv!(hv_vmx_read_capability(field, &mut v));
    v
}

fn allowed(field: u32, bit: u64) -> bool {
    (capability(field) >> 32) & bit != 0
}

struct Vcpu {
    id: hv_vcpuid_t,
    index: usize,
    apic_base: u64,
    pat: u64,
    mtrr: std::collections::HashMap<u32, u64>,
    rdtscp: bool,
    invpcid: bool,
}

impl Vcpu {
    fn reg(&self, r: u32) -> u64 {
        let mut v = 0;
        let _ = hv!(hv_vcpu_read_register(self.id, r, &mut v));
        v
    }
    fn set_reg(&self, r: u32, v: u64) {
        let _ = hv!(hv_vcpu_write_register(self.id, r, v));
    }
    fn vmcs(&self, f: u32) -> u64 {
        let mut v = 0;
        let _ = hv!(hv_vmx_vcpu_read_vmcs(self.id, f, &mut v));
        v
    }
    fn set_vmcs(&self, f: u32, v: u64) {
        if let Err(e) = hv!(hv_vmx_vcpu_write_vmcs(self.id, f, v)) {
            eprintln!("orbit-vmm: vcpu{}: VMCS 0x{f:x} = 0x{v:x}: {e}", self.index);
        }
    }
    fn advance(&self) {
        let rip = self.vmcs(hvf::VMCS_GUEST_RIP);
        let len = self.vmcs(hvf::VMCS_RO_VMEXIT_INSTR_LEN);
        self.set_vmcs(hvf::VMCS_GUEST_RIP, rip + len);
    }

    fn seg(&self, n: u32, selector: u16, base: u64, limit: u32, ar: u32) {
        // VMCS segment fields are laid out ES, CS, SS, DS, FS, GS, LDTR, TR at stride 2.
        self.set_vmcs(hvf::VMCS_GUEST_ES + 2 * n, selector as u64);
        self.set_vmcs(hvf::VMCS_GUEST_ES_BASE + 2 * n, base);
        self.set_vmcs(hvf::VMCS_GUEST_ES_LIMIT + 2 * n, limit as u64);
        self.set_vmcs(hvf::VMCS_GUEST_ES_AR + 2 * n, ar as u64);
    }

    fn setup_controls(&mut self) {
        use hvf::*;
        self.set_vmcs(
            VMCS_CTRL_PIN_BASED,
            cap2ctrl(capability(HV_VMX_CAP_PINBASED), (PIN_BASED_INTR | PIN_BASED_NMI) as u64),
        );
        let proc = CPU_BASED_HLT
            | CPU_BASED_MWAIT
            | CPU_BASED_MONITOR
            | CPU_BASED_UNCOND_IO
            | CPU_BASED_MSR_BITMAPS
            | CPU_BASED_SECONDARY_CTLS;
        self.set_vmcs(VMCS_CTRL_CPU_BASED, cap2ctrl(capability(HV_VMX_CAP_PROCBASED), proc as u64));
        self.rdtscp = allowed(HV_VMX_CAP_PROCBASED2, CPU_BASED2_RDTSCP as u64);
        self.invpcid = allowed(HV_VMX_CAP_PROCBASED2, CPU_BASED2_INVPCID as u64);
        let mut proc2 = CPU_BASED2_UNRESTRICTED as u64;
        if self.rdtscp {
            proc2 |= CPU_BASED2_RDTSCP as u64;
        }
        if self.invpcid {
            proc2 |= CPU_BASED2_INVPCID as u64;
        }
        self.set_vmcs(VMCS_CTRL_CPU_BASED2, cap2ctrl(capability(HV_VMX_CAP_PROCBASED2), proc2));
        let entry = VMENTRY_LOAD_EFER as u64;
        self.set_vmcs(VMCS_CTRL_VMENTRY_CONTROLS, cap2ctrl(capability(HV_VMX_CAP_ENTRY), entry));
        self.set_vmcs(VMCS_CTRL_VMEXIT_CONTROLS, cap2ctrl(capability(HV_VMX_CAP_EXIT), 0));
        self.set_vmcs(VMCS_CTRL_EXC_BITMAP, 0);
        self.set_vmcs(VMCS_CTRL_CR0_MASK, CR0_PE | CR0_NE | CR0_NW | CR0_CD | CR0_PG);
        self.set_vmcs(VMCS_CTRL_CR4_MASK, CR4_VMXE);
        for msr in NATIVE_MSRS {
            if msr == 0xc000_0103 && !self.rdtscp {
                continue;
            }
            if let Err(e) = hv!(hv_vcpu_enable_native_msr(self.id, msr, true)) {
                eprintln!("orbit-vmm: vcpu{}: native MSR 0x{msr:x}: {e}", self.index);
            }
        }
    }

    fn set_cr0(&self, cr0: u64) {
        let mut efer = self.vmcs(hvf::VMCS_GUEST_IA32_EFER);
        let mut entry = self.vmcs(hvf::VMCS_CTRL_VMENTRY_CONTROLS);
        if cr0 & CR0_PG != 0 && efer & EFER_LME != 0 && efer & EFER_LMA == 0 {
            efer |= EFER_LMA;
            entry |= hvf::VMENTRY_GUEST_IA32E as u64;
            // IA-32e mode requires TR to describe a busy 64-bit TSS.
            let ar = self.vmcs(hvf::VMCS_GUEST_TR_AR);
            self.set_vmcs(hvf::VMCS_GUEST_TR_AR, (ar & !0xf) | 0xb);
        } else if cr0 & CR0_PG == 0 && efer & EFER_LMA != 0 {
            efer &= !EFER_LMA;
            entry &= !(hvf::VMENTRY_GUEST_IA32E as u64);
        }
        self.set_vmcs(hvf::VMCS_GUEST_IA32_EFER, efer);
        self.set_vmcs(hvf::VMCS_CTRL_VMENTRY_CONTROLS, entry);
        self.set_vmcs(hvf::VMCS_CTRL_CR0_SHADOW, cr0);
        self.set_vmcs(hvf::VMCS_GUEST_CR0, (cr0 | CR0_NE | CR0_ET) & !(CR0_CD | CR0_NW));
        let _ = hv!(hv_vcpu_invalidate_tlb(self.id));
    }

    fn set_cr4(&self, cr4: u64) {
        self.set_vmcs(hvf::VMCS_CTRL_CR4_SHADOW, cr4);
        self.set_vmcs(hvf::VMCS_GUEST_CR4, cr4 | CR4_VMXE);
        let _ = hv!(hv_vcpu_invalidate_tlb(self.id));
    }

    fn set_efer(&self, value: u64) {
        let cr0 = self.vmcs(hvf::VMCS_CTRL_CR0_SHADOW);
        let mut efer = value & !EFER_LMA;
        if cr0 & CR0_PG != 0 && efer & EFER_LME != 0 {
            efer |= EFER_LMA;
        }
        let old = self.vmcs(hvf::VMCS_GUEST_IA32_EFER);
        self.set_vmcs(hvf::VMCS_GUEST_IA32_EFER, efer);
        if (old ^ efer) & EFER_NXE != 0 {
            let _ = hv!(hv_vcpu_invalidate_tlb(self.id));
        }
    }

    /// vCPU 0 at the kernel's 64-bit entry: long mode, identity paging, boot GDT.
    fn enter_long_mode(&self, entry: u64) {
        let (code, data) = (0xa09b, 0xc093);
        for n in 0..6 {
            let (sel, ar) = if n == 1 { (boot::BOOT_CS, code) } else { (boot::BOOT_DS, data) };
            self.seg(n, sel, 0, 0xffff_ffff, ar);
        }
        self.seg(6, 0, 0, 0, 0x1_0000); // LDTR unusable
        self.seg(7, boot::BOOT_TR, 0, 0x67, 0x8b);
        self.set_vmcs(hvf::VMCS_GUEST_GDTR_BASE, boot::GDT);
        self.set_vmcs(hvf::VMCS_GUEST_GDTR_LIMIT, boot::GDT_LIMIT);
        self.set_vmcs(hvf::VMCS_GUEST_IDTR_BASE, 0);
        self.set_vmcs(hvf::VMCS_GUEST_IDTR_LIMIT, 0);
        self.set_vmcs(hvf::VMCS_GUEST_CR3, boot::PML4);
        self.set_cr4(CR4_PAE);
        self.set_vmcs(hvf::VMCS_GUEST_IA32_EFER, EFER_LME);
        self.set_cr0(CR0_PE | CR0_ET | CR0_NE | CR0_PG);
        self.set_vmcs(hvf::VMCS_GUEST_RFLAGS, 2);
        self.set_vmcs(hvf::VMCS_GUEST_RIP, entry);
        self.set_reg(hvf::HV_X86_RSI, boot::ZERO_PAGE);
        self.set_vmcs(hvf::VMCS_GUEST_ACTIVITY_STATE, 0);
    }

    /// An AP after INIT + SIPI: real mode at `rip` (vector << 12).
    fn enter_real_mode(&self, rip: u64) {
        for n in 0..6 {
            let (sel, base, ar) = if n == 1 { ((rip >> 4) as u16, rip, 0x9b) } else { (0, 0, 0x93) };
            self.seg(n, sel, base, 0xffff, ar);
        }
        self.seg(6, 0, 0, 0xffff, 0x82);
        self.seg(7, 0, 0, 0xffff, 0x8b);
        self.set_vmcs(hvf::VMCS_GUEST_GDTR_BASE, 0);
        self.set_vmcs(hvf::VMCS_GUEST_GDTR_LIMIT, 0xffff);
        self.set_vmcs(hvf::VMCS_GUEST_IDTR_BASE, 0);
        self.set_vmcs(hvf::VMCS_GUEST_IDTR_LIMIT, 0xffff);
        self.set_vmcs(hvf::VMCS_GUEST_CR3, 0);
        self.set_cr4(0);
        self.set_vmcs(hvf::VMCS_GUEST_IA32_EFER, 0);
        self.set_cr0(CR0_ET | CR0_CD | CR0_NW);
        self.set_vmcs(hvf::VMCS_GUEST_RFLAGS, 2);
        self.set_vmcs(hvf::VMCS_GUEST_RIP, 0);
        self.set_vmcs(hvf::VMCS_GUEST_ACTIVITY_STATE, 0);
        self.set_vmcs(hvf::VMCS_GUEST_INTERRUPTIBILITY, 0);
        for r in GPRS {
            if r != hvf::HV_X86_RDX {
                self.set_reg(r, 0);
            }
        }
        self.set_reg(hvf::HV_X86_RDX, 0x600); // reset signature: family 6
    }

    fn inject_exception(&self, vector: u8, error: Option<u32>) {
        let mut info = vector as u64 | hvf::IRQ_INFO_HARD_EXC as u64 | hvf::IRQ_INFO_VALID as u64;
        if let Some(code) = error {
            info |= hvf::IRQ_INFO_ERROR_VALID as u64;
            self.set_vmcs(hvf::VMCS_CTRL_VMENTRY_EXC_ERROR, code as u64);
        }
        self.set_vmcs(hvf::VMCS_CTRL_VMENTRY_IRQ_INFO, info);
    }

    /// An exit that interrupted event delivery (e.g. the guest stack page
    /// faulting in through EPT while pushing an interrupt frame) must deliver
    /// the event again on the next entry.
    fn reinject_vectoring(&self) {
        let info = self.vmcs(hvf::VMCS_RO_IDT_VECTOR_INFO);
        if info & hvf::IRQ_INFO_VALID as u64 == 0
            || self.vmcs(hvf::VMCS_CTRL_VMENTRY_IRQ_INFO) & hvf::IRQ_INFO_VALID as u64 != 0
        {
            return;
        }
        let kind = info & 0x700;
        if kind == hvf::IRQ_INFO_SOFT_IRQ as u64 || kind == hvf::IRQ_INFO_SOFT_EXC as u64 {
            self.set_vmcs(hvf::VMCS_CTRL_VMENTRY_INSTR_LEN, self.vmcs(hvf::VMCS_RO_VMEXIT_INSTR_LEN));
        }
        if info & hvf::IRQ_INFO_ERROR_VALID as u64 != 0 {
            self.set_vmcs(hvf::VMCS_CTRL_VMENTRY_EXC_ERROR, self.vmcs(hvf::VMCS_RO_IDT_VECTOR_ERROR));
        }
        self.set_vmcs(hvf::VMCS_CTRL_VMENTRY_IRQ_INFO, info & !(1 << 12));
    }

    /// Guest-linear to guest-physical, for fetching the faulting instruction.
    fn translate(&self, mem: &GuestMemory, la: u64) -> Option<u64> {
        if self.vmcs(hvf::VMCS_GUEST_CR0) & CR0_PG == 0 {
            return Some(la);
        }
        if self.vmcs(hvf::VMCS_GUEST_IA32_EFER) & EFER_LMA == 0 {
            return None; // 32-bit paging: nothing does device I/O there
        }
        const ADDR: u64 = 0x000f_ffff_ffff_f000;
        let mut table = self.vmcs(hvf::VMCS_GUEST_CR3) & ADDR;
        for shift in [39u32, 30, 21, 12] {
            let e: u64 = mem.read_obj(table + ((la >> shift) & 511) * 8)?;
            if e & 1 == 0 {
                return None;
            }
            if (shift == 30 || shift == 21) && e & 0x80 != 0 {
                let mask = (1u64 << shift) - 1;
                return Some((e & ADDR & !mask) | (la & mask));
            }
            table = e & ADDR;
        }
        Some(table | (la & 0xfff))
    }

    fn fetch(&self, mem: &GuestMemory) -> Result<Vec<u8>, String> {
        let rip = self.vmcs(hvf::VMCS_GUEST_RIP) + self.vmcs(hvf::VMCS_GUEST_CS_BASE);
        let mut code = Vec::with_capacity(15);
        let mut la = rip;
        while code.len() < 15 {
            let gpa = self.translate(mem, la).ok_or_else(|| format!("cannot translate RIP 0x{rip:x}"))?;
            let n = (15 - code.len()).min((0x1000 - (la & 0xfff)) as usize);
            let mut buf = vec![0u8; n];
            if !mem.read(gpa, &mut buf) {
                break; // the rest is outside RAM; decode with what we have
            }
            code.extend_from_slice(&buf);
            la += n as u64;
        }
        Ok(code)
    }
}

fn vcpu_thread(m: &Arc<Machine>, index: usize) -> Result<(), String> {
    // Create vCPUs in index order so hv_vcpuid_t (and the in-kernel APIC ID)
    // follows the vCPU index, which is what the MP table and CPUID report.
    let id = {
        let (lock, cond) = &m.created;
        let mut next = lock.lock().unwrap();
        while *next != index {
            next = cond.wait(next).unwrap();
        }
        let mut id: hv_vcpuid_t = 0;
        let r = hv!(hv_vcpu_create(&mut id, hvf::HV_VCPU_DEFAULT));
        *next += 1;
        cond.notify_all();
        r.map_err(|e| e.to_string())?;
        id
    };
    m.ids.lock().unwrap()[index] = Some(id);
    let mut apic_id = 0u32;
    if hv!(hv_vcpu_apic_read(id, 0x20, &mut apic_id)).is_ok() && apic_id >> 24 != index as u32 {
        eprintln!("orbit-vmm: vcpu{index}: local APIC ID {} differs from its index", apic_id >> 24);
    }
    let mut cpu = Vcpu {
        id,
        index,
        apic_base: layout::LAPIC_BASE | (1 << 11) | if index == 0 { 1 << 8 } else { 0 },
        pat: 0x0007_0406_0007_0406,
        mtrr: Default::default(),
        rdtscp: false,
        invpcid: false,
    };
    cpu.setup_controls();
    let slot = &m.slots[index];
    let start = {
        let mut start = slot.start.lock().unwrap();
        loop {
            if m.stopped() {
                return Ok(());
            }
            if let Some(s) = start.take() {
                break s;
            }
            start = slot.cond.wait(start).unwrap();
        }
    };
    match start {
        Start::Boot { entry } => cpu.enter_long_mode(entry),
        Start::Sipi { rip } => cpu.enter_real_mode(rip),
    }
    slot.running.store(true, Ordering::SeqCst);
    let r = run(m, &mut cpu);
    slot.running.store(false, Ordering::SeqCst);
    r
}

fn run(m: &Machine, cpu: &mut Vcpu) -> Result<(), String> {
    let mut warned_hole = false;
    loop {
        if m.stopped() {
            return Ok(());
        }
        hv!(hv_vcpu_run_until(cpu.id, hvf::HV_DEADLINE_FOREVER)).map_err(|e| e.to_string())?;
        let reason = (cpu.vmcs(hvf::VMCS_RO_EXIT_REASON) & 0xffff) as u32;
        if reason != hvf::VMX_REASON_EPT_VIOLATION {
            cpu.reinject_vectoring();
        }
        match reason {
            hvf::VMX_REASON_IRQ | hvf::VMX_REASON_IRQ_WND => {
                if cpu.vmcs(hvf::VMCS_GUEST_ACTIVITY_STATE) == ACTIVITY_HLT {
                    idle_until_interrupt(m, cpu);
                }
            }
            hvf::VMX_REASON_EXC_NMI => {}
            hvf::VMX_REASON_TRIPLE_FAULT => {
                eprintln!("orbit-vmm: vcpu{}: triple fault at RIP 0x{:x}", cpu.index, cpu.vmcs(hvf::VMCS_GUEST_RIP));
                m.power_off(EXIT_RESET);
                return Ok(());
            }
            hvf::VMX_REASON_CPUID => {
                emulate_cpuid(m, cpu);
                cpu.advance();
            }
            hvf::VMX_REASON_HLT => {
                cpu.advance();
            }
            hvf::VMX_REASON_VMCALL => {
                cpu.set_reg(hvf::HV_X86_RAX, (-1000i64) as u64); // -KVM_ENOSYS
                cpu.advance();
            }
            hvf::VMX_REASON_MOV_CR => {
                mov_cr(cpu)?;
                cpu.advance();
            }
            hvf::VMX_REASON_IO => {
                if port_io(m, cpu) {
                    return Ok(());
                }
                cpu.advance();
            }
            hvf::VMX_REASON_RDMSR => {
                let msr = cpu.reg(hvf::HV_X86_RCX) as u32;
                let v = rdmsr(cpu, msr);
                cpu.set_reg(hvf::HV_X86_RAX, v & 0xffff_ffff);
                cpu.set_reg(hvf::HV_X86_RDX, v >> 32);
                cpu.advance();
            }
            hvf::VMX_REASON_WRMSR => {
                let msr = cpu.reg(hvf::HV_X86_RCX) as u32;
                let v = (cpu.reg(hvf::HV_X86_RDX) << 32) | (cpu.reg(hvf::HV_X86_RAX) & 0xffff_ffff);
                wrmsr(cpu, msr, v);
                cpu.advance();
            }
            hvf::VMX_REASON_XSETBV => {
                let v = (cpu.reg(hvf::HV_X86_RDX) << 32) | (cpu.reg(hvf::HV_X86_RAX) & 0xffff_ffff);
                if cpu.reg(hvf::HV_X86_RCX) & 0xffff_ffff != 0 || v & 1 == 0 || v & !0x7 != 0 || (v & 4 != 0 && v & 2 == 0) {
                    cpu.inject_exception(13, Some(0));
                } else {
                    cpu.set_reg(hvf::HV_X86_XCR0, v);
                    cpu.advance();
                }
            }
            hvf::VMX_REASON_WBINVD => cpu.advance(),
            hvf::VMX_REASON_MWAIT | hvf::VMX_REASON_MONITOR => cpu.inject_exception(6, None),
            hvf::VMX_REASON_APIC_ACCESS => {
                let offset = cpu.vmcs(hvf::VMCS_RO_EXIT_QUALIFIC) & 0xfff;
                mmio(m, cpu, layout::LAPIC_BASE + offset)?;
            }
            hvf::VMX_REASON_EPT_VIOLATION => {
                let gpa = cpu.vmcs(hvf::VMCS_GUEST_PHYSICAL_ADDRESS);
                if m.mem.ptr(gpa, 1).is_some() {
                    // First touch of a RAM page: the kernel has filled in EPT.
                    cpu.reinject_vectoring();
                    continue;
                }
                cpu.reinject_vectoring();
                if m.bus.find(gpa).is_none() && gpa & !0xfff != layout::LAPIC_BASE && !warned_hole {
                    warned_hole = true;
                    eprintln!("orbit-vmm: vcpu{}: access to unbacked guest address 0x{gpa:x}", cpu.index);
                }
                mmio(m, cpu, gpa)?;
            }
            hvf::VMX_REASON_VMENTRY_GUEST => {
                return Err(format!(
                    "VM entry failed (invalid guest state): RIP 0x{:x} CR0 0x{:x} CR4 0x{:x} EFER 0x{:x} entry 0x{:x} CS AR 0x{:x} TR AR 0x{:x}",
                    cpu.vmcs(hvf::VMCS_GUEST_RIP),
                    cpu.vmcs(hvf::VMCS_GUEST_CR0),
                    cpu.vmcs(hvf::VMCS_GUEST_CR4),
                    cpu.vmcs(hvf::VMCS_GUEST_IA32_EFER),
                    cpu.vmcs(hvf::VMCS_CTRL_VMENTRY_CONTROLS),
                    cpu.vmcs(hvf::VMCS_GUEST_CS_AR),
                    cpu.vmcs(hvf::VMCS_GUEST_TR_AR),
                ))
            }
            other => {
                return Err(format!(
                    "unhandled exit {other} (qualification 0x{:x}) at RIP 0x{:x}",
                    cpu.vmcs(hvf::VMCS_RO_EXIT_QUALIFIC),
                    cpu.vmcs(hvf::VMCS_GUEST_RIP)
                ))
            }
        }
    }
}

fn apic_reg(cpu: &Vcpu, offset: u32) -> u32 {
    let mut v = 0;
    let _ = hv!(hv_vcpu_apic_read(cpu.id, offset, &mut v));
    v
}

/// The guest halted (Linux's idle loop): sleep until an interrupt is pending
/// in this vCPU's local APIC or its timer is due, instead of letting the
/// kernel keep the core in a guest HLT.
fn idle_until_interrupt(m: &Machine, cpu: &Vcpu) {
    let slot = &m.slots[cpu.index];
    // HLT with interrupts off only ends with NMI or INIT: Linux's halt and
    // poweroff end state. Sleep until the machine stops.
    let dead = cpu.vmcs(hvf::VMCS_GUEST_RFLAGS) & RFLAGS_IF == 0;
    slot.dead.store(dead, Ordering::SeqCst);
    if dead {
        while !m.stopped() {
            let seen = m.kicker.generation();
            m.kicker.park(seen, Duration::from_secs(1));
        }
        return;
    }
    // Re-enter at least this often even with no timer armed.
    let start = std::time::Instant::now();
    let mut deadline = start + Duration::from_millis(100);
    if let Some(t) = idle::timer_remaining(
        apic_reg(cpu, idle::APIC_LVT_TIMER),
        apic_reg(cpu, idle::APIC_TIMER_CURRENT),
        apic_reg(cpu, idle::APIC_TIMER_DIVIDE),
        layout::APIC_BUS_HZ,
    ) {
        deadline = deadline.min(start + t);
    }
    loop {
        let seen = m.kicker.generation();
        if m.stopped() || (0..8).any(|i| apic_reg(cpu, idle::APIC_IRR + 0x10 * i) != 0) {
            return;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return;
        }
        m.kicker.park(seen, deadline - now);
    }
}

fn emulate_cpuid(m: &Machine, cpu: &Vcpu) {
    let leaf = cpu.reg(hvf::HV_X86_RAX) as u32;
    let subleaf = cpu.reg(hvf::HV_X86_RCX) as u32;
    let ctx = cpuid::Ctx {
        index: cpu.index as u32,
        cpus: m.slots.len() as u32,
        tsc_hz: m.tsc_hz,
        bus_hz: layout::APIC_BUS_HZ,
        osxsave: cpu.vmcs(hvf::VMCS_CTRL_CR4_SHADOW) & CR4_OSXSAVE != 0,
        rdtscp: cpu.rdtscp,
        invpcid: cpu.invpcid,
    };
    let [a, b, c, d] = cpuid::filter(leaf, subleaf, cpuid::host(leaf, subleaf), &ctx);
    cpu.set_reg(hvf::HV_X86_RAX, a as u64);
    cpu.set_reg(hvf::HV_X86_RBX, b as u64);
    cpu.set_reg(hvf::HV_X86_RCX, c as u64);
    cpu.set_reg(hvf::HV_X86_RDX, d as u64);
}

fn mov_cr(cpu: &Vcpu) -> Result<(), String> {
    let q = cpu.vmcs(hvf::VMCS_RO_EXIT_QUALIFIC);
    let (cr, kind, reg) = (q & 0xf, (q >> 4) & 3, ((q >> 8) & 0xf) as usize);
    match (kind, cr) {
        (0, 0) => cpu.set_cr0(cpu.reg(GPRS[reg])),
        (0, 4) => cpu.set_cr4(cpu.reg(GPRS[reg])),
        (2, _) => cpu.set_cr0(cpu.vmcs(hvf::VMCS_CTRL_CR0_SHADOW) & !(1 << 3)), // CLTS
        (3, _) => {
            // LMSW: low 4 bits of CR0, and it cannot clear PE.
            let old = cpu.vmcs(hvf::VMCS_CTRL_CR0_SHADOW);
            let new = (q >> 16) & 0xf;
            cpu.set_cr0((old & !0xe) | (new & 0xe) | ((old | new) & 1));
        }
        _ => return Err(format!("unexpected control register access 0x{q:x}")),
    }
    Ok(())
}

fn rdmsr(cpu: &Vcpu, msr: u32) -> u64 {
    match msr {
        0x1b => cpu.apic_base,
        0x3a => 1,  // IA32_FEATURE_CONTROL: locked, VMX off
        0x1a0 => 1, // MISC_ENABLE: fast strings
        // The framework owns the VMCS PAT field. Without MTRRs Linux runs with
        // PAT off, so the value only has to read back.
        0x277 => cpu.pat,
        0xfe => 0, // MTRRcap: no variable or fixed ranges
        0x200..=0x2ff => cpu.mtrr.get(&msr).copied().unwrap_or(0),
        0xc000_0080 => cpu.vmcs(hvf::VMCS_GUEST_IA32_EFER),
        _ => 0,
    }
}

fn wrmsr(cpu: &mut Vcpu, msr: u32, v: u64) {
    match msr {
        // Relocating or switching the APIC to x2APIC mode is not supported;
        // CPUID doesn't offer x2APIC, so Linux only writes the value back.
        0x1b => cpu.apic_base = (cpu.apic_base & !0xfff) | (v & 0xfff),
        0x277 => cpu.pat = v,
        0x200..=0x2ff => {
            cpu.mtrr.insert(msr, v);
        }
        0xc000_0080 => cpu.set_efer(v),
        _ => {}
    }
}

/// Port I/O. Returns true when the guest asked for a reset.
fn port_io(m: &Machine, cpu: &Vcpu) -> bool {
    let q = cpu.vmcs(hvf::VMCS_RO_EXIT_QUALIFIC);
    let size = (q & 7) as usize + 1;
    let input = q & 8 != 0;
    let port = (q >> 16) as u16;
    if q & 0x10 != 0 {
        eprintln!("orbit-vmm: vcpu{}: string I/O on port 0x{port:x} ignored", cpu.index);
        return false;
    }
    let rax = cpu.reg(hvf::HV_X86_RAX);
    if input {
        let v: u64 = match port {
            serial::BASE..=0x3ff => m.serial.read(port - serial::BASE) as u64,
            rtc::DATA => m.rtc.read_data() as u64,
            // No 8259 PIC (reading back 0 makes Linux pick its NULL PIC), no
            // i8042 (0 = input buffer empty, so the reset write goes at once).
            0x20 | 0x21 | 0xa0 | 0xa1 | 0x60 | 0x64 => 0,
            _ => !0,
        };
        let merged = match size {
            1 => (rax & !0xff) | (v & 0xff),
            2 => (rax & !0xffff) | (v & 0xffff),
            _ => v & 0xffff_ffff,
        };
        cpu.set_reg(hvf::HV_X86_RAX, merged);
    } else {
        let v = rax as u8;
        match port {
            serial::BASE..=0x3ff => m.serial.write(port - serial::BASE, v),
            rtc::INDEX => m.rtc.write_index(v),
            // i8042 "pulse reset line": reboot=k, and a kernel panic with panic=-1.
            0x64 if v == 0xfe => {
                m.power_off(EXIT_RESET);
                return true;
            }
            _ => {}
        }
    }
    false
}

/// A guest load or store to device memory at `gpa`: decode the instruction,
/// do the access on the device (or the in-kernel local APIC), step over it.
fn mmio(m: &Machine, cpu: &Vcpu, gpa: u64) -> Result<(), String> {
    let code = cpu.fetch(&m.mem)?;
    let insn = decode::decode(&code)
        .map_err(|e| format!("MMIO at 0x{gpa:x} from RIP 0x{:x}: {e}", cpu.vmcs(hvf::VMCS_GUEST_RIP)))?;
    let apic = gpa & !0xfff == layout::LAPIC_BASE;
    match insn.access {
        Access::Load { size, reg, extend } => {
            let mut bytes = [0u8; 8];
            if apic {
                bytes[..4].copy_from_slice(&apic_read(cpu, (gpa & 0xff0) as u32).to_le_bytes());
            } else if let Some((region, offset)) = m.bus.find(gpa) {
                region.device.read(offset, &mut bytes[..size]);
            } else {
                bytes = [0xff; 8];
            }
            let mask = if size == 8 { !0 } else { (1u64 << (8 * size)) - 1 };
            let data = u64::from_le_bytes(bytes) & mask;
            let r = GPRS[reg.index as usize];
            cpu.set_reg(r, decode::merge_load(cpu.reg(r), reg, size, extend, data));
        }
        Access::Store { size, src } => {
            let reg_value = match src {
                decode::Src::Reg(r) => cpu.reg(GPRS[r.index as usize]),
                decode::Src::Imm(_) => 0,
            };
            let value = decode::store_value(src, reg_value);
            if apic {
                if !apic_write(m, cpu, (gpa & 0xff0) as u32, value as u32) {
                    return Ok(()); // fault injected; don't step over the instruction
                }
            } else if let Some((region, offset)) = m.bus.find(gpa) {
                region.device.write(offset, &value.to_le_bytes()[..size]);
            }
        }
    }
    let rip = cpu.vmcs(hvf::VMCS_GUEST_RIP);
    cpu.set_vmcs(hvf::VMCS_GUEST_RIP, rip + insn.len as u64);
    Ok(())
}

fn apic_read(cpu: &Vcpu, offset: u32) -> u32 {
    let mut info = 0u32;
    let mut v = 0u32;
    if hv!(hv_vcpu_exit_info(cpu.id, &mut info)).is_ok()
        && info == hvf::HV_VM_EXITINFO_APIC_ACCESS_READ
        && hv!(hv_vcpu_exit_apic_access_read(cpu.id, &mut v)).is_ok()
    {
        return v;
    }
    let _ = hv!(hv_vcpu_apic_read(cpu.id, offset, &mut v));
    v
}

/// Writes a local APIC register and handles the side effects the kernel hands
/// back: starting APs (INIT/SIPI) and faults. Returns false if a fault was
/// injected instead of completing the write.
fn apic_write(m: &Machine, cpu: &Vcpu, offset: u32, value: u32) -> bool {
    let mut no_side_effect = true;
    if let Err(e) = hv!(hv_vcpu_apic_write(cpu.id, offset, value, &mut no_side_effect)) {
        eprintln!("orbit-vmm: vcpu{}: APIC write 0x{offset:x}: {e}", cpu.index);
        return true;
    }
    if offset == idle::APIC_ICR_LOW {
        m.kicker.kick(); // an IPI: the target may be parked in idle_until_interrupt
    }
    if no_side_effect {
        return true;
    }
    let mut info = 0u32;
    let _ = hv!(hv_vcpu_exit_info(cpu.id, &mut info));
    let n = m.slots.len();
    let max_id = m.ids.lock().unwrap().iter().flatten().max().copied().unwrap_or(0) as usize + 1;
    match info {
        hvf::HV_VM_EXITINFO_INIT_AP => {
            let mut targets = vec![false; max_id];
            let _ = hv!(hv_vcpu_exit_init_ap(cpu.id, targets.as_mut_ptr(), max_id as u32));
        }
        hvf::HV_VM_EXITINFO_STARTUP_AP => {
            let mut targets = vec![false; max_id];
            let mut rip = 0u64;
            if hv!(hv_vcpu_exit_startup_ap(cpu.id, targets.as_mut_ptr(), max_id as u32, &mut rip)).is_ok() {
                for (id, _) in targets.iter().enumerate().filter(|(_, t)| **t) {
                    let Some(i) = m.index_of(id as hv_vcpuid_t).filter(|&i| i < n) else { continue };
                    let slot = &m.slots[i];
                    let mut start = slot.start.lock().unwrap();
                    // The second SIPI of INIT-SIPI-SIPI finds the AP running already.
                    if !slot.running.load(Ordering::SeqCst) && start.is_none() {
                        *start = Some(Start::Sipi { rip });
                        slot.cond.notify_all();
                    }
                }
            }
        }
        hvf::HV_VM_EXITINFO_INJECT_EXCP => {
            let (mut vec, mut valid, mut code, mut restart) = (0u8, false, 0u32, false);
            if hv!(hv_vcpu_exit_inject_excp(cpu.id, &mut vec, &mut valid, &mut code, &mut restart)).is_ok() {
                cpu.inject_exception(vec, valid.then_some(code));
                return !restart;
            }
        }
        _ => {}
    }
    true
}
