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
