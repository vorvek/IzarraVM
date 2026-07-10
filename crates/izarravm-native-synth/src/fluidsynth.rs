// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::Path;
use std::ptr::{self, NonNull};

use crate::midi::{self, MidiMessage};
use crate::{Error, SAMPLE_RATE_HZ};

#[repr(C)]
struct FluidSettings {
    _private: [u8; 0],
}

#[repr(C)]
struct FluidSynthHandle {
    _private: [u8; 0],
}

// These declarations match FluidSynth 2.5.6's public C interface. The build script
// links that exact source release, so the opaque handles and calling convention agree.
unsafe extern "C" {
    fn new_fluid_settings() -> *mut FluidSettings;
    fn delete_fluid_settings(settings: *mut FluidSettings);
    fn fluid_settings_setnum(
        settings: *mut FluidSettings,
        name: *const c_char,
        value: f64,
    ) -> c_int;
    fn new_fluid_synth(settings: *mut FluidSettings) -> *mut FluidSynthHandle;
    fn delete_fluid_synth(synth: *mut FluidSynthHandle);
    fn fluid_synth_sfload(
        synth: *mut FluidSynthHandle,
        filename: *const c_char,
        reset_presets: c_int,
    ) -> c_int;
    fn fluid_synth_noteon(
        synth: *mut FluidSynthHandle,
        channel: c_int,
        key: c_int,
        velocity: c_int,
    ) -> c_int;
    fn fluid_synth_noteoff(synth: *mut FluidSynthHandle, channel: c_int, key: c_int) -> c_int;
    fn fluid_synth_cc(
        synth: *mut FluidSynthHandle,
        channel: c_int,
        controller: c_int,
        value: c_int,
    ) -> c_int;
    fn fluid_synth_program_change(
        synth: *mut FluidSynthHandle,
        channel: c_int,
        program: c_int,
    ) -> c_int;
    fn fluid_synth_channel_pressure(
        synth: *mut FluidSynthHandle,
        channel: c_int,
        value: c_int,
    ) -> c_int;
    fn fluid_synth_key_pressure(
        synth: *mut FluidSynthHandle,
        channel: c_int,
        key: c_int,
        value: c_int,
    ) -> c_int;
    fn fluid_synth_pitch_bend(synth: *mut FluidSynthHandle, channel: c_int, value: c_int) -> c_int;
    fn fluid_synth_sysex(
        synth: *mut FluidSynthHandle,
        data: *const c_char,
        length: c_int,
        response: *mut c_char,
        response_length: *mut c_int,
        handled: *mut c_int,
        dry_run: c_int,
    ) -> c_int;
    fn fluid_synth_system_reset(synth: *mut FluidSynthHandle) -> c_int;
    fn fluid_synth_all_notes_off(synth: *mut FluidSynthHandle, channel: c_int) -> c_int;
    fn fluid_synth_write_s16(
        synth: *mut FluidSynthHandle,
        frames: c_int,
        left: *mut c_void,
        left_offset: c_int,
        left_stride: c_int,
        right: *mut c_void,
        right_offset: c_int,
        right_stride: c_int,
    ) -> c_int;
}

#[derive(Debug)]
pub struct FluidSynth {
    settings: NonNull<FluidSettings>,
    synth: NonNull<FluidSynthHandle>,
}

impl FluidSynth {
    pub fn new(soundfont: impl AsRef<Path>) -> Result<Self, Error> {
        let soundfont = path_string(soundfont.as_ref())?;
        // SAFETY: This constructor has no preconditions and its result is checked for null.
        let settings = NonNull::new(unsafe { new_fluid_settings() })
            .ok_or(Error::NativeAllocation("FluidSynth settings"))?;
        let sample_rate = c"synth.sample-rate";
        // SAFETY: `settings` is live and the setting name is a static NUL-terminated string.
        let code = unsafe {
            fluid_settings_setnum(
                settings.as_ptr(),
                sample_rate.as_ptr(),
                f64::from(SAMPLE_RATE_HZ),
            )
        };
        if code < 0 {
            // SAFETY: Ownership of `settings` has not been transferred to a synth.
            unsafe { delete_fluid_settings(settings.as_ptr()) };
            return Err(Error::NativeCall {
                operation: "fluid_settings_setnum",
                code,
            });
        }
        // SAFETY: `settings` remains live for the full lifetime of the returned synth.
        let synth = match NonNull::new(unsafe { new_fluid_synth(settings.as_ptr()) }) {
            Some(synth) => synth,
            None => {
                // SAFETY: Synth construction failed, so `settings` is still solely owned here.
                unsafe { delete_fluid_settings(settings.as_ptr()) };
                return Err(Error::NativeAllocation("FluidSynth"));
            }
        };
        let mut result = Self { settings, synth };
        // SAFETY: Both handles are live and `soundfont` remains NUL-terminated for the call.
        let code = unsafe { fluid_synth_sfload(result.synth.as_ptr(), soundfont.as_ptr(), 1) };
        if code < 0 {
            return Err(Error::NativeCall {
                operation: "fluid_synth_sfload",
                code,
            });
        }
        result.reset()?;
        Ok(result)
    }

