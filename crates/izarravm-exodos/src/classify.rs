// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Conf-only classification, the input to the honest census.
//!
//! This looks at `dosbox.conf` and nothing else, so it can run over all 7,666
//! confs without extracting a single zip. It answers "would the translator
//! even try", not "did the game run": a `call run` conf is TRANSLATABLE here
//! and can still fail later when the called BAT turns out to loop.

use serde::Serialize;

use crate::conf::{AutoexecStep, DosboxConf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Class {
    Translatable,
    Recoverable,
    Untranslatable,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Translatable => "TRANSLATABLE",
            Class::Recoverable => "RECOVERABLE",
            Class::Untranslatable => "UNTRANSLATABLE",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfVerdict {
    pub class: Class,
    /// Reason codes, worst first. Empty for a clean TRANSLATABLE conf.
    pub reasons: Vec<String>,
    pub machine: String,
    pub memsize_mib: u32,
    pub cycles: String,
    pub sb_irq: Option<u8>,
    pub wants_gus: bool,
    pub wants_mt32: bool,
    pub speed_sensitive: bool,
    pub cd_image: Option<String>,
    pub has_call: bool,
    pub payload_commands: usize,
}

/// The video cards IzarraVM can scan out. VGA and SVGA are the bulk; CGA,
/// planar EGA and Hercules each have their own path in `izarravm-video`.
/// `tandy`, `pcjr` and `amstrad` have none, so a title that needs one is
/// refused rather than run against the wrong hardware.
///
/// An empty string is a conf with no `machine=` line, which DOSBox reads as its
/// `svga_s3` default. 6,946 of the corpus's 7,666 confs are that default, so an
/// empty or `svga_s3` value is silence about the game, not a claim.
///
/// This accepted only the VGA family until 2026-08-29, which refused 621 corpus
/// games outright and meant no sweep had ever run a CGA, EGA or Hercules title.
/// 454 of those refusals were stale: the cards had a path and the check had not
/// been told.
pub fn is_supported_video_machine(machine: &str) -> bool {
    let machine = machine.trim().to_ascii_lowercase();
    machine.is_empty()
        || machine.starts_with("svga_")
        || machine.starts_with("vesa_")
        || machine == "vgaonly"
        // eXo's own typo, 11 confs.
        || machine == "vesa_noflb"
        || machine == "cga"
        || machine == "ega"
        || machine == "hercules"
}

/// Image extensions `izarravm --cd-image` mounts. MEASURED against
/// `load_cd_image_from_path`: a `.cue` is parsed as a sheet and its FILE lines
/// (including the sibling `.bin`) are read through it; ANY other extension is
/// handed to `CdImage::from_iso`, which assumes 2048-byte data sectors. A bare
/// `.bin` is normally 2352-byte raw sectors, so it either fails the
/// multiple-of-2048 check or, when the length happens to divide, mounts
/// garbage. `.bin` is therefore supported only THROUGH its `.cue`, and an
/// imgmount naming one directly is refused rather than silently dropped.
pub fn is_supported_cd_extension(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".cue") || lower.ends_with(".iso")
}

fn image_kind(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".cue") {
        "cue"
    } else if lower.ends_with(".iso") {
        "iso"
    } else if lower.ends_with(".bin") {
        "bin"
    } else if lower.ends_with(".img")
        || lower.ends_with(".ima")
        || lower.ends_with(".dsk")
        || lower.ends_with(".vhd")
    {
        "floppy"
    } else {
        "unknown"
    }
}

