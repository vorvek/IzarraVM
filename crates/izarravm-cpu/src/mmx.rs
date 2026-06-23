//! Pure MMX lane operations on 64-bit registers. The CPU dispatch in lib.rs
//! reads the operands and the x87-aliased register file; everything numeric
//! lives here so it is testable on its own.
//!
//! Limit: MMX registers are modeled as a separate [u64; 8] in the x87 state.
//! Real silicon aliases MM0-7 onto the x87 register mantissas. The visible tag
//! effect is modeled (writing MMX marks the tags valid, EMMS marks them empty),
//! but reading an x87 register as a float after MMX use without EMMS is not
//! bit-faithful. Real code always issues EMMS between MMX and x87.

fn map_bytes(a: u64, b: u64, f: impl Fn(u8, u8) -> u8) -> u64 {
    let mut out = [0u8; 8];
    let (ab, bb) = (a.to_le_bytes(), b.to_le_bytes());
    for ((o, &x), &y) in out.iter_mut().zip(ab.iter()).zip(bb.iter()) {
        *o = f(x, y);
    }
    u64::from_le_bytes(out)
}

fn words(v: u64) -> [u16; 4] {
    [
        v as u16,
        (v >> 16) as u16,
        (v >> 32) as u16,
        (v >> 48) as u16,
    ]
}

fn from_words(w: [u16; 4]) -> u64 {
    u64::from(w[0]) | (u64::from(w[1]) << 16) | (u64::from(w[2]) << 32) | (u64::from(w[3]) << 48)
}

fn dwords(v: u64) -> [u32; 2] {
    [v as u32, (v >> 32) as u32]
}

fn from_dwords(d: [u32; 2]) -> u64 {
    u64::from(d[0]) | (u64::from(d[1]) << 32)
}

fn map_words(a: u64, b: u64, f: impl Fn(u16, u16) -> u16) -> u64 {
    let (aw, bw) = (words(a), words(b));
    let mut out = [0u16; 4];
    for ((o, &x), &y) in out.iter_mut().zip(aw.iter()).zip(bw.iter()) {
        *o = f(x, y);
    }
    from_words(out)
}

fn map_dwords(a: u64, b: u64, f: impl Fn(u32, u32) -> u32) -> u64 {
    let (ad, bd) = (dwords(a), dwords(b));
    let mut out = [0u32; 2];
    for ((o, &x), &y) in out.iter_mut().zip(ad.iter()).zip(bd.iter()) {
        *o = f(x, y);
    }
    from_dwords(out)
}

// ---- add / subtract (wrapping) ----
pub fn padd_b(a: u64, b: u64) -> u64 {
    map_bytes(a, b, u8::wrapping_add)
}
pub fn padd_w(a: u64, b: u64) -> u64 {
    map_words(a, b, u16::wrapping_add)
}
pub fn padd_d(a: u64, b: u64) -> u64 {
    map_dwords(a, b, u32::wrapping_add)
}
pub fn psub_b(a: u64, b: u64) -> u64 {
    map_bytes(a, b, u8::wrapping_sub)
}
pub fn psub_w(a: u64, b: u64) -> u64 {
    map_words(a, b, u16::wrapping_sub)
}
pub fn psub_d(a: u64, b: u64) -> u64 {
    map_dwords(a, b, u32::wrapping_sub)
}

// ---- saturating add / subtract ----
pub fn padds_b(a: u64, b: u64) -> u64 {
    map_bytes(a, b, |x, y| {
        (i16::from(x as i8) + i16::from(y as i8)).clamp(-128, 127) as i8 as u8
    })
}
pub fn padds_w(a: u64, b: u64) -> u64 {
    map_words(a, b, |x, y| {
        (i32::from(x as i16) + i32::from(y as i16)).clamp(-32768, 32767) as i16 as u16
    })
}
pub fn paddus_b(a: u64, b: u64) -> u64 {
    map_bytes(a, b, u8::saturating_add)
}
pub fn paddus_w(a: u64, b: u64) -> u64 {
    map_words(a, b, u16::saturating_add)
}
pub fn psubs_b(a: u64, b: u64) -> u64 {
    map_bytes(a, b, |x, y| {
        (i16::from(x as i8) - i16::from(y as i8)).clamp(-128, 127) as i8 as u8
    })
}
pub fn psubs_w(a: u64, b: u64) -> u64 {
    map_words(a, b, |x, y| {
        (i32::from(x as i16) - i32::from(y as i16)).clamp(-32768, 32767) as i16 as u16
    })
}
pub fn psubus_b(a: u64, b: u64) -> u64 {
    map_bytes(a, b, u8::saturating_sub)
}
pub fn psubus_w(a: u64, b: u64) -> u64 {
    map_words(a, b, u16::saturating_sub)
}

