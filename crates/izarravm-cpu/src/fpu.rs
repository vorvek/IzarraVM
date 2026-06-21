//! x87 FPU state for the 387-class core: the register stack, the control and
//! status words, and the tag word.
//!
//! ponytail: registers hold f64, not the hardware's 80-bit extended format. The
//! Izarra's games-first target (Wolfenstein 3D-era DOS) never needs 80-bit
//! exactness, so the ceiling is that transcendental and denormal edge cases are
//! not bit-accurate. Upgrade path if a title ever needs it: a soft float80.

/// Control word after FINIT: all exceptions masked, round-to-nearest, 64-bit
/// precision (0x037F is the 387 reset value).
const CONTROL_INIT: u16 = 0x037f;

/// Status-word bit positions for the condition codes and TOP field.
const C0: u16 = 1 << 8;
const C1: u16 = 1 << 9;
const C2: u16 = 1 << 10;
const C3: u16 = 1 << 14;
const TOP_SHIFT: u16 = 11;
const TOP_MASK: u16 = 0x7 << TOP_SHIFT;

#[derive(Debug, Clone)]
pub struct X87 {
    /// The eight physical registers. The architectural ST(i) maps to
    /// `st[(top + i) mod 8]`.
    st: [f64; 8],
    pub control: u16,
    pub status: u16,
    /// Two bits per physical register: 00 valid, 01 zero, 10 special, 11 empty.
    pub tag: u16,
    /// MMX registers MM0-7. Real silicon aliases these onto the x87 mantissas; we
    /// keep them separate and model the visible tag effect (see mmx.rs).
    mm: [u64; 8],
}

// Bit-wise equality so the containing Cpu386 can keep deriving Eq despite the
// f64 register file (f64 is not Eq because of NaN; comparing raw bits is).
impl PartialEq for X87 {
    fn eq(&self, other: &Self) -> bool {
        self.control == other.control
            && self.status == other.status
            && self.tag == other.tag
            && self.mm == other.mm
            && self
                .st
                .iter()
                .zip(other.st.iter())
                .all(|(a, b)| a.to_bits() == b.to_bits())
    }
}

impl Eq for X87 {}

impl Default for X87 {
    fn default() -> Self {
        let mut fpu = X87 {
            st: [0.0; 8],
            control: 0,
            status: 0,
            tag: 0,
            mm: [0; 8],
        };
        fpu.finit();
        fpu
    }
}

impl X87 {
    /// FNINIT / FINIT: reset to the documented power-on state.
    pub fn finit(&mut self) {
        self.st = [0.0; 8];
        self.control = CONTROL_INIT;
        self.status = 0;
        self.tag = 0xffff; // every register empty
    }

    /// FNCLEX: clear the exception flags and the busy bit, leave TOP and the
    /// condition codes alone.
    pub fn clear_exceptions(&mut self) {
        self.status &= !0x80ff;
    }

    pub fn top(&self) -> u8 {
        ((self.status & TOP_MASK) >> TOP_SHIFT) as u8
    }

    fn set_top(&mut self, top: u8) {
        self.status = (self.status & !TOP_MASK) | ((u16::from(top) & 0x7) << TOP_SHIFT);
    }

    fn phys(&self, i: u8) -> usize {
        ((self.top() + i) & 0x7) as usize
    }

    /// Read ST(i).
    pub fn get(&self, i: u8) -> f64 {
        self.st[self.phys(i)]
    }

    /// Write ST(i) and refresh its tag.
    pub fn set(&mut self, i: u8, value: f64) {
        let p = self.phys(i);
        self.st[p] = value;
        self.tag_physical(p, value);
    }

    /// Push a value: TOP decrements, the new ST(0) takes the value.
    pub fn push(&mut self, value: f64) {
        let new_top = (self.top() + 7) & 0x7; // (top - 1) mod 8
        self.set_top(new_top);
        self.st[new_top as usize] = value;
        self.tag_physical(new_top as usize, value);
    }

    /// Pop: mark the current ST(0) empty and increment TOP.
    pub fn pop(&mut self) {
        let p = self.top() as usize;
        self.tag |= 0b11 << (p * 2);
        self.set_top((self.top() + 1) & 0x7);
    }

    /// Whether ST(i) currently holds a value (tag != empty).
    pub fn is_empty(&self, i: u8) -> bool {
        let p = self.phys(i);
        (self.tag >> (p * 2)) & 0b11 == 0b11
    }

