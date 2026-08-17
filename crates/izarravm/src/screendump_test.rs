// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

use izarravm_core::VideoCard;
use izarravm_machine::MachineProfile;

/// A scratch directory for one test, removed when the guard drops.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!(
            "izarravm-screendump-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A halting guest. The dumper never runs the machine itself, so the picture is
/// driven here by writing text memory directly, which is what a game does.
fn dump_test_machine() -> Machine {
    let mut machine =
        Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xF4])
            .expect("build raw machine");
    // Present a first frame, so the sample reads a real one rather than an
    // empty scanout.
    machine.advance_devices_ticks(MASTER_CLOCK_HZ / 20);
    machine
}

/// Write one glyph into the text page and let the raster present it.
fn poke_text(machine: &mut Machine, offset: u32, glyph: u8) {
    machine.write_physical_u8(0xB_8000 + offset, glyph);
    machine.write_physical_u8(0xB_8000 + offset + 1, 0x07);
    machine.advance_devices_ticks(MASTER_CLOCK_HZ / 20);
}

fn index_lines(dir: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(dir.join("screens.jsonl"))
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a screens.jsonl line is JSON"))
        .collect()
}

#[test]
fn the_index_line_carries_every_field_the_sweep_reads() {
    let scratch = Scratch::new("shape");
    let mut machine = dump_test_machine();
    let mut dumper = ScreenDumper::new(scratch.path(), 5_000_000).unwrap();
    dumper.after_slice(&machine);
    poke_text(&mut machine, 0, b'A');
    dumper.after_slice(&machine);
    dumper.finish();

    let lines = index_lines(scratch.path());
    assert_eq!(lines.len(), 2);
    for (expected_index, line) in lines.iter().enumerate() {
        assert_eq!(line["i"].as_u64(), Some(expected_index as u64));
        assert!(line["master_ticks"].is_u64());
        assert!(line["guest_ms"].is_u64());
        // The sweep's `Measure-ScreenRecurrence` reads `hash`, `Get-Outcome`
        // reads `video_mode`; both must be present on every line.
        assert_eq!(line["hash"].as_str().map(str::len), Some(16));
        assert_eq!(line["display"].as_str(), Some("vga"));
        assert_eq!(line["video_mode"].as_str(), Some("text"));
        assert!(line["changed"].is_boolean());
        // Text mode reports its glyph count; a graphics mode reports null.
        assert!(line["text_glyphs"].is_u64());
    }
    assert_ne!(
        lines[0]["hash"], lines[1]["hash"],
        "writing a glyph must move the frame hash"
    );
    assert!(lines[1]["guest_ms"].as_u64() >= lines[0]["guest_ms"].as_u64());
}

#[test]
fn a_ppm_is_written_only_when_the_hash_moves() {
    let scratch = Scratch::new("ppm");
    let mut machine = dump_test_machine();
    let mut dumper = ScreenDumper::new(scratch.path(), 5_000_000).unwrap();
    // Three samples of one unchanged picture, then one changed.
    dumper.after_slice(&machine);
    dumper.after_slice(&machine);
    dumper.after_slice(&machine);
    poke_text(&mut machine, 0, b'Z');
    dumper.after_slice(&machine);
    dumper.finish();

    let lines = index_lines(scratch.path());
    let changed: Vec<bool> = lines
        .iter()
        .map(|line| line["changed"].as_bool().unwrap())
        .collect();
    assert_eq!(changed, vec![true, false, false, true]);
    // The PPM name is on the changed lines and null on the rest, and exactly
    // those files exist: 60 samples of one 640x480 picture is 55 MB of the
    // same image, which is why the writes are gated on the hash.
    assert_eq!(lines[0]["ppm"].as_str(), Some("0000.ppm"));
    assert!(lines[1]["ppm"].is_null());
    assert!(lines[2]["ppm"].is_null());
    assert_eq!(lines[3]["ppm"].as_str(), Some("0003.ppm"));

    let mut written: Vec<String> = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".ppm"))
        .collect();
    written.sort();
    assert_eq!(
        written,
        vec!["0000.ppm".to_string(), "0003.ppm".to_string()]
    );

    let ppm = std::fs::read(scratch.path().join("0000.ppm")).unwrap();
    assert!(ppm.starts_with(b"P6\n"), "not a binary PPM");
    let header: String = String::from_utf8_lossy(&ppm[..24]).to_string();
    let mut parts = header.split_whitespace().skip(1);
    let width: usize = parts.next().unwrap().parse().unwrap();
    let height: usize = parts.next().unwrap().parse().unwrap();
    let pixels_at = ppm
        .windows(4)
        .position(|w| w == b"255\n")
        .expect("a maxval line")
        + 4;
    assert_eq!(ppm.len() - pixels_at, width * height * 3);
}

// ---------------------------------------------------------------------------
// Defect E8: the frame that is not there.
//
// `Vga::presented` holds the last FINALIZED frame, and every mode set clears it
// on purpose so a consumer cannot be handed a frame with the previous mode's
// geometry. It stays empty until the beam finishes the next frame — up to one
// frame period, about 14 ms at 70 Hz. A sample landing in that window has no
// frame to write.
//
// The stage-1 sweep wrote one anyway: 30 of its 1,854 kept frames are the
// byte-identical 14-byte PPM `P6\n1 1\n255\n\0\0\0`, one in each of 30
// different games. A single black pixel is not a screenshot, and it read as
// data all the way through the classifier.
// ---------------------------------------------------------------------------

/// A machine that has never completed a frame. `dump_test_machine` advances
/// past this state deliberately; this one stops inside it.
fn unpresented_machine() -> Machine {
    Machine::new_raw_program(MachineProfile::gsw_386(16, VideoCard::Vega), &[0xF4])
        .expect("build raw machine")
}