// ---- compare (per-lane, all-ones on true) ----
pub fn pcmpeq_b(a: u64, b: u64) -> u64 {
    map_bytes(a, b, |x, y| if x == y { 0xff } else { 0 })
}
pub fn pcmpeq_w(a: u64, b: u64) -> u64 {
    map_words(a, b, |x, y| if x == y { 0xffff } else { 0 })
}
pub fn pcmpeq_d(a: u64, b: u64) -> u64 {
    map_dwords(a, b, |x, y| if x == y { 0xffff_ffff } else { 0 })
}
pub fn pcmpgt_b(a: u64, b: u64) -> u64 {
    map_bytes(a, b, |x, y| if (x as i8) > (y as i8) { 0xff } else { 0 })
}
pub fn pcmpgt_w(a: u64, b: u64) -> u64 {
    map_words(
        a,
        b,
        |x, y| if (x as i16) > (y as i16) { 0xffff } else { 0 },
    )
}
pub fn pcmpgt_d(a: u64, b: u64) -> u64 {
    map_dwords(a, b, |x, y| {
        if (x as i32) > (y as i32) {
            0xffff_ffff
        } else {
            0
        }
    })
}

// ---- multiply ----
pub fn pmullw(a: u64, b: u64) -> u64 {
    map_words(a, b, |x, y| {
        (i32::from(x as i16) * i32::from(y as i16)) as u16
    })
}
pub fn pmulhw(a: u64, b: u64) -> u64 {
    map_words(a, b, |x, y| {
        ((i32::from(x as i16) * i32::from(y as i16)) >> 16) as u16
    })
}
pub fn pmaddwd(a: u64, b: u64) -> u64 {
    let (aw, bw) = (words(a), words(b));
    let lo = i32::from(aw[0] as i16) * i32::from(bw[0] as i16)
        + i32::from(aw[1] as i16) * i32::from(bw[1] as i16);
    let hi = i32::from(aw[2] as i16) * i32::from(bw[2] as i16)
        + i32::from(aw[3] as i16) * i32::from(bw[3] as i16);
    from_dwords([lo as u32, hi as u32])
}

// ---- logical ----
pub fn pand(a: u64, b: u64) -> u64 {
    a & b
}
pub fn pandn(a: u64, b: u64) -> u64 {
    !a & b
}
pub fn por(a: u64, b: u64) -> u64 {
    a | b
}
pub fn pxor(a: u64, b: u64) -> u64 {
    a ^ b
}

// ---- pack (saturate) ----
fn sat_i8(v: i32) -> u8 {
    v.clamp(-128, 127) as i8 as u8
}
fn sat_u8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}
fn sat_i16(v: i32) -> u16 {
    v.clamp(-32768, 32767) as i16 as u16
}

pub fn packsswb(a: u64, b: u64) -> u64 {
    let (aw, bw) = (words(a), words(b));
    let mut out = [0u8; 8];
    for (o, &w) in out.iter_mut().zip(aw.iter().chain(bw.iter())) {
        *o = sat_i8(i32::from(w as i16));
    }
    u64::from_le_bytes(out)
}
pub fn packuswb(a: u64, b: u64) -> u64 {
    let (aw, bw) = (words(a), words(b));
    let mut out = [0u8; 8];
    for (o, &w) in out.iter_mut().zip(aw.iter().chain(bw.iter())) {
        *o = sat_u8(i32::from(w as i16));
    }
    u64::from_le_bytes(out)
}
pub fn packssdw(a: u64, b: u64) -> u64 {
    let (ad, bd) = (dwords(a), dwords(b));
    let mut out = [0u16; 4];
    for (o, &d) in out.iter_mut().zip(ad.iter().chain(bd.iter())) {
        *o = sat_i16(d as i32);
    }
    from_words(out)
}

// ---- unpack ----
pub fn punpcklbw(a: u64, b: u64) -> u64 {
    let (ab, bb) = (a.to_le_bytes(), b.to_le_bytes());
    let mut out = [0u8; 8];
    for k in 0..4 {
        out[2 * k] = ab[k];
        out[2 * k + 1] = bb[k];
    }
    u64::from_le_bytes(out)
}
pub fn punpckhbw(a: u64, b: u64) -> u64 {
    let (ab, bb) = (a.to_le_bytes(), b.to_le_bytes());
    let mut out = [0u8; 8];
    for k in 0..4 {
        out[2 * k] = ab[k + 4];
        out[2 * k + 1] = bb[k + 4];
    }
    u64::from_le_bytes(out)
}
pub fn punpcklwd(a: u64, b: u64) -> u64 {
    let (aw, bw) = (words(a), words(b));
    from_words([aw[0], bw[0], aw[1], bw[1]])
}
pub fn punpckhwd(a: u64, b: u64) -> u64 {
    let (aw, bw) = (words(a), words(b));
    from_words([aw[2], bw[2], aw[3], bw[3]])
}
pub fn punpckldq(a: u64, b: u64) -> u64 {
    from_dwords([dwords(a)[0], dwords(b)[0]])
}
pub fn punpckhdq(a: u64, b: u64) -> u64 {
    from_dwords([dwords(a)[1], dwords(b)[1]])
}