    fn tag_physical(&mut self, phys: usize, value: f64) {
        let tag = if value == 0.0 {
            0b01
        } else if !value.is_finite() {
            0b10
        } else {
            0b00
        };
        self.tag = (self.tag & !(0b11 << (phys * 2))) | (tag << (phys * 2));
    }

    /// FINCSTP: rotate TOP up by one, leaving the tags untouched.
    pub fn inc_top(&mut self) {
        self.set_top((self.top() + 1) & 0x7);
    }

    /// FDECSTP: rotate TOP down by one, leaving the tags untouched.
    pub fn dec_top(&mut self) {
        self.set_top((self.top() + 7) & 0x7);
    }

    /// FFREE ST(i): mark the register empty without changing TOP.
    pub fn free(&mut self, i: u8) {
        let p = self.phys(i);
        self.tag |= 0b11 << (p * 2);
    }

    /// FXCH: swap ST(0) and ST(i).
    pub fn exchange(&mut self, i: u8) {
        let a = self.get(0);
        let b = self.get(i);
        self.set(0, b);
        self.set(i, a);
    }

    /// Read MMX register MMi.
    pub fn mm(&self, i: u8) -> u64 {
        self.mm[(i & 7) as usize]
    }

    /// Write MMX register MMi. Touching an MMX register marks every x87 tag valid,
    /// matching the silicon (the registers share storage).
    pub fn set_mm(&mut self, i: u8, value: u64) {
        self.mm[(i & 7) as usize] = value;
        self.tag = 0x0000;
    }

    /// EMMS: empty the MMX state by marking the x87 tag word all-empty.
    pub fn emms(&mut self) {
        self.tag = 0xffff;
    }

    /// Set one of the six exception flags (IE 0x01, DE 0x02, ZE 0x04, OE 0x08,
    /// UE 0x10, PE 0x20) and refresh the error-summary (ES) and busy (B) bits. ES
    /// follows whether any *unmasked* exception is now pending; the control word's
    /// low six bits are the masks.
    pub fn raise_exception(&mut self, flag: u16) {
        self.status |= flag;
        if self.pending_unmasked_exception() {
            self.status |= 0x80 | 0x8000; // ES + B
        }
    }

    /// True when an exception flag is set whose mask is clear: the next waiting FPU
    /// instruction (or FWAIT) must trap with #MF.
    pub fn pending_unmasked_exception(&self) -> bool {
        self.status & 0x3f & !(self.control & 0x3f) != 0
    }

    /// Set the four condition-code bits (used by FCOM/FXAM/FTST later).
    pub fn set_condition(&mut self, c3: bool, c2: bool, c1: bool, c0: bool) {
        let mut status = self.status & !(C0 | C1 | C2 | C3);
        if c0 {
            status |= C0;
        }
        if c1 {
            status |= C1;
        }
        if c2 {
            status |= C2;
        }
        if c3 {
            status |= C3;
        }
        self.status = status;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finit_sets_documented_reset_state() {
        let fpu = X87::default();
        assert_eq!(fpu.control, 0x037f);
        assert_eq!(fpu.status, 0);
        assert_eq!(fpu.tag, 0xffff);
        assert_eq!(fpu.top(), 0);
    }

    #[test]
    fn push_decrements_top_and_fills_st0() {
        let mut fpu = X87::default();
        fpu.push(1.5);
        assert_eq!(fpu.top(), 7);
        assert_eq!(fpu.get(0), 1.5);
        assert!(!fpu.is_empty(0));
        assert!(fpu.is_empty(1));
    }

    #[test]
    fn push_then_pop_restores_top_and_empties() {
        let mut fpu = X87::default();
        fpu.push(3.0);
        fpu.push(4.0);
        assert_eq!(fpu.get(0), 4.0);
        assert_eq!(fpu.get(1), 3.0);
        fpu.pop();
        assert_eq!(fpu.top(), 7);
        assert_eq!(fpu.get(0), 3.0);
    }

    #[test]
    fn top_is_reflected_in_the_status_word() {
        let mut fpu = X87::default();
        fpu.push(0.0);
        // TOP=7 lands in status bits 11-13.
        assert_eq!((fpu.status & TOP_MASK) >> TOP_SHIFT, 7);
    }
}
