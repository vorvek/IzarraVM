// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![deny(unsafe_op_in_unsafe_fn)]

use std::fmt;
use std::path::PathBuf;

#[cfg(not(izarravm_native_synth_unavailable))]
mod fluidsynth;
#[cfg(not(izarravm_native_synth_unavailable))]
mod midi;
#[cfg(not(izarravm_native_synth_unavailable))]
mod munt;
#[cfg(izarravm_native_synth_unavailable)]
mod unavailable;

#[cfg(not(izarravm_native_synth_unavailable))]
pub use fluidsynth::FluidSynth;
#[cfg(not(izarravm_native_synth_unavailable))]
pub use munt::MuntSynth;
#[cfg(izarravm_native_synth_unavailable)]
pub use unavailable::{FluidSynth, MuntSynth};

pub const SAMPLE_RATE_HZ: u32 = 44_100;
pub const NATIVE_SYNTH_AVAILABLE: bool = !cfg!(izarravm_native_synth_unavailable);

/// Which of the two ROM images a Roland synthesiser needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RomKind {
    Control,
    Pcm,
}

impl RomKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Pcm => "PCM",
        }
    }
}

impl fmt::Display for RomKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    InvalidPath(PathBuf),
    NativeAllocation(&'static str),
    NativeCall {
        operation: &'static str,
        code: i32,
    },
    InvalidMidiMessage,
    OutputMustBeStereo,
    TooManyFrames,
    MissingRom(PathBuf),
    InvalidRom(PathBuf),
    /// Files were found and offered to the library, but none of them was a
    /// usable image of this kind. `searched` is every file that was tried, so
    /// the message can say what was looked at rather than only what was wanted.
    RomNotFound {
        kind: RomKind,
        searched: Vec<PathBuf>,
    },
    MissingRoms,
    Unavailable,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath(path) => {
                write!(
                    formatter,
                    "path cannot be passed to the native library: {}",
                    path.display()
                )
            }
            Self::NativeAllocation(name) => write!(formatter, "native allocation failed: {name}"),
            Self::NativeCall { operation, code } => {
                write!(formatter, "native call {operation} failed with code {code}")
            }
            Self::InvalidMidiMessage => formatter.write_str("invalid complete MIDI message"),
            Self::OutputMustBeStereo => {
                formatter.write_str("output buffer must contain complete stereo frames")
            }
            Self::TooManyFrames => formatter.write_str("output buffer is too large"),
            Self::MissingRom(path) => write!(formatter, "ROM file is missing: {}", path.display()),
            Self::InvalidRom(path) => {
                write!(formatter, "ROM file is not recognized: {}", path.display())
            }
            Self::RomNotFound { kind, searched } => {
                write!(
                    formatter,
                    "no {kind} ROM image was recognised among the {} file(s) tried: {}",
                    searched.len(),
                    searched
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            Self::MissingRoms => {
                formatter.write_str("a matching control and PCM ROM pair is required")
            }
            Self::Unavailable => {
                formatter.write_str("native synthesis is unavailable on this host or target")
            }
        }
    }
}

impl std::error::Error for Error {}
