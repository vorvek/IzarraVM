// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::{CString, c_char, c_double, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr::{self, NonNull};

use crate::midi::{self, MidiMessage};
use crate::{Error, RomKind, SAMPLE_RATE_HZ};

#[repr(C)]
struct MuntContext {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ReportHandler {
    v0: *const c_void,
}

// These declarations match libmt32emu 2.8.2's C interface. The report handler
// union is pointer-sized, and a null v0 member selects the library's default handler.
unsafe extern "C" {
    fn mt32emu_create_context(
        report_handler: ReportHandler,
        instance_data: *mut c_void,
    ) -> *mut MuntContext;
    fn mt32emu_free_context(context: *mut MuntContext);
    fn mt32emu_add_rom_file(context: *mut MuntContext, filename: *const c_char) -> c_int;
    fn mt32emu_merge_and_add_rom_files(
        context: *mut MuntContext,
        part1_filename: *const c_char,
        part2_filename: *const c_char,
    ) -> c_int;
    fn mt32emu_set_stereo_output_samplerate(context: *mut MuntContext, sample_rate: c_double);
    fn mt32emu_open_synth(context: *const MuntContext) -> c_int;
    fn mt32emu_close_synth(context: *const MuntContext);
    fn mt32emu_play_msg(context: *const MuntContext, message: u32) -> c_int;
    fn mt32emu_play_sysex(context: *const MuntContext, message: *const u8, length: u32) -> c_int;
    fn mt32emu_render_bit16s(context: *const MuntContext, output: *mut i16, frames: u32);
}

const ADDED_CONTROL_ROM: c_int = 1;
const ADDED_PCM_ROM: c_int = 2;
const MISSING_ROMS: c_int = -4;
/// `MT32EMU_RC_NOT_OPENED`: the context has no open synth. `mt32emu_play_msg`
/// and `mt32emu_play_sysex` return this or `QUEUE_FULL` and nothing else, so the
/// two have to be told apart HERE -- the caller decides whether a failed send
/// costs the message or the engine, and only one of these two costs the engine.
const NOT_OPENED: c_int = -5;
/// `MT32EMU_RC_QUEUE_FULL`: the synth is open and healthy, its event queue is
/// simply full this instant (the default report handler declines to wait).
const QUEUE_FULL: c_int = -6;

/// How many files a directory scan will offer to the library. A ROM set is a
/// handful of files; the cap only stops a user who points the picker at their
/// downloads folder from stalling the config panel.
const ROM_SCAN_LIMIT: usize = 64;

/// How many unidentified files are tried against each other as split halves.
/// Pairing is quadratic and every attempt re-reads and re-digests two files, so
/// the pool is kept to the size a real half-image set has (two to four parts)
/// plus room to spare.
const ROM_PAIR_LIMIT: usize = 8;

#[derive(Debug)]
pub struct MuntSynth {
    context: NonNull<MuntContext>,
}

impl MuntSynth {
    /// Open an MT-32 / CM-32L context from a control and a PCM ROM.
    ///
    /// Both arguments are HINTS, not a fixed layout. Either may name a single
    /// ROM file or the folder a ROM set lives in, the two may name the same
    /// place, and neither has to be the kind of image its name suggests -- the
    /// library identifies every file by SHA-1, so the set is found by content
    /// and the order the user picked them in does not matter. When the hints
    /// alone are not enough, the folders they sit in are scanned as well, which
    /// is what makes a set of split half-images work: those cannot be identified
    /// one at a time and have to be merged with their partner, and the partner
    /// is the file next door.
    ///
    /// Every rom-set layout in the wild differs in NAMING, and naming is exactly
    /// what this loader refuses to depend on: `MT32_CONTROL.ROM`,
    /// `CM32L_CONTROL.ROM`, `MT32_1.0.7_control.rom`, `ctrl_mt32_1_07.rom` and
    /// the `_a`/`_b` half pairs all arrive here as "some files"; the library
    /// says which is which.
    pub fn new(control_rom: impl AsRef<Path>, pcm_rom: impl AsRef<Path>) -> Result<Self, Error> {
        let control_rom = control_rom.as_ref();
        let pcm_rom = pcm_rom.as_ref();
        for hint in [control_rom, pcm_rom] {
            if !hint.exists() {
                return Err(Error::MissingRom(hint.to_path_buf()));
            }
        }
        let handler = ReportHandler { v0: ptr::null() };
        // SAFETY: A null report handler and instance pointer select libmt32emu defaults.
        let context = NonNull::new(unsafe { mt32emu_create_context(handler, ptr::null_mut()) })
            .ok_or(Error::NativeAllocation("Munt context"))?;
        let mut result = Self { context };

        // Round 1 is what the user actually chose; round 2 widens to the folders
        // those choices live in, and only if round 1 came up short. A user who
        // named two files gets those two files honoured first.
        let mut set = RomSet::default();
        let mut tried = Vec::new();
        result.offer(
            &direct_candidates(control_rom, pcm_rom),
            &mut set,
            &mut tried,
        );
        if !set.is_complete() {
            result.offer(
                &folder_candidates(control_rom, pcm_rom),
                &mut set,
                &mut tried,
            );
        }
        set.require(RomKind::Control, &tried)?;
        set.require(RomKind::Pcm, &tried)?;

        // SAFETY: The context is live and this setting is applied before opening the synth.
        unsafe {
            mt32emu_set_stereo_output_samplerate(result.context.as_ptr(), f64::from(SAMPLE_RATE_HZ))
        };
        result.open()?;
        Ok(result)
    }