#[test]
fn a_sample_before_the_first_frame_writes_no_ppm() {
    let scratch = Scratch::new("noframe");
    let machine = unpresented_machine();
    let mut dumper = ScreenDumper::new(scratch.path(), 5_000_000).unwrap();
    dumper.after_slice(&machine);
    dumper.finish();

    let written: Vec<String> = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".ppm"))
        .collect();
    assert!(
        written.is_empty(),
        "wrote a frame with nothing to show: {written:?}"
    );
}

/// The sample still reports itself. The index line is the sweep's only liveness
/// signal — its watchdog kills a run whose index stops growing — so a guest that
/// wedges its CRTC and never finalizes another frame must not read as a dead
/// process. The line says there was no frame instead of inventing one.
#[test]
fn a_sample_before_the_first_frame_still_writes_an_index_line() {
    let scratch = Scratch::new("noframe-index");
    let machine = unpresented_machine();
    let mut dumper = ScreenDumper::new(scratch.path(), 5_000_000).unwrap();
    dumper.after_slice(&machine);
    dumper.finish();

    let lines = index_lines(scratch.path());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["presented"].as_bool(), Some(false));
    assert!(
        lines[0]["hash"].is_null(),
        "a frame that does not exist has no hash"
    );
    assert!(lines[0]["ppm"].is_null());
    assert_eq!(lines[0]["changed"].as_bool(), Some(false));
    // What the machine knows without a frame is still reported.
    assert!(lines[0]["master_ticks"].is_u64());
    assert_eq!(lines[0]["display"].as_str(), Some("vga"));
}

/// The mid-run case, which is 28 of the 30. A frame is presented, the guest sets
/// a mode, and the next sample arrives before the beam has finished a frame in
/// the new mode.
#[test]
fn a_sample_between_a_mode_set_and_its_first_frame_writes_no_ppm() {
    let scratch = Scratch::new("modeset");
    let mut machine = dump_test_machine();
    let mut dumper = ScreenDumper::new(scratch.path(), 5_000_000).unwrap();
    dumper.after_slice(&machine);

    // The mode set drops the text frame; no frame exists in mode 13h yet.
    assert!(machine.set_vga_mode(0x13));
    dumper.after_slice(&machine);

    // One frame later there is a picture again.
    machine.advance_devices_ticks(MASTER_CLOCK_HZ / 20);
    dumper.after_slice(&machine);
    dumper.finish();

    let lines = index_lines(scratch.path());
    assert_eq!(lines.len(), 3);
    assert!(lines[0]["hash"].is_string(), "the text frame");
    assert_eq!(
        lines[1]["presented"].as_bool(),
        Some(false),
        "no frame exists between the mode set and the first raster of the new mode"
    );
    assert!(lines[1]["ppm"].is_null());
    assert!(lines[2]["hash"].is_string(), "the mode 13h frame");
    assert_eq!(lines[2]["video_mode"].as_str(), Some("mode13h"));

    // Two PPMs, not three: the middle sample had nothing to write.
    let mut written: Vec<String> = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".ppm"))
        .collect();
    written.sort();
    assert_eq!(
        written,
        vec!["0000.ppm".to_string(), "0002.ppm".to_string()]
    );
}

/// The placeholder also poisoned the CHANGE test. A missing frame must not count
/// as a picture, so it cannot make the next real frame look unchanged, and two
/// missing frames in a row must not look like one steady picture.
#[test]
fn a_missing_frame_is_not_a_picture_the_next_sample_can_match() {
    let scratch = Scratch::new("nochange");
    let mut machine = dump_test_machine();
    let mut dumper = ScreenDumper::new(scratch.path(), 5_000_000).unwrap();

    // A mode 13h picture, sampled once.
    assert!(machine.set_vga_mode(0x13));
    machine.advance_devices_ticks(MASTER_CLOCK_HZ / 20);
    dumper.after_slice(&machine);
    let first = index_lines(scratch.path())[0]["hash"]
        .as_str()
        .unwrap()
        .to_string();

    // Re-setting the same mode drops the frame without touching video memory,
    // so the picture that comes back is the one that left.
    assert!(machine.set_vga_mode(0x13));
    dumper.after_slice(&machine);
    dumper.after_slice(&machine);
    machine.advance_devices_ticks(MASTER_CLOCK_HZ / 20);
    dumper.after_slice(&machine);
    dumper.finish();

    let lines = index_lines(scratch.path());
    assert_eq!(lines.len(), 4);
    for missing in &lines[1..3] {
        assert_eq!(missing["presented"].as_bool(), Some(false));
        assert_eq!(missing["changed"].as_bool(), Some(false));
        assert!(missing["hash"].is_null());
    }
    // The returning picture is compared against the last REAL frame, so an
    // identical one is still reported unchanged and costs no second PPM.
    assert_eq!(lines[3]["hash"].as_str(), Some(first.as_str()));
    assert_eq!(lines[3]["changed"].as_bool(), Some(false));
    assert!(lines[3]["ppm"].is_null());
}

#[test]
fn the_slice_never_falls_below_the_floor() {
    let scratch = Scratch::new("slice");
    // A tiny interval would sample every few instructions and turn the run
    // into a screenshot loop, so the floor is a million guest clocks.
    for requested in [0, 1, 999_999, 1_000_000] {
        let dumper = ScreenDumper::new(scratch.path(), requested).unwrap();
        assert_eq!(dumper.slice, 1_000_000, "requested {requested}");
        dumper.finish();
    }
    let dumper = ScreenDumper::new(scratch.path(), 8_300_000).unwrap();
    assert_eq!(dumper.slice, 8_300_000);
    dumper.finish();
}
