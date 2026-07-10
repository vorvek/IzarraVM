// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use crate::Error;

#[derive(Debug)]
pub struct FluidSynth;

impl FluidSynth {
    pub fn new(_soundfont: impl AsRef<Path>) -> Result<Self, Error> {
        Err(Error::Unavailable)
    }

    pub fn send(&mut self, _message: &[u8]) -> Result<(), Error> {
        Err(Error::Unavailable)
    }

    pub fn render_interleaved_i16(&mut self, _output: &mut [i16]) -> Result<(), Error> {
        Err(Error::Unavailable)
    }

    pub fn all_notes_off(&mut self) -> Result<(), Error> {
        Err(Error::Unavailable)
    }

    pub fn reset(&mut self) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
}

#[derive(Debug)]
pub struct MuntSynth;

impl MuntSynth {
    pub fn new(_control_rom: impl AsRef<Path>, _pcm_rom: impl AsRef<Path>) -> Result<Self, Error> {
        Err(Error::Unavailable)
    }

    pub fn send(&mut self, _message: &[u8]) -> Result<(), Error> {
        Err(Error::Unavailable)
    }

    pub fn render_interleaved_i16(&mut self, _output: &mut [i16]) -> Result<(), Error> {
        Err(Error::Unavailable)
    }

    pub fn all_notes_off(&mut self) -> Result<(), Error> {
        Err(Error::Unavailable)
    }

    pub fn reset(&mut self) -> Result<(), Error> {
        Err(Error::Unavailable)
    }
}