    /// Offer `candidates` to the library, first one at a time and then, for the
    /// ones it could not identify alone, in pairs. A pair that merges is a ROM
    /// image split into halves, which `mt32emu_add_rom_file` cannot recognise on
    /// its own -- it matches against FULL images only.
    fn offer(&mut self, candidates: &[PathBuf], set: &mut RomSet, tried: &mut Vec<PathBuf>) {
        let mut unpaired: Vec<PathBuf> = Vec::new();
        for path in candidates {
            if tried.contains(path) {
                continue;
            }
            tried.push(path.clone());
            let Ok(native) = rom_path(path) else {
                continue;
            };
            // SAFETY: The context is live and `native` is NUL-terminated for the call.
            match unsafe { mt32emu_add_rom_file(self.context.as_ptr(), native.as_ptr()) } {
                ADDED_CONTROL_ROM => set.control = Some(path.clone()),
                ADDED_PCM_ROM => set.pcm = Some(path.clone()),
                _ => unpaired.push(path.clone()),
            }
        }
        if set.is_complete() {
            return;
        }
        unpaired.truncate(ROM_PAIR_LIMIT);
        for (first, second) in ordered_pairs(&unpaired) {
            if set.is_complete() {
                return;
            }
            let (Ok(first_native), Ok(second_native)) = (rom_path(&first), rom_path(&second))
            else {
                continue;
            };
            // SAFETY: The context is live and both paths are NUL-terminated for the call.
            let code = unsafe {
                mt32emu_merge_and_add_rom_files(
                    self.context.as_ptr(),
                    first_native.as_ptr(),
                    second_native.as_ptr(),
                )
            };
            match code {
                ADDED_CONTROL_ROM => set.control = Some(first),
                ADDED_PCM_ROM => set.pcm = Some(first),
                _ => {}
            }
        }
    }

    pub fn send(&mut self, message: &[u8]) -> Result<(), Error> {
        let code = match midi::validate(message)? {
            MidiMessage::Short { bytes, len } => {
                let packed = bytes[..len]
                    .iter()
                    .enumerate()
                    .fold(0_u32, |value, (index, byte)| {
                        value | (u32::from(*byte) << (index * 8))
                    });
                // SAFETY: The context is open and the packed message passed validation.
                unsafe { mt32emu_play_msg(self.context.as_ptr(), packed) }
            }
            MidiMessage::SysEx(bytes) => {
                let length = u32::try_from(bytes.len()).map_err(|_| Error::InvalidMidiMessage)?;
                // SAFETY: The context is open and `bytes` is a validated, readable SysEx message.
                unsafe { mt32emu_play_sysex(self.context.as_ptr(), bytes.as_ptr(), length) }
            }
        };
        munt_result("Munt MIDI", code)
    }

    pub fn render_interleaved_i16(&mut self, output: &mut [i16]) -> Result<(), Error> {
        if !output.len().is_multiple_of(2) {
            return Err(Error::OutputMustBeStereo);
        }
        let frames = u32::try_from(output.len() / 2).map_err(|_| Error::TooManyFrames)?;
        if frames == 0 {
            return Ok(());
        }
        // SAFETY: `output` contains exactly `frames * 2` writable interleaved samples and
        // the context remains open for the duration of the call.
        unsafe { mt32emu_render_bit16s(self.context.as_ptr(), output.as_mut_ptr(), frames) };
        Ok(())
    }

    pub fn all_notes_off(&mut self) -> Result<(), Error> {
        for channel in 0..16_u32 {
            let message = 0xB0 | channel | (123 << 8);
            // SAFETY: The context is open and this is a valid channel-mode message.
            munt_result("mt32emu_play_msg(all notes off)", unsafe {
                mt32emu_play_msg(self.context.as_ptr(), message)
            })?;
        }
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), Error> {
        // SAFETY: The context is currently open and uniquely accessed through `&mut self`.
        unsafe { mt32emu_close_synth(self.context.as_ptr()) };
        self.open()
    }

