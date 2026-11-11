//! vCPU threads: the hv_vcpu_run loop, MMIO dispatch, PSCI, WFI parking.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::time::Duration;

use super::fdt::mpidr_for;
use super::hvf::{self, hv, hv_vcpu_t};
use crate::devices::MmioBus;

/// Exit counters for ORBIT_DEBUG=stats.
#[derive(Default)]
pub struct Stats {
    pub wfi: AtomicU64,
    pub wfi_requested_us: AtomicU64,
    pub wfi_actual_us: AtomicU64,
    pub wfi_immediate: AtomicU64,
    pub vtimer: AtomicU64,
    pub mmio: AtomicU64,
    pub canceled: AtomicU64,
}

pub static STATS: std::sync::LazyLock<Stats> = std::sync::LazyLock::new(Stats::default);

pub fn spawn_stats_reporter() {
    if std::env::var("ORBIT_DEBUG").as_deref() != Ok("stats") {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(Duration::from_secs(5));
        let s = &*STATS;
        let take = |a: &AtomicU64| a.swap(0, Ordering::Relaxed);
        let (w, rq, ac, imm) =
            (take(&s.wfi), take(&s.wfi_requested_us), take(&s.wfi_actual_us), take(&s.wfi_immediate));
        eprintln!(
            "stats/5s: wfi={w} (immediate {imm}) avg requested={}us actual={}us vtimer={} mmio={} canceled={}",
            if w > imm { rq / (w - imm) } else { 0 },
            if w > imm { ac / (w - imm) } else { 0 },
            take(&s.vtimer),
            take(&s.mmio),
            take(&s.canceled)
        );
    });
}

/// Wakes vCPUs parked in WFI and forces running ones out of the guest so they
/// notice newly pending interrupts.
pub struct Kicker {
    handles: Mutex<Vec<hv_vcpu_t>>,
    generation: AtomicU64,
    lock: Mutex<()>,
    cond: Condvar,
}

impl Kicker {
    pub fn new() -> Self {
        Self {
            handles: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
            lock: Mutex::new(()),
            cond: Condvar::new(),
        }
    }

    fn register(&self, vcpu: hv_vcpu_t) {
        self.handles.lock().unwrap().push(vcpu);
    }

    /// Wakes vCPUs parked in WFI. Running vCPUs get the interrupt straight
    /// from the in-kernel GIC, so forcing them out (hv_vcpus_exit) is only
    /// done when `force` is set (machine stop) — doing it on every device
    /// interrupt costs every vCPU a full exit/re-entry.
    pub fn kick_all(&self) {
        static FORCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        self.kick(*FORCE.get_or_init(|| std::env::var_os("ORBIT_EXIT_KICK").is_some()));
    }

    pub fn kick(&self, force: bool) {
        {
            let _g = self.lock.lock().unwrap();
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.cond.notify_all();
        }
        if force {
            let mut handles = self.handles.lock().unwrap().clone();
            if !handles.is_empty() {
                let _ = hv!(hv_vcpus_exit(handles.as_mut_ptr(), handles.len() as u32));
            }
        }
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Sleep until kicked (generation moves past `seen`) or `timeout` passes.
    fn park(&self, seen: u64, timeout: Duration) {
        let guard = self.lock.lock().unwrap();
        if self.generation() != seen {
            return;
        }
        let _ = self.cond.wait_timeout_while(guard, timeout, |_| self.generation() == seen);
    }
}

/// Per-vCPU power state for PSCI CPU_ON / CPU_OFF.
#[derive(Default)]
struct CpuSlot {
    start: Mutex<Option<(u64, u64)>>, // (entry, context) pending CPU_ON
    on: AtomicBool,
    cond: Condvar,
}

pub struct Machine {
    pub bus: Arc<MmioBus>,
    pub kicker: Arc<Kicker>,
    slots: Vec<CpuSlot>,
    stop: AtomicBool,
    exit_code: AtomicI32,
}

impl Machine {
    pub fn new(bus: Arc<MmioBus>, kicker: Arc<Kicker>, cpus: u32) -> Self {
        Self {
            bus,
            kicker,
            slots: (0..cpus).map(|_| CpuSlot::default()).collect(),
            stop: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
        }
    }