// ---- shifts (count from the source; out-of-range clears, arithmetic fills sign) ----
pub fn psllw(a: u64, count: u64) -> u64 {
    if count > 15 {
        return 0;
    }
    map_words(a, 0, |x, _| x << count)
}
pub fn pslld(a: u64, count: u64) -> u64 {
    if count > 31 {
        return 0;
    }
    map_dwords(a, 0, |x, _| x << count)
}
pub fn psllq(a: u64, count: u64) -> u64 {
    if count > 63 { 0 } else { a << count }
}
pub fn psrlw(a: u64, count: u64) -> u64 {
    if count > 15 {
        return 0;
    }
    map_words(a, 0, |x, _| x >> count)
}
pub fn psrld(a: u64, count: u64) -> u64 {
    if count > 31 {
        return 0;
    }
    map_dwords(a, 0, |x, _| x >> count)
}
pub fn psrlq(a: u64, count: u64) -> u64 {
    if count > 63 { 0 } else { a >> count }
}
pub fn psraw(a: u64, count: u64) -> u64 {
    let c = count.min(15) as u32;
    map_words(a, 0, |x, _| ((x as i16) >> c) as u16)
}
pub fn psrad(a: u64, count: u64) -> u64 {
    let c = count.min(31) as u32;
    map_dwords(a, 0, |x, _| ((x as i32) >> c) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padd_w_wraps_per_lane() {
        // lanes: 0xffff+1=0, 1+1=2, 0x7fff+1=0x8000, 0+0=0
        let a = from_words([0xffff, 1, 0x7fff, 0]);
        let b = from_words([1, 1, 1, 0]);
        assert_eq!(words(padd_w(a, b)), [0, 2, 0x8000, 0]);
    }

    #[test]
    fn padds_w_saturates_signed() {
        let a = from_words([0x7fff, 0x8000, 0, 0]);
        let b = from_words([1, 0xffff, 0, 0]); // +1, -1
        assert_eq!(words(padds_w(a, b)), [0x7fff, 0x8000, 0, 0]);
    }

    #[test]
    fn paddus_b_saturates_unsigned() {
        let a = u64::from_le_bytes([250, 10, 0, 0, 0, 0, 0, 0]);
        let b = u64::from_le_bytes([10, 10, 0, 0, 0, 0, 0, 0]);
        assert_eq!(paddus_b(a, b).to_le_bytes(), [255, 20, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn pcmpgt_w_is_signed() {
        let a = from_words([0, 5, 0xffff, 0]); // 0, 5, -1
        let b = from_words([0xffff, 4, 0, 0]); // -1, 4, 0
        assert_eq!(words(pcmpgt_w(a, b)), [0xffff, 0xffff, 0, 0]);
    }

    #[test]
    fn punpcklbw_interleaves_low_bytes() {
        let a = u64::from_le_bytes([1, 2, 3, 4, 0, 0, 0, 0]);
        let b = u64::from_le_bytes([0x11, 0x22, 0x33, 0x44, 0, 0, 0, 0]);
        assert_eq!(
            punpcklbw(a, b).to_le_bytes(),
            [1, 0x11, 2, 0x22, 3, 0x33, 4, 0x44]
        );
    }

    #[test]
    fn packsswb_saturates_to_signed_bytes() {
        let a = from_words([0x7fff, 0x8000, 5, 0xfffb]); // 32767, -32768, 5, -5
        assert_eq!(packsswb(a, 0).to_le_bytes(), [127, 128, 5, 251, 0, 0, 0, 0]);
    }

    #[test]
    fn pmaddwd_multiplies_and_adds_pairs() {
        let a = from_words([2, 3, 4, 5]);
        let b = from_words([10, 20, 30, 40]);
        // lo = 2*10 + 3*20 = 80; hi = 4*30 + 5*40 = 320
        assert_eq!(dwords(pmaddwd(a, b)), [80, 320]);
    }

    #[test]
    fn psllq_shifts_the_whole_register() {
        assert_eq!(psllq(1, 4), 16);
        assert_eq!(psllq(1, 64), 0);
    }

    #[test]
    fn psraw_replicates_sign() {
        let a = from_words([0x8000, 0x4000, 0, 0]); // -32768, 16384
        assert_eq!(words(psraw(a, 1)), [0xc000, 0x2000, 0, 0]);
    }
}
