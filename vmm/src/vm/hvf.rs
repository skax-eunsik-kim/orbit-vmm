//! Hand-written bindings for the parts of Hypervisor.framework (arm64) we use.
//! Values come from the macOS 26 SDK headers (hv_vcpu_types.h, hv_types.h, ...).
#![allow(non_camel_case_types, dead_code)]

use std::ffi::c_void;

pub type hv_return_t = i32;
pub type hv_vcpu_t = u64;
pub type hv_ipa_t = u64;
pub type os_object = *mut c_void;

pub const HV_SUCCESS: hv_return_t = 0;

pub const HV_MEMORY_READ: u64 = 1 << 0;
pub const HV_MEMORY_WRITE: u64 = 1 << 1;
pub const HV_MEMORY_EXEC: u64 = 1 << 2;

pub const HV_EXIT_REASON_CANCELED: u32 = 0;
pub const HV_EXIT_REASON_EXCEPTION: u32 = 1;
pub const HV_EXIT_REASON_VTIMER_ACTIVATED: u32 = 2;
pub const HV_EXIT_REASON_UNKNOWN: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct hv_vcpu_exit_exception_t {
    pub syndrome: u64,
    pub virtual_address: u64,
    pub physical_address: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct hv_vcpu_exit_t {
    pub reason: u32,
    pub exception: hv_vcpu_exit_exception_t,
}

// hv_reg_t: X0..X30 = 0..30, then PC, FPCR, FPSR, CPSR.
pub const HV_REG_X0: u32 = 0;
pub const HV_REG_PC: u32 = 31;
pub const HV_REG_CPSR: u32 = 34;

// hv_sys_reg_t
pub const HV_SYS_REG_MPIDR_EL1: u16 = 0xc005;
pub const HV_SYS_REG_CNTV_CTL_EL0: u16 = 0xdf19;
pub const HV_SYS_REG_CNTV_CVAL_EL0: u16 = 0xdf1a;

#[link(name = "Hypervisor", kind = "framework")]
extern "C" {
    pub fn hv_vm_create(config: os_object) -> hv_return_t;
    pub fn hv_vm_destroy() -> hv_return_t;
    pub fn hv_vm_map(addr: *mut c_void, ipa: hv_ipa_t, size: usize, flags: u64) -> hv_return_t;
    pub fn hv_vm_unmap(ipa: hv_ipa_t, size: usize) -> hv_return_t;
    pub fn hv_vm_allocate(uvap: *mut *mut c_void, size: usize, flags: u64) -> hv_return_t;
    pub fn hv_vm_deallocate(uva: *mut c_void, size: usize) -> hv_return_t;

    pub fn hv_vcpu_create(vcpu: *mut hv_vcpu_t, exit: *mut *mut hv_vcpu_exit_t, config: os_object) -> hv_return_t;
    pub fn hv_vcpu_destroy(vcpu: hv_vcpu_t) -> hv_return_t;
    pub fn hv_vcpu_run(vcpu: hv_vcpu_t) -> hv_return_t;
    pub fn hv_vcpus_exit(vcpus: *mut hv_vcpu_t, count: u32) -> hv_return_t;
    pub fn hv_vcpu_get_reg(vcpu: hv_vcpu_t, reg: u32, value: *mut u64) -> hv_return_t;
    pub fn hv_vcpu_set_reg(vcpu: hv_vcpu_t, reg: u32, value: u64) -> hv_return_t;
    pub fn hv_vcpu_get_sys_reg(vcpu: hv_vcpu_t, reg: u16, value: *mut u64) -> hv_return_t;
    pub fn hv_vcpu_set_sys_reg(vcpu: hv_vcpu_t, reg: u16, value: u64) -> hv_return_t;
    pub fn hv_vcpu_set_vtimer_mask(vcpu: hv_vcpu_t, masked: bool) -> hv_return_t;
    pub fn hv_vcpu_get_vtimer_offset(vcpu: hv_vcpu_t, offset: *mut u64) -> hv_return_t;

    pub fn hv_gic_config_create() -> os_object;
    pub fn hv_gic_config_set_distributor_base(config: os_object, base: hv_ipa_t) -> hv_return_t;
    pub fn hv_gic_config_set_redistributor_base(config: os_object, base: hv_ipa_t) -> hv_return_t;
    pub fn hv_gic_create(config: os_object) -> hv_return_t;
    pub fn hv_gic_set_spi(intid: u32, level: bool) -> hv_return_t;
    pub fn hv_gic_get_distributor_size(size: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_distributor_base_alignment(align: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_redistributor_size(size: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_redistributor_region_size(size: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_redistributor_base_alignment(align: *mut usize) -> hv_return_t;
    pub fn hv_gic_get_spi_interrupt_range(base: *mut u32, count: *mut u32) -> hv_return_t;
}

pub const HV_ERROR: hv_return_t = 0xfae9_4001_u32 as i32;

// hv_gic_intid_t (uint16_t)
pub const HV_GIC_INT_EL1_VIRTUAL_TIMER: u16 = 27;
pub const HV_GIC_INT_EL1_PHYSICAL_TIMER: u16 = 30;

// hv_gic_redistributor_reg_t (uint32_t)
pub const HV_GIC_REDISTRIBUTOR_REG_GICR_ISENABLER0: u32 = 0x10100;
pub const HV_GIC_REDISTRIBUTOR_REG_GICR_ISPENDR0: u32 = 0x10200;

#[link(name = "Hypervisor", kind = "framework")]
extern "C" {
    pub fn hv_gic_get_intid(interrupt: u16, intid: *mut u32) -> hv_return_t;
    pub fn hv_gic_get_redistributor_reg(vcpu: hv_vcpu_t, reg: u32, value: *mut u64) -> hv_return_t;
}

extern "C" {
    /// libSystem: releases an os_object (e.g. hv_gic_config_t).
    pub fn os_release(object: os_object);
}

#[derive(Debug, Clone, Copy)]
pub struct HvError {
    pub call: &'static str,
    pub code: hv_return_t,
}

impl std::fmt::Display for HvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} failed: 0x{:08x}", self.call, self.code as u32)
    }
}

pub type HvResult<T = ()> = Result<T, HvError>;

/// `hv!(hv_fn(args...))` calls a binding from this module and turns its
/// hv_return_t into an HvResult that names the call.
macro_rules! hv {
    ($f:ident ( $($arg:expr),* $(,)? )) => {{
        let ret = unsafe { $crate::vm::hvf::$f($($arg),*) };
        if ret == $crate::vm::hvf::HV_SUCCESS {
            Ok(())
        } else {
            Err($crate::vm::hvf::HvError { call: stringify!($f), code: ret })
        }
    }};
}
pub(crate) use hv;
