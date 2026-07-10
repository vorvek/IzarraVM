// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use crate::TimedMidiMessage;
use izarravm_core::{MidiBackend, MidiConfig, MidiPortId, MidiStatus};
use midir::{MidiOutput, MidiOutputConnection};

const ALL_NOTES_OFF: u8 = 123;

pub struct MidiEngine {
    connection: Option<MidiOutputConnection>,
    status: MidiStatus,
}

impl MidiEngine {
    pub fn open(config: &MidiConfig) -> Self {
        let mut engine = Self {
            connection: None,
            status: MidiStatus::Ready,
        };
        engine.configure(config);
        engine
    }

    pub const fn status(&self) -> MidiStatus {
        self.status
    }

    pub fn send(&mut self, message: &TimedMidiMessage) {
        if let Some(connection) = &mut self.connection
            && connection.send(&message.bytes).is_err()
        {
            self.status = MidiStatus::InitializationFailed;
        }
    }

    pub fn reconfigure(&mut self, config: &MidiConfig) {
        self.close();
        self.configure(config);
    }

    fn configure(&mut self, config: &MidiConfig) {
        self.status = match config.backend {
            MidiBackend::Off => MidiStatus::Ready,
            MidiBackend::External => self.open_external(config.external_port.as_ref()),
            MidiBackend::FluidSynth => {
                if config
                    .soundfont
                    .as_ref()
                    .is_some_and(|path| !path.is_file())
                {
                    MidiStatus::MissingSoundFont
                } else {
                    MidiStatus::InitializationFailed
                }
            }
            MidiBackend::Munt => {
                if config
                    .mt32_control_rom
                    .as_ref()
                    .is_none_or(|path| !path.is_file())
                    || config
                        .mt32_pcm_rom
                        .as_ref()
                        .is_none_or(|path| !path.is_file())
                {
                    MidiStatus::MissingRoms
                } else {
                    MidiStatus::InitializationFailed
                }
            }
        };
    }

    fn open_external(&mut self, requested: Option<&MidiPortId>) -> MidiStatus {
        let Some(requested) = requested else {
            return MidiStatus::MissingPort;
        };
        let Ok(output) = MidiOutput::new("IzarraVM") else {
            return MidiStatus::InitializationFailed;
        };
        let ports = output.ports();
        let names: Vec<_> = ports
            .iter()
            .map(|port| output.port_name(port).ok())
            .collect();
        let Some(index) = matching_port_index(names.iter().map(|name| name.as_deref()), requested)
        else {
            return MidiStatus::MissingPort;
        };
        match output.connect(&ports[index], "IzarraVM wavetable") {
            Ok(connection) => {
                self.connection = Some(connection);
                MidiStatus::Ready
            }
            Err(_) => MidiStatus::InitializationFailed,
        }
    }

    fn close(&mut self) {
        if let Some(connection) = &mut self.connection {
            for message in all_notes_off_messages() {
                let _ = connection.send(&message);
            }
        }
        self.connection = None;
    }
}

impl Drop for MidiEngine {
    fn drop(&mut self) {
        self.close();
    }
}

fn matching_port_index<'a>(
    names: impl IntoIterator<Item = Option<&'a str>>,
    requested: &MidiPortId,
) -> Option<usize> {
    names
        .into_iter()
        .enumerate()
        .filter(|(_, name)| name.is_some_and(|name| name == requested.name))
        .nth(usize::from(requested.ordinal))
        .map(|(index, _)| index)
}

fn all_notes_off_messages() -> impl Iterator<Item = [u8; 3]> {
    (0..16).map(|channel| [0xb0 | channel, ALL_NOTES_OFF, 0])
}

#[cfg(test)]
#[path = "midi_test.rs"]
mod tests;