    fn open(&mut self) -> Result<(), Error> {
        // SAFETY: Both ROM loads completed and the context is live but not open.
        match unsafe { mt32emu_open_synth(self.context.as_ptr()) } {
            0 => Ok(()),
            MISSING_ROMS => Err(Error::MissingRoms),
            code => Err(Error::NativeCall {
                operation: "mt32emu_open_synth",
                code,
            }),
        }
    }
}

impl Drop for MuntSynth {
    fn drop(&mut self) {
        // SAFETY: The context is live, and close/free are called exactly once in order.
        unsafe {
            mt32emu_close_synth(self.context.as_ptr());
            mt32emu_free_context(self.context.as_ptr());
        }
    }
}

/// The images identified so far, and where each came from.
#[derive(Debug, Default)]
struct RomSet {
    control: Option<PathBuf>,
    pcm: Option<PathBuf>,
}

impl RomSet {
    fn is_complete(&self) -> bool {
        self.control.is_some() && self.pcm.is_some()
    }

    fn slot(&self, kind: RomKind) -> &Option<PathBuf> {
        match kind {
            RomKind::Control => &self.control,
            RomKind::Pcm => &self.pcm,
        }
    }

    /// Turn a missing image into an error that names what was looked at.
    ///
    /// One file, recognised as nothing, is reported as that file: "this ROM is
    /// not one I know" is a sharper thing to read than a list of one. Once
    /// anything HAS been identified the answer is which image is still missing,
    /// even if only one file was ever offered.
    fn require(&self, kind: RomKind, tried: &[PathBuf]) -> Result<(), Error> {
        if self.slot(kind).is_some() {
            return Ok(());
        }
        let nothing_identified = self.control.is_none() && self.pcm.is_none();
        match tried {
            [only] if nothing_identified => Err(Error::InvalidRom(only.clone())),
            _ => Err(Error::RomNotFound {
                kind,
                searched: tried.to_vec(),
            }),
        }
    }
}

/// The files the two hints name outright: each hint's own file, or every file
/// in it when the hint is a folder.
fn direct_candidates(control_hint: &Path, pcm_hint: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for hint in [control_hint, pcm_hint] {
        if hint.is_dir() {
            out.extend(files_in(hint));
        } else if hint.is_file() {
            push_unique(&mut out, hint.to_path_buf());
        }
    }
    out
}

/// The files sitting alongside the hints. Only consulted when the hints alone
/// did not produce a full set, which is the split-half case: half an image
/// cannot be identified by itself, and its partner is the file next door.
fn folder_candidates(control_hint: &Path, pcm_hint: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for hint in [control_hint, pcm_hint] {
        let folder = if hint.is_dir() {
            Some(hint.to_path_buf())
        } else {
            hint.parent().map(Path::to_path_buf)
        };
        if let Some(folder) = folder {
            for file in files_in(&folder) {
                push_unique(&mut out, file);
            }
        }
    }
    out
}

/// Files directly inside `folder`, in a stable order, capped.
fn files_in(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files.truncate(ROM_SCAN_LIMIT);
    files
}

fn push_unique(out: &mut Vec<PathBuf>, path: PathBuf) {
    if !out.contains(&path) {
        out.push(path);
    }
}

/// Every ordered pair of distinct entries. Ordered, because merging two halves
/// is not commutative: the library pairs part 1 with part 2, so a set whose
/// halves are named in the other order still has to be tried both ways.
fn ordered_pairs(paths: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let mut pairs = Vec::new();
    for (index, first) in paths.iter().enumerate() {
        for (other, second) in paths.iter().enumerate() {
            if index != other {
                pairs.push((first.clone(), second.clone()));
            }
        }
    }
    pairs
}

fn rom_path(path: &Path) -> Result<CString, Error> {
    if !path.is_file() {
        return Err(Error::MissingRom(path.to_path_buf()));
    }
    let value = path
        .to_str()
        .ok_or_else(|| Error::InvalidPath(path.to_path_buf()))?;
    CString::new(value).map_err(|_| Error::InvalidPath(path.to_path_buf()))
}

fn munt_result(operation: &'static str, code: c_int) -> Result<(), Error> {
    match code {
        0 => Ok(()),
        QUEUE_FULL => Err(Error::SynthQueueFull),
        NOT_OPENED => Err(Error::SynthNotOpened),
        code => Err(Error::NativeCall { operation, code }),
    }
}

#[cfg(test)]
#[path = "munt_test.rs"]
mod tests;
