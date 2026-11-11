//! Just enough of an x86-64 instruction decoder to emulate a guest access to
//! device memory. VMX reports the guest-physical address and whether the
//! access was a read or a write, but not the register, the value or the
//! instruction length, so the vCPU loop decodes the instruction at RIP.
//!
//! Linux reaches MMIO only through readb/w/l/q and writeb/w/l/q, which compile
//! to plain `mov`/`movzx` (and `movsx` in odd drivers). Anything else is
//! reported as unsupported rather than guessed at.

/// What the instruction does with the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Read `size` bytes into general register `reg` (0 = RAX ... 15 = R15).
    Load { size: usize, reg: Reg, extend: Extend },
    /// Write `size` bytes taken from `src`.
    Store { size: usize, src: Src },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reg {
    pub index: u8,
    /// AH/CH/DH/BH: bits 8..16 of register `index` (0..3).
    pub high8: bool,
    /// Width written to the register, in bytes. 4 zero-extends to 64 bits,
    /// 1 and 2 leave the rest of the register alone (x86 rules).
    pub width: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Extend {
    None,
    Zero,
    Sign,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    Reg(Reg),
    Imm(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Insn {
    pub len: usize,
    pub access: Access,
}

/// Decodes the instruction in `code` (up to 15 bytes, 64-bit mode).
pub fn decode(code: &[u8]) -> Result<Insn, String> {
    let at = |i: usize| code.get(i).copied().ok_or_else(|| format!("truncated instruction {code:02x?}"));
    let mut i = 0;
    let mut opsize16 = false;
    loop {
        match at(i)? {
            0x66 => opsize16 = true,
            0x67 | 0xf0 | 0xf2 | 0xf3 | 0x2e | 0x36 | 0x3e | 0x26 | 0x64 | 0x65 => {}
            _ => break,
        }
        i += 1;
    }
    let mut rex = 0u8;
    if at(i)? & 0xf0 == 0x40 {
        rex = at(i)?;
        i += 1;
    }
    let (w, r) = (rex & 8 != 0, rex & 4 != 0);
    let osize = if w {
        8
    } else if opsize16 {
        2
    } else {
        4
    };
    let reg_of = |modrm: u8, width: usize| {
        let n = (modrm >> 3) & 7;
        if width == 1 && rex == 0 && n >= 4 {
            Reg { index: n - 4, high8: true, width }
        } else {
            Reg { index: n | if r { 8 } else { 0 }, high8: false, width }
        }
    };
    let op = at(i)?;
    i += 1;
    let (access, modrm_at, imm_len) = match op {
        0x88..=0x8b => {
            let modrm = at(i)?;
            let size = if op & 1 == 0 { 1 } else { osize };
            let reg = reg_of(modrm, size);
            let access = if op <= 0x89 {
                Access::Store { size, src: Src::Reg(reg) }
            } else {
                Access::Load { size, reg, extend: Extend::None }
            };
            (access, i, 0)
        }
        0xc6 | 0xc7 => {
            let modrm = at(i)?;
            if (modrm >> 3) & 7 != 0 {
                return Err(format!("unsupported opcode {op:02x} /{}", (modrm >> 3) & 7));
            }
            let size = if op == 0xc6 { 1 } else { osize };
            // Immediate filled in below, once the ModRM length is known.
            (Access::Store { size, src: Src::Imm(0) }, i, size.min(4))
        }
        0x0f => {
            let op2 = at(i)?;
            i += 1;
            let modrm = at(i)?;
            let (size, extend) = match op2 {
                0xb6 => (1, Extend::Zero),
                0xb7 => (2, Extend::Zero),
                0xbe => (1, Extend::Sign),
                0xbf => (2, Extend::Sign),
                _ => return Err(format!("unsupported opcode 0f {op2:02x}")),
            };
            (Access::Load { size, reg: reg_of(modrm, osize), extend }, i, 0)
        }
        _ => return Err(format!("unsupported opcode {op:02x} in {code:02x?}")),
    };
    let modrm = at(modrm_at)?;
    let (md, rm) = (modrm >> 6, modrm & 7);
    if md == 3 {
        return Err("register operand where memory was expected".into());
    }
    let mut len = modrm_at + 1;
    let mut base = rm;
    if rm == 4 {
        base = at(len)? & 7;
        len += 1;
    }
    len += match md {
        0 if rm == 5 || base == 5 => 4,
        1 => 1,
        2 => 4,
        _ => 0,
    };
    let access = match access {
        Access::Store { size, src: Src::Imm(_) } => {
            let mut imm = 0u64;
            for k in 0..imm_len {
                imm |= (at(len + k)? as u64) << (8 * k);
            }
            if size == 8 {
                imm = imm as u32 as i32 as i64 as u64; // imm32 sign-extends
            }
            len += imm_len;
            Access::Store { size, src: Src::Imm(imm) }
        }
        a => a,
    };
    Ok(Insn { len, access })
}

/// The new value of a 64-bit register after a load of `data` (already
/// truncated to the access size) into `reg`, following x86 partial-register
/// rules.
pub fn merge_load(old: u64, reg: Reg, size: usize, extend: Extend, data: u64) -> u64 {
    let value = match extend {
        Extend::Sign => {
            let shift = 64 - 8 * size as u32;
            ((data << shift) as i64 >> shift) as u64
        }
        Extend::None | Extend::Zero => data,
    };
    match (reg.width, reg.high8) {
        (1, true) => (old & !0xff00) | ((value & 0xff) << 8),
        (1, false) => (old & !0xff) | (value & 0xff),
        (2, _) => (old & !0xffff) | (value & 0xffff),
        (4, _) => value & 0xffff_ffff,
        _ => value,
    }
}

/// The bytes a store writes, given the source register's value.
pub fn store_value(src: Src, reg_value: u64) -> u64 {
    match src {
        Src::Imm(v) => v,
        Src::Reg(r) if r.high8 => reg_value >> 8,
        Src::Reg(_) => reg_value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg(index: u8, width: usize) -> Reg {
        Reg { index, high8: false, width }
    }

    #[test]
    fn decodes_the_mmio_accessors_linux_emits() {
        let cases: &[(&[u8], Insn)] = &[
            // mov %eax,(%rdx)           writel
            (&[0x89, 0x02], Insn { len: 2, access: Access::Store { size: 4, src: Src::Reg(reg(0, 4)) } }),
            // mov 0x30(%rax),%eax       readl with disp8
            (
                &[0x8b, 0x40, 0x30],
                Insn { len: 3, access: Access::Load { size: 4, reg: reg(0, 4), extend: Extend::None } },
            ),
            // mov %r12d,0xb0(%r13)      REX.R+B, disp32
            (
                &[0x45, 0x89, 0xa5, 0xb0, 0, 0, 0],
                Insn { len: 7, access: Access::Store { size: 4, src: Src::Reg(reg(12, 4)) } },
            ),
            // movzbl (%rdi),%eax        readb
            (
                &[0x0f, 0xb6, 0x07],
                Insn { len: 3, access: Access::Load { size: 1, reg: reg(0, 4), extend: Extend::Zero } },
            ),
            // movzwl 0x4(%rsp,%rbx,1),%ecx   SIB + disp8
            (
                &[0x0f, 0xb7, 0x4c, 0x1c, 0x04],
                Insn { len: 5, access: Access::Load { size: 2, reg: reg(1, 4), extend: Extend::Zero } },
            ),
            // mov %dx,(%rax)            writew (0x66)
            (&[0x66, 0x89, 0x10], Insn { len: 3, access: Access::Store { size: 2, src: Src::Reg(reg(2, 2)) } }),
            // mov %rax,(%rdx)           writeq
            (&[0x48, 0x89, 0x02], Insn { len: 3, access: Access::Store { size: 8, src: Src::Reg(reg(0, 8)) } }),
            // movl $0x0,0xb0(%rax)      store imm32
            (
                &[0xc7, 0x80, 0xb0, 0, 0, 0, 0, 0, 0, 0],
                Insn { len: 10, access: Access::Store { size: 4, src: Src::Imm(0) } },
            ),
            // movb $0x7,(%rax)
            (&[0xc6, 0x00, 0x07], Insn { len: 3, access: Access::Store { size: 1, src: Src::Imm(7) } }),
            // mov %ah,(%rcx)            high byte register without REX
            (
                &[0x88, 0x21],
                Insn {
                    len: 2,
                    access: Access::Store { size: 1, src: Src::Reg(Reg { index: 0, high8: true, width: 1 }) },
                },
            ),
            // mov %sil,(%rcx)           REX makes 6 = SIL, not DH
            (&[0x40, 0x88, 0x31], Insn { len: 3, access: Access::Store { size: 1, src: Src::Reg(reg(6, 1)) } }),
            // mov 0x1000(%rip),%eax     RIP-relative
            (
                &[0x8b, 0x05, 0, 0x10, 0, 0],
                Insn { len: 6, access: Access::Load { size: 4, reg: reg(0, 4), extend: Extend::None } },
            ),
        ];
        for (code, want) in cases {
            assert_eq!(decode(code).as_ref(), Ok(want), "{code:02x?}");
        }
        assert!(decode(&[0x87, 0x02]).is_err(), "xchg is not emulated");
        assert!(decode(&[0x89, 0xc0]).is_err(), "register-to-register mov is not an MMIO access");
        assert!(decode(&[0x8b]).is_err(), "truncated");
    }

    #[test]
    fn loads_follow_partial_register_rules() {
        let old = 0x1122_3344_5566_7788;
        assert_eq!(merge_load(old, reg(0, 4), 4, Extend::None, 0xaabb_ccdd), 0xaabb_ccdd);
        assert_eq!(merge_load(old, reg(0, 2), 2, Extend::None, 0xbeef), 0x1122_3344_5566_beef);
        assert_eq!(merge_load(old, reg(0, 1), 1, Extend::None, 0xef), 0x1122_3344_5566_77ef);
        let ah = Reg { index: 0, high8: true, width: 1 };
        assert_eq!(merge_load(old, ah, 1, Extend::None, 0xef), 0x1122_3344_5566_ef88);
        assert_eq!(merge_load(old, reg(0, 4), 1, Extend::Sign, 0x80), 0xffff_ff80);
        assert_eq!(merge_load(old, reg(0, 8), 2, Extend::Sign, 0x8000), 0xffff_ffff_ffff_8000);
        assert_eq!(store_value(Src::Reg(ah), 0x1234), 0x12);
    }
}
