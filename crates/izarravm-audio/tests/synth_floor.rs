//! G0 probe (2026-07-07 JIT/perf plan): measure the OPL3 audio-synthesis wall
//! floor. One 586 guest-second = OPL_NATIVE_HZ (49716) render_sample calls (the
//! debt formula in gui.rs: samples/guest-s = OPL_NATIVE_HZ). So timing 49716
//! render_sample calls = the wall cost of one guest-second of OPL synthesis, and
//! `audio-only rt ceiling = 1 / (that wall time in seconds)`. If that ceiling is
//! below 1.6, audio synthesis ALONE blocks the GUI 1.6x target (render_sample is
//! native -O3 Rust the dynarec cannot speed up). Run:
//!   cargo test -j8 -p izarravm-audio --release --test synth_floor -- --ignored --nocapture
use izarravm_audio::OplChip;

const OPL_NATIVE_HZ: usize = 49716; // samples per 586 guest-second
const TARGET_RT: f64 = 1.6;

fn time_samples(label: &str, opl: &mut OplChip, n: usize) {
    // warm up (branch predictor / caches) then time.
    for _ in 0..OPL_NATIVE_HZ {
        std::hint::black_box(opl.render_sample());
    }
    let t = std::time::Instant::now();
    for _ in 0..n {
        std::hint::black_box(opl.render_sample());
    }
    let wall = t.elapsed().as_secs_f64();
    let per_guest_second = wall * (OPL_NATIVE_HZ as f64 / n as f64);
    let ceiling = 1.0 / per_guest_second;
    eprintln!(
        "{label:<22} {n} samples in {:.1} ms  =>  {:.4} wall-s / guest-s  =>  audio-only rt ceiling {:.2}x   [{}]",
        wall * 1000.0,
        per_guest_second,
        ceiling,
        if ceiling >= TARGET_RT {
            "OK for 1.6x"
        } else {
            "BLOCKS 1.6x on audio alone"
        },
    );
}

fn enable_opl3(opl: &mut OplChip) {
    opl.write_port(0x038a, 0x05);
    opl.write_port(0x038b, 0x01); // reg 0x105 NEW bit -> 18 channels
}

// Key on channel `ch` (bank 0 = ch 0..8) with a sustained loud carrier so the
// operator envelopes are ACTIVE, not released — the realistic "music playing" load.
fn key_on_bank0(opl: &mut OplChip, ch: u8) {
    opl.write_register(0x20 + op_slot(ch, 0), 0x21); // modulator: sustain, mult 1
    opl.write_register(0x20 + op_slot(ch, 1), 0x21); // carrier
    opl.write_register(0x40 + op_slot(ch, 0), 0x10);
    opl.write_register(0x40 + op_slot(ch, 1), 0x00); // carrier loud
    opl.write_register(0x60 + op_slot(ch, 0), 0xf0); // fast attack, no decay
    opl.write_register(0x60 + op_slot(ch, 1), 0xf0);
    opl.write_register(0x80 + op_slot(ch, 0), 0x00);
    opl.write_register(0x80 + op_slot(ch, 1), 0x00);
    opl.write_register(0xc0 + ch, 0x01); // additive so carrier reaches output
    opl.write_register(0xa0 + ch, 0x40);
    opl.write_register(0xb0 + ch, 0x20 | (4 << 2)); // key-on, block 4
}

// Operator register slot offset for (channel, op) within a bank. OPL layout: the
// two operators of channel c live at base offsets `[0,1,2, 8,9,10, 16,17,18][c]`
// plus 0 (modulator) / 3 (carrier).
fn op_slot(ch: u8, op: u8) -> u8 {
    const BASE: [u8; 9] = [0, 1, 2, 8, 9, 10, 16, 17, 18];
    BASE[ch as usize] + op * 3
}

#[test]
#[ignore]
fn opl_synthesis_wall_floor() {
    eprintln!(
        "\n=== G0 audio-synthesis wall floor (1 guest-second = {OPL_NATIVE_HZ} OPL samples @ 586) ==="
    );

    let mut idle2 = OplChip::default();
    time_samples("OPL2 idle (9ch)", &mut idle2, OPL_NATIVE_HZ);

    let mut idle3 = OplChip::default();
    enable_opl3(&mut idle3);
    time_samples("OPL3 idle (18ch)", &mut idle3, OPL_NATIVE_HZ);

    let mut loud2 = OplChip::default();
    for ch in 0..9 {
        key_on_bank0(&mut loud2, ch);
    }
    time_samples("OPL2 9 voices keyed", &mut loud2, OPL_NATIVE_HZ);

    let mut loud3 = OplChip::default();
    enable_opl3(&mut loud3);
    for ch in 0..9 {
        key_on_bank0(&mut loud3, ch); // bank-0 half; bank-1 voices stay idle (still rendered)
    }
    time_samples("OPL3 9 voices keyed", &mut loud3, OPL_NATIVE_HZ);

    eprintln!("=== end G0 audio floor ===\n");
}
