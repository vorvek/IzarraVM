// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::{CString, c_char, c_double, c_int, c_void};
use std::path::Path;
use std::ptr::{self, NonNull};

use crate::midi::{self, MidiMessage};
use crate::{Error, SAMPLE_RATE_HZ};

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
    fn mt32emu_set_stereo_output_samplerate(context: *mut MuntContext, sample_rate: c_double);
    fn mt32emu_open_synth(context: *const MuntContext) -> c_int;
    fn mt32emu_close_synth(context: *const MuntContext);
    fn mt32emu_play_msg(context: *const MuntContext, message: u32) -> c_int;
    fn mt32emu_play_sysex(context: *const MuntContext, message: *const u8, length: u32) -> c_int;
    fn mt32emu_render_bit16s(context: *const MuntContext, output: *mut i16, frames: u32);
}

const ADDED_CONTROL_ROM: c_int = 1;
const ADDED_PCM_ROM: c_int = 2;
const ROM_NOT_IDENTIFIED: c_int = -1;
const FILE_NOT_FOUND: c_int = -2;
const MISSING_ROMS: c_int = -4;

#[derive(Debug)]
pub struct MuntSynth {
    context: NonNull<MuntContext>,
}

impl MuntSynth {
    pub fn new(control_rom: impl AsRef<Path>, pcm_rom: impl AsRef<Path>) -> Result<Self, Error> {
        let control_rom = control_rom.as_ref();
        let pcm_rom = pcm_rom.as_ref();
        let control_path = rom_path(control_rom)?;
        let pcm_path = rom_path(pcm_rom)?;
        let handler = ReportHandler { v0: ptr::null() };
        // SAFETY: A null report handler and instance pointer select libmt32emu defaults.
        let context = NonNull::new(unsafe { mt32emu_create_context(handler, ptr::null_mut()) })
            .ok_or(Error::NativeAllocation("Munt context"))?;
        let mut result = Self { context };
        result.add_rom(control_rom, &control_path, ADDED_CONTROL_ROM, "control")?;
        result.add_rom(pcm_rom, &pcm_path, ADDED_PCM_ROM, "PCM")?;
        // SAFETY: The context is live and this setting is applied before opening the synth.
        unsafe {
            mt32emu_set_stereo_output_samplerate(result.context.as_ptr(), f64::from(SAMPLE_RATE_HZ))
        };
        result.open()?;
        Ok(result)
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

    fn add_rom(
        &mut self,
        path: &Path,
        native_path: &CString,
        expected_code: c_int,
        expected_name: &'static str,
    ) -> Result<(), Error> {
        // SAFETY: The context is live and `native_path` is NUL-terminated for the call.
        let code = unsafe { mt32emu_add_rom_file(self.context.as_ptr(), native_path.as_ptr()) };
        match code {
            code if code == expected_code => Ok(()),
            ROM_NOT_IDENTIFIED => Err(Error::InvalidRom(path.to_path_buf())),
            FILE_NOT_FOUND => Err(Error::MissingRom(path.to_path_buf())),
            ADDED_CONTROL_ROM | ADDED_PCM_ROM => Err(Error::WrongRomType {
                path: path.to_path_buf(),
                expected: expected_name,
            }),
            _ => Err(Error::NativeCall {
                operation: "mt32emu_add_rom_file",
                code,
            }),
        }
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
    if code == 0 {
        Ok(())
    } else {
        Err(Error::NativeCall { operation, code })
    }
}