    pub fn power_off(&self, code: i32) {
        self.exit_code.store(code, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        for slot in &self.slots {
            let _g = slot.start.lock().unwrap();
            slot.cond.notify_all();
        }
        self.kicker.kick(true);
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// Start all vCPU threads; vCPU 0 boots the kernel. Returns the exit code
/// once the guest powers off.
pub fn run_all(machine: Arc<Machine>, entry: u64, dtb: u64) -> i32 {
    spawn_stats_reporter();
    let cpus = machine.slots.len();
    // Every vCPU must exist before any runs: the GIC topology is fixed at first run.
    let created = Arc::new(Barrier::new(cpus));
    *machine.slots[0].start.lock().unwrap() = Some((entry, dtb));
    let threads: Vec<_> = (0..cpus)
        .map(|i| {
            let m = machine.clone();
            let created = created.clone();
            std::thread::Builder::new()
                .name(format!("vcpu{i}"))
                .spawn(move || {
                    if let Err(e) = vcpu_thread(&m, i, &created) {
                        eprintln!("orbit-vmm: vcpu{i}: {e}");
                        m.power_off(1);
                    }
                })
                .expect("spawn vcpu thread")
        })
        .collect();
    for t in threads {
        let _ = t.join();
    }
    machine.exit_code.load(Ordering::SeqCst)
}

struct Vcpu {
    id: hv_vcpu_t,
    exit: *mut hvf::hv_vcpu_exit_t,
}

impl Vcpu {
    fn reg(&self, r: u32) -> u64 {
        let mut v = 0;
        let _ = hv!(hv_vcpu_get_reg(self.id, r, &mut v));
        v
    }
    fn set_reg(&self, r: u32, v: u64) {
        let _ = hv!(hv_vcpu_set_reg(self.id, r, v));
    }
    fn sys_reg(&self, r: u16) -> u64 {
        let mut v = 0;
        let _ = hv!(hv_vcpu_get_sys_reg(self.id, r, &mut v));
        v
    }
    /// Xn, where register 31 reads as zero (XZR) in these encodings.
    fn x(&self, n: u32) -> u64 {
        if n == 31 {
            0
        } else {
            self.reg(hvf::HV_REG_X0 + n)
        }
    }
    fn set_x(&self, n: u32, v: u64) {
        if n != 31 {
            self.set_reg(hvf::HV_REG_X0 + n, v);
        }
    }
    fn advance_pc(&self) {
        let pc = self.reg(hvf::HV_REG_PC);
        self.set_reg(hvf::HV_REG_PC, pc + 4);
    }
}

fn vcpu_thread(m: &Machine, index: usize, created: &Barrier) -> Result<(), String> {
    // With the in-kernel GIC, guest WFI is handled inside hv_vcpu_run: the
    // thread blocks in the kernel until the guest timer or an interrupt. Default
    // timer slack made a 20 µs guest sleep take ~250 µs, so ask for precise wakeups.
    set_low_latency_thread();
    let mut id: hv_vcpu_t = 0;
    let mut exit: *mut hvf::hv_vcpu_exit_t = std::ptr::null_mut();
    let r = hv!(hv_vcpu_create(&mut id, &mut exit, std::ptr::null_mut()));
    let cpu = Vcpu { id, exit };
    if r.is_ok() {
        let _ = hv!(hv_vcpu_set_sys_reg(id, hvf::HV_SYS_REG_MPIDR_EL1, mpidr_for(index as u32)));
        m.kicker.register(id);
    }
    created.wait();
    r.map_err(|e| e.to_string())?;

    let slot = &m.slots[index];
    loop {
        // Wait for PSCI CPU_ON (vCPU 0 is pre-armed with the kernel entry).
        let (entry, context) = {
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
        slot.on.store(true, Ordering::SeqCst);
        cpu.set_reg(hvf::HV_REG_PC, entry);
        cpu.set_reg(hvf::HV_REG_X0, context);
        for n in 1..4 {
            cpu.set_x(n, 0);
        }
        cpu.set_reg(hvf::HV_REG_CPSR, 0x3c5); // EL1h, DAIF masked

        let off = run_until_off(m, &cpu)?;
        slot.on.store(false, Ordering::SeqCst);
        if !off || m.stopped() {
            return Ok(());
        }
    }
}

/// Runs the guest on this vCPU. Ok(true) means PSCI CPU_OFF, Ok(false) machine stop.
fn run_until_off(m: &Machine, cpu: &Vcpu) -> Result<bool, String> {
    let mut warned_vtimer = false;
    loop {
        if m.stopped() {
            return Ok(false);
        }
        let seen = m.kicker.generation();
        hv!(hv_vcpu_run(cpu.id)).map_err(|e| e.to_string())?;
        let exit = unsafe { *cpu.exit };
        match exit.reason {
            hvf::HV_EXIT_REASON_CANCELED => {
                STATS.canceled.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            hvf::HV_EXIT_REASON_VTIMER_ACTIVATED => {
                STATS.vtimer.fetch_add(1, Ordering::Relaxed);
                // With the in-kernel GIC the framework delivers the timer PPI
                // itself; just unmask so we keep getting guest time.
                if !warned_vtimer {
                    warned_vtimer = true;
                }
                let _ = hv!(hv_vcpu_set_vtimer_mask(cpu.id, false));
            }
            hvf::HV_EXIT_REASON_EXCEPTION => {
                let esr = exit.exception.syndrome;
                let ec = (esr >> 26) & 0x3f;
                match ec {
                    0x01 => {
                        cpu.advance_pc();
                        wait_for_interrupt(m, cpu, seen);
                    }
                    0x16 => {
                        // HVC: PC already points past the instruction.
                        if let Some(off) = psci(m, cpu) {
                            return Ok(off);
                        }
                    }
                    0x17 => {
                        cpu.advance_pc();
                        if let Some(off) = psci(m, cpu) {
                            return Ok(off);
                        }
                    }
                    0x18 => {
                        // MSR/MRS to an untrapped-by-hardware sysreg: RAZ/WI.
                        let rt = ((esr >> 5) & 0x1f) as u32;
                        if esr & 1 == 1 {
                            cpu.set_x(rt, 0);
                        }
                        cpu.advance_pc();
                    }
                    0x24 => {
                        STATS.mmio.fetch_add(1, Ordering::Relaxed);
                        mmio(m, cpu, esr, exit.exception.physical_address)?;
                        cpu.advance_pc();
                    }
                    _ => {
                        return Err(format!(
                            "unhandled exception EC=0x{ec:x} ESR=0x{esr:x} PC=0x{:x} FAR=0x{:x}",
                            cpu.reg(hvf::HV_REG_PC),
                            exit.exception.virtual_address
                        ))
                    }
                }
            }
            other => return Err(format!("unexpected exit reason {other}")),
        }
    }
}

/// Emulates a trapped load/store to device memory (data abort with ISV=1).
fn mmio(m: &Machine, cpu: &Vcpu, esr: u64, gpa: u64) -> Result<(), String> {
    let iss = esr & 0x1ff_ffff;
    if iss & (1 << 24) == 0 {
        return Err(format!("MMIO abort without syndrome at 0x{gpa:x} (PC 0x{:x})", cpu.reg(hvf::HV_REG_PC)));
    }
    let size = 1usize << ((iss >> 22) & 3);
    let sign_extend = iss & (1 << 21) != 0;
    let rt = ((iss >> 16) & 0x1f) as u32;
    let sixty_four = iss & (1 << 15) != 0;
    let is_write = iss & (1 << 6) != 0;

    let target = m.bus.find(gpa);
    if is_write {
        let bytes = cpu.x(rt).to_le_bytes();
        if let Some((region, offset)) = target {
            region.device.write(offset, &bytes[..size]);
        }
    } else {
        let mut bytes = [0u8; 8];
        if let Some((region, offset)) = target {
            region.device.read(offset, &mut bytes[..size]);
        }
        let mut value = u64::from_le_bytes(bytes);
        if sign_extend && size < 8 {
            let shift = 64 - 8 * size as u32;
            value = (((value << shift) as i64) >> shift) as u64;
        }
        if !sixty_four {
            value &= 0xffff_ffff;
        }
        cpu.set_x(rt, value);
    }
    Ok(())
}

/// WFI: park the thread until a device interrupt arrives or the guest's
/// virtual timer is due, instead of spinning a host core.
///
/// Device interrupts (SPIs) wake us through the Kicker, but IPIs (SGIs) sent
/// by other vCPUs go straight into the in-kernel GIC with no notification, so
/// we also poll this vCPU's redistributor for pending SGIs/PPIs, starting
/// with short slices (busy guests re-enter WFI constantly and need IPIs fast)
/// and backing off to 1 ms when idle. Without this, cross-CPU wakeups (RCU,
/// scheduler, TLB shootdowns) waited for the next timer tick.
#[allow(deprecated)] // libc's mach_* bindings; mach2 is a whole crate for two calls
fn wait_for_interrupt(m: &Machine, cpu: &Vcpu, seen: u64) {
    let ctl = cpu.sys_reg(hvf::HV_SYS_REG_CNTV_CTL_EL0);
    let max = Duration::from_millis(100);
    let total = if ctl & 1 == 1 && ctl & 2 == 0 {
        let cval = cpu.sys_reg(hvf::HV_SYS_REG_CNTV_CVAL_EL0);
        let mut offset = 0;
        let _ = hv!(hv_vcpu_get_vtimer_offset(cpu.id, &mut offset));
        let now = unsafe { libc::mach_absolute_time() }.wrapping_sub(offset);
        if cval <= now {
            STATS.wfi.fetch_add(1, Ordering::Relaxed);
            STATS.wfi_immediate.fetch_add(1, Ordering::Relaxed);
            return;
        }
        ticks_to_duration(cval - now).min(max)
    } else {
        max
    };
    // Waking from a condvar sleep on macOS costs ~250 µs, which made every
    // short guest sleep (Go runtimes do lots of 20 µs ones) 10x too long. So
    // sleep only until SPIN_WINDOW before the deadline, then spin the rest.
    const SPIN_WINDOW: Duration = Duration::from_micros(300);
    let start = std::time::Instant::now();
    STATS.wfi.fetch_add(1, Ordering::Relaxed);
    STATS.wfi_requested_us.fetch_add(total.as_micros() as u64, Ordering::Relaxed);
    let _record = Record(start);
    let mut slice = Duration::from_micros(50);
    loop {
        if sgi_or_ppi_pending(cpu) || m.kicker.generation() != seen {
            return;
        }
        let elapsed = start.elapsed();
        if elapsed >= total {
            return;
        }
        let left = total - elapsed;
        if left <= SPIN_WINDOW {
            std::hint::spin_loop();
            continue;
        }
        m.kicker.park(seen, slice.min(left - SPIN_WINDOW));
        slice = (slice * 2).min(Duration::from_millis(1));
    }
}

/// Time-constraint (real-time) policy for vCPU threads, which gives their
/// kernel timers minimal leeway. ORBIT_VCPU_POLICY=qos uses interactive QoS
/// instead, =none leaves the default (for comparisons).
fn set_low_latency_thread() {
    #[repr(C)]
    struct TimeConstraint {
        period: u32,
        computation: u32,
        constraint: u32,
        preemptible: u32,
    }
    extern "C" {
        fn mach_thread_self() -> u32;
        fn thread_policy_set(thread: u32, flavor: u32, policy: *const TimeConstraint, count: u32) -> i32;
    }
    const THREAD_TIME_CONSTRAINT_POLICY: u32 = 2;
    match std::env::var("ORBIT_VCPU_POLICY").as_deref() {
        Ok("none") => {}
        Ok("qos") => unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0);
        },
        _ => {
            // In mach absolute time units (24 MHz on Apple Silicon): 1 ms period,
            // up to 0.5 ms of work, preemptible so a busy vCPU can't hog a core.
            let ms = 24_000;
            let policy = TimeConstraint { period: ms, computation: ms / 2, constraint: ms, preemptible: 1 };
            let rc = unsafe { thread_policy_set(mach_thread_self(), THREAD_TIME_CONSTRAINT_POLICY, &policy, 4) };
            if rc != 0 {
                eprintln!("orbit-vmm: time-constraint policy not applied ({rc})");
            }
        }
    }
}

struct Record(std::time::Instant);
impl Drop for Record {
    fn drop(&mut self) {
        STATS.wfi_actual_us.fetch_add(self.0.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
}

fn sgi_or_ppi_pending(cpu: &Vcpu) -> bool {
    let (mut pending, mut enabled) = (0u64, 0u64);
    let ok = hv!(hv_gic_get_redistributor_reg(cpu.id, hvf::HV_GIC_REDISTRIBUTOR_REG_GICR_ISPENDR0, &mut pending))
        .is_ok()
        && hv!(hv_gic_get_redistributor_reg(cpu.id, hvf::HV_GIC_REDISTRIBUTOR_REG_GICR_ISENABLER0, &mut enabled))
            .is_ok();
    ok && pending & enabled != 0
}

#[allow(deprecated)]
fn ticks_to_duration(ticks: u64) -> Duration {
    static TIMEBASE: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
    let (numer, denom) = *TIMEBASE.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        unsafe { libc::mach_timebase_info(&mut info) };
        (info.numer, info.denom)
    });
    Duration::from_nanos((ticks as u128 * numer as u128 / denom as u128) as u64)
}

const PSCI_VERSION: u64 = 0x8400_0000;
const PSCI_CPU_SUSPEND_64: u64 = 0xc400_0001;
const PSCI_CPU_OFF: u64 = 0x8400_0002;
const PSCI_CPU_ON_64: u64 = 0xc400_0003;
const PSCI_AFFINITY_INFO_64: u64 = 0xc400_0004;
const PSCI_MIGRATE_INFO_TYPE: u64 = 0x8400_0006;
/// Process exit status when the guest requests a reset.
pub const EXIT_RESET: i32 = 3;
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
const PSCI_FEATURES: u64 = 0x8400_000a;
const PSCI_NOT_SUPPORTED: u64 = -1i64 as u64;
const PSCI_INVALID_PARAMS: u64 = -2i64 as u64;
const PSCI_ALREADY_ON: u64 = -4i64 as u64;

/// Handles an SMCCC call. Some(true) = this vCPU turned off, Some(false) = machine stop.
fn psci(m: &Machine, cpu: &Vcpu) -> Option<bool> {
    let func = cpu.x(0) & 0xffff_ffff;
    let ret = match func {
        PSCI_VERSION => 0x0001_0000, // 1.0
        PSCI_MIGRATE_INFO_TYPE => 2, // no trusted OS
        PSCI_FEATURES => match cpu.x(1) & 0xffff_ffff {
            PSCI_VERSION
            | PSCI_CPU_OFF
            | PSCI_CPU_ON_64
            | PSCI_AFFINITY_INFO_64
            | PSCI_SYSTEM_OFF
            | PSCI_SYSTEM_RESET
            | PSCI_FEATURES
            | PSCI_MIGRATE_INFO_TYPE => 0,
            _ => PSCI_NOT_SUPPORTED,
        },
        PSCI_CPU_SUSPEND_64 => 0,
        PSCI_CPU_ON_64 => {
            let target = cpu.x(1);
            match m.slots.iter().enumerate().find(|(i, _)| mpidr_for(*i as u32) == target & 0xff_ffff) {
                None => PSCI_INVALID_PARAMS,
                Some((_, slot)) if slot.on.load(Ordering::SeqCst) => PSCI_ALREADY_ON,
                Some((_, slot)) => {
                    *slot.start.lock().unwrap() = Some((cpu.x(2), cpu.x(3)));
                    slot.on.store(true, Ordering::SeqCst);
                    slot.cond.notify_all();
                    0
                }
            }
        }
        PSCI_AFFINITY_INFO_64 => {
            let target = cpu.x(1);
            match m.slots.iter().enumerate().find(|(i, _)| mpidr_for(*i as u32) == target & 0xff_ffff) {
                None => PSCI_INVALID_PARAMS,
                Some((_, slot)) => u64::from(!slot.on.load(Ordering::SeqCst)), // 0 = ON, 1 = OFF
            }
        }
        PSCI_CPU_OFF => return Some(true),
        // Exit status: 0 = the guest powered off, EXIT_RESET = it asked to reboot
        // (`reboot`, or a kernel panic with panic=-1); the watchdog restarts it.
        PSCI_SYSTEM_OFF => {
            m.power_off(0);
            return Some(false);
        }
        PSCI_SYSTEM_RESET => {
            m.power_off(EXIT_RESET);
            return Some(false);
        }
        _ => PSCI_NOT_SUPPORTED,
    };
    cpu.set_x(0, ret);
    None
}