pub fn classify_conf(conf: &DosboxConf) -> ConfVerdict {
    let mut hard: Vec<String> = Vec::new();
    let mut soft: Vec<String> = Vec::new();

    if !is_supported_video_machine(conf.machine()) {
        hard.push("machine-non-vga".to_string());
    }

    let mut mount_c = 0usize;
    let mut mount_other = 0usize;
    let mut cd_images: Vec<String> = Vec::new();
    let mut floppy_images = 0usize;
    let mut unknown_images = 0usize;
    let mut unsupported_cd_images = 0usize;
    let mut cd_steps = 0usize;
    let mut payload = 0usize;
    let mut has_call = false;
    let mut seen_mount = false;
    let mut switched_off_c = false;

    for step in &conf.autoexec {
        match step {
            AutoexecStep::Mount { drive, .. } => {
                seen_mount = true;
                if *drive == 'c' {
                    mount_c += 1;
                } else {
                    mount_other += 1;
                }
            }
            AutoexecStep::ImgMount { image, kind, .. } => {
                let file = image_kind(image);
                let cdrom = kind == "cdrom" || kind == "iso" || kind.is_empty();
                match (file, cdrom) {
                    ("floppy", _) | (_, false) => floppy_images += 1,
                    ("unknown", _) => unknown_images += 1,
                    ("bin", _) => unsupported_cd_images += 1,
                    _ => cd_images.push(image.clone()),
                }
            }
            AutoexecStep::Cd(_) => {
                // Host-side navigation before the mount is not a guest `cd`.
                if seen_mount {
                    cd_steps += 1;
                }
            }
            AutoexecStep::Pause => soft.push("pause-prompt".to_string()),
            AutoexecStep::Boot => hard.push("booter-disk".to_string()),
            AutoexecStep::Call(_) => {
                has_call = true;
                payload += 1;
            }
            AutoexecStep::Command(text) => {
                let lower = text.to_ascii_lowercase();
                let verb = lower.split_whitespace().next().unwrap_or("");
                match verb {
                    "goto" | "if" => hard.push("batch-control-flow".to_string()),
                    "choice" => soft.push("choice-menu".to_string()),
                    "basica" | "gwbasic" | "basic" => hard.push("needs-basic".to_string()),
                    "4dos" => hard.push("4dos-shell".to_string()),
                    "mixer" | "ver" | "config" | "aspect" => {}
                    _ if lower.starts_with("path=") || lower.starts_with("set ") => {}
                    _ => payload += 1,
                }
            }
            AutoexecStep::Drive(letter) => {
                if *letter != 'c' {
                    switched_off_c = true;
                }
            }
            AutoexecStep::Noise | AutoexecStep::Exit => {}
        }
    }

    if mount_c != 1 {
        hard.push("mount-c-count".to_string());
    }
    if mount_other > 0 {
        soft.push("extra-directory-mount".to_string());
    }
    if floppy_images > 0 {
        hard.push("floppy-image".to_string());
    }
    if unknown_images > 0 {
        hard.push("unrecognised-image".to_string());
    }
    if cd_images.len() > 1 {
        hard.push("multi-cd-swap".to_string());
    }
    if unsupported_cd_images > 0 {
        // A `.bin` named without its sheet. See `is_supported_cd_extension`:
        // the emulator would mount it as a 2048-byte ISO, and counting it as a
        // working CD is the census lying about a title that cannot boot.
        hard.push("cd-image-unsupported".to_string());
    }
    // The guest leaves C: for a drive nothing mounts. There is exactly one CD
    // in this machine and it comes from `--cd-image`; a conf that mounts a host
    // DIRECTORY as its CD, or imgmounts nothing at all, still writes `d:` and
    // its launch line then runs on a drive that does not exist. That is a boot
    // failure, not a recoverable translation.
    if switched_off_c && cd_images.is_empty() {
        hard.push("cd-mount-unsupported".to_string());
    }
    if payload == 0 {
        hard.push("no-launch-command".to_string());
    }
    if payload > 1 {
        soft.push("multiple-launch-commands".to_string());
    }
    if cd_steps > 1 {
        soft.push("multiple-cd".to_string());
    }
    if conf.memsize_mib() > 64 {
        soft.push("memsize-cap".to_string());
    }

    let class = if !hard.is_empty() {
        Class::Untranslatable
    } else if !soft.is_empty() {
        Class::Recoverable
    } else {
        Class::Translatable
    };
    let mut reasons = hard;
    reasons.extend(soft);
    reasons.sort();
    reasons.dedup();

    ConfVerdict {
        class,
        reasons,
        machine: conf.machine().to_string(),
        memsize_mib: conf.memsize_mib(),
        cycles: conf.cycles().to_string(),
        sb_irq: conf.sb_irq(),
        wants_gus: conf.wants_gus(),
        wants_mt32: conf.wants_mt32(),
        speed_sensitive: conf.cycles_fixed().is_some_and(|c| c < 5000),
        cd_image: cd_images.first().cloned(),
        has_call,
        payload_commands: payload,
    }
}

#[cfg(test)]
#[path = "classify_test.rs"]
mod tests;
