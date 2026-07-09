//! Flag handling: pending (for JIT direct writes) and legacy lazy flags.
//! Extracted for compartmentalization.

use izarravm_bus::BusWidth;

pub(crate) const FLAG_CF: u32 = 0x0000_0001;
pub(crate) const FLAG_PF: u32 = 0x0000_0004;
pub(crate) const FLAG_AF: u32 = 0x0000_0010;
pub(crate) const FLAG_ZF: u32 = 0x0000_0040;
pub(crate) const FLAG_SF: u32 = 0x0000_0080;
pub(crate) const FLAG_TF: u32 = 0x0000_0100;
pub(crate) const FLAG_IF: u32 = 0x0000_0200;
pub(crate) const FLAG_DF: u32 = 0x0000_0400;
pub(crate) const FLAG_OF: u32 = 0x0000_0800;
pub(crate) const FLAG_NT: u32 = 0x0000_4000; // bit 14, nested task
pub(crate) const FLAG_IOPL: u32 = 0x0000_3000; // bits 12-13, I/O privilege level
pub(crate) const FLAG_VM: u32 = 0x0002_0000; // bit 17, virtual-8086 mode

// 486 EFLAGS additions. AC (bit 18) is the alignment-check enable consulted by the
// #AC path together with CR0.AM; ID (bit 21) is the toggleable bit software flips to
// probe for CPUID. Both are plain read/write storage otherwise, and both survive a
// PUSHFD/POPFD round-trip (the dword flag image carries them).
pub(crate) const FLAG_AC: u32 = 0x0004_0000; // bit 18
pub(crate) const FLAG_ID: u32 = 0x0020_0000; // bit 21

/// A deferred ADD or SUB whose six arithmetic flags (CF/PF/AF/ZF/SF/OF) have not been computed yet.
/// `a`/`b` are the width-masked operands, `result` the width-masked result, exactly as `alu_add`/
/// `alu_sub` computed them. While this is `Some`, the six arithmetic-flag bits in `registers.eflags`
/// are STALE; this descriptor is the source of truth for them. Control flags in `eflags` stay live.
/// Carry-free ADD/SUB/CMP, CF-preserving INC/DEC, and logical ops are representable:
/// `b` is the raw second operand (NOT b+carry / b+borrow), so ADC/SBB with a
/// non-zero carry/borrow stay eager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LazyFlagOp {
    Add,
    Sub,
    Logic,
}

/// Packed pending flags for native code emission (v2 inlining).
/// (tag & (1<<31)) == 0 means none.
/// Layout is #[repr(C)] so emitted x86-64 can write it directly from scratch regs.
/// Packing (little-endian tag):
///   bits  0-7 : op (0=Add, 1=Sub, 2=Logic)
///   bits  8-15: width (0=Byte, 1=Word, 2=Dword as BusWidth discriminant)
///   bit  16   : has_cf_override
///   bit  17   : cf value (if has)
///   bit  31   : has-pending (set for any valid pending)
/// a/b/result are the raw operands and result (masked by caller as before).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PendingFlags {
    pub tag: u32,
    pub a: u32,
    pub b: u32,
    pub result: u32,
}

impl PendingFlags {
    #[inline]
    pub fn is_none(&self) -> bool {
        (self.tag & (1u32 << 31)) == 0
    }

    #[inline]
    pub(crate) fn op(&self) -> LazyFlagOp {
        match self.tag & 0xff {
            0 => LazyFlagOp::Add,
            1 => LazyFlagOp::Sub,
            _ => LazyFlagOp::Logic,
        }
    }

    #[inline]
    pub fn width(&self) -> BusWidth {
        match (self.tag >> 8) & 0xff {
            0 => BusWidth::Byte,
            1 => BusWidth::Word,
            _ => BusWidth::Dword,
        }
    }

    #[inline]
    pub fn cf_override(&self) -> Option<bool> {
        if (self.tag & (1 << 16)) != 0 {
            Some((self.tag & (1 << 17)) != 0)
        } else {
            None
        }
    }

    /// Return a copy with the CF override set (for special-case set_flag on CF while pending).
    pub fn with_cf_override(self, cf: bool) -> Self {
        let mut tag = self.tag;
        tag |= 1 << 16;
        if cf {
            tag |= 1 << 17;
        } else {
            tag &= !(1 << 17);
        }
        Self { tag, ..self }
    }

    /// Pack a legacy LazyFlags into the C form used by the emitter.
    /// This is the bridge during v2 migration.
    pub(crate) fn from_legacy(l: &LazyFlags) -> Self {
        let op = match l.op {
            LazyFlagOp::Add => 0u32,
            LazyFlagOp::Sub => 1,
            LazyFlagOp::Logic => 2,
        };
        let w = match l.width {
            BusWidth::Byte => 0,
            BusWidth::Word => 1,
            BusWidth::Dword => 2,
        };
        let mut tag = op | (w << 8);
        if let Some(cf) = l.cf_override {
            tag |= 1 << 16;
            if cf {
                tag |= 1 << 17;
            }
        }
        tag |= 1u32 << 31; // mark has-pending
        Self {
            tag,
            a: l.a,
            b: l.b,
            result: l.result,
        }
    }
}

/// Legacy wrapper for the short term (will be removed once all sites use PendingFlags directly).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LazyFlags {
    pub(crate) a: u32,
    pub(crate) b: u32,
    pub(crate) result: u32,
    pub(crate) width: BusWidth,
    pub(crate) op: LazyFlagOp,
    pub(crate) cf_override: Option<bool>,
}