    pub fn send(&mut self, message: &[u8]) -> Result<(), Error> {
        let code = match midi::validate(message)? {
            MidiMessage::Short { bytes, .. } => self.send_short(bytes),
            MidiMessage::SysEx(bytes) => {
                let body = &bytes[1..bytes.len() - 1];
                let length = c_int::try_from(body.len()).map_err(|_| Error::InvalidMidiMessage)?;
                // SAFETY: The synth is live and `body` is readable for `length` bytes.
                unsafe {
                    fluid_synth_sysex(
                        self.synth.as_ptr(),
                        body.as_ptr().cast(),
                        length,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        ptr::null_mut(),
                        0,
                    )
                }
            }
        };
        native_result("FluidSynth MIDI", code)
    }

    pub fn render_interleaved_i16(&mut self, output: &mut [i16]) -> Result<(), Error> {
        if output.len() % 2 != 0 {
            return Err(Error::OutputMustBeStereo);
        }
        let frames = c_int::try_from(output.len() / 2).map_err(|_| Error::TooManyFrames)?;
        if frames == 0 {
            return Ok(());
        }
        // SAFETY: `output` contains `frames * 2` writable samples. Left and right use
        // offsets 0 and 1 with stride 2, so both writes stay within that allocation.
        let code = unsafe {
            fluid_synth_write_s16(
                self.synth.as_ptr(),
                frames,
                output.as_mut_ptr().cast(),
                0,
                2,
                output.as_mut_ptr().cast(),
                1,
                2,
            )
        };
        native_result("fluid_synth_write_s16", code)
    }

    pub fn all_notes_off(&mut self) -> Result<(), Error> {
        for channel in 0..16 {
            // SAFETY: The synth is live and MIDI channel numbers 0 through 15 are valid.
            let code = unsafe { fluid_synth_all_notes_off(self.synth.as_ptr(), channel) };
            native_result("fluid_synth_all_notes_off", code)?;
        }
        Ok(())
    }

    pub fn reset(&mut self) -> Result<(), Error> {
        // SAFETY: The synth handle is live and uniquely accessed through `&mut self`.
        native_result("fluid_synth_system_reset", unsafe {
            fluid_synth_system_reset(self.synth.as_ptr())
        })
    }

    fn send_short(&mut self, bytes: [u8; 3]) -> c_int {
        let channel = c_int::from(bytes[0] & 0x0F);
        let first = c_int::from(bytes[1]);
        let second = c_int::from(bytes[2]);
        // SAFETY: MIDI validation checked the exact length and 7-bit data bytes. The synth
        // is live and each call receives the parameter range required by FluidSynth.
        unsafe {
            match bytes[0] & 0xF0 {
                0x80 => fluid_synth_noteoff(self.synth.as_ptr(), channel, first),
                0x90 => fluid_synth_noteon(self.synth.as_ptr(), channel, first, second),
                0xA0 => fluid_synth_key_pressure(self.synth.as_ptr(), channel, first, second),
                0xB0 => fluid_synth_cc(self.synth.as_ptr(), channel, first, second),
                0xC0 => fluid_synth_program_change(self.synth.as_ptr(), channel, first),
                0xD0 => fluid_synth_channel_pressure(self.synth.as_ptr(), channel, first),
                0xE0 => fluid_synth_pitch_bend(self.synth.as_ptr(), channel, first | (second << 7)),
                0xF0 if bytes[0] == 0xFF => fluid_synth_system_reset(self.synth.as_ptr()),
                0xF0 => 0,
                _ => unreachable!("validated MIDI status"),
            }
        }
    }
}

impl Drop for FluidSynth {
    fn drop(&mut self) {
        // SAFETY: These handles were created together, remain live, and are freed once in
        // the required order: synth first, then the settings it references.
        unsafe {
            delete_fluid_synth(self.synth.as_ptr());
            delete_fluid_settings(self.settings.as_ptr());
        }
    }
}

fn path_string(path: &Path) -> Result<CString, Error> {
    let value = path
        .to_str()
        .ok_or_else(|| Error::InvalidPath(path.to_path_buf()))?;
    CString::new(value).map_err(|_| Error::InvalidPath(path.to_path_buf()))
}

fn native_result(operation: &'static str, code: c_int) -> Result<(), Error> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::NativeCall { operation, code })
    }
}
