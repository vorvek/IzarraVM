// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::VecDeque;

const ACK: u8 = 0xfe;
const RESET: u8 = 0xff;
const ENTER_UART: u8 = 0x3f;
const REQUEST_VERSION: u8 = 0xac;
const REQUEST_REVISION: u8 = 0xad;
const RX_EMPTY: u8 = 0x80;
const INPUT_CAPACITY: usize = 4_096;
const OUTPUT_CAPACITY: usize = 1_024;
const SYSEX_CAPACITY: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedMidiMessage {
    /// Fixed machine-timeline tick at which the message completed.
    pub guest_tick: u64,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MpuMode {
    Intelligent,
    Uart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingParameter {
    Tempo,
}

#[derive(Debug)]
pub struct Mpu401 {
    mode: MpuMode,
    input: VecDeque<u8>,
    output: VecDeque<TimedMidiMessage>,
    parser: MidiParser,
    timebase: u16,
    tempo: u8,
    pending_parameter: Option<PendingParameter>,
}

impl Default for Mpu401 {
    fn default() -> Self {
        Self {
            mode: MpuMode::Intelligent,
            input: VecDeque::with_capacity(INPUT_CAPACITY),
            output: VecDeque::with_capacity(OUTPUT_CAPACITY),
            parser: MidiParser::default(),
            timebase: 120,
            tempo: 120,
            pending_parameter: None,
        }
    }
}

impl Mpu401 {
    /// Read the command-port status. The transmitter is never busy, so only the
    /// active-low receive-ready bit changes.
    pub fn status(&self) -> u8 {
        if self.input.is_empty() { RX_EMPTY } else { 0 }
    }

    pub fn read_data(&mut self) -> u8 {
        self.input.pop_front().unwrap_or(0xff)
    }

    pub fn write_command(&mut self, command: u8) {
        match command {
            RESET => {
                self.mode = MpuMode::Intelligent;
                self.input.clear();
                self.parser.reset();
                self.timebase = 120;
                self.tempo = 120;
                self.pending_parameter = None;
                self.queue_response(&[ACK]);
            }
            ENTER_UART => {
                self.mode = MpuMode::Uart;
                self.parser.reset();
                self.pending_parameter = None;
                self.queue_response(&[ACK]);
            }
            REQUEST_VERSION => {
                self.queue_response(&[ACK, 0x15]);
            }
            REQUEST_REVISION => {
                self.queue_response(&[ACK, 0x01]);
            }
            0xc2..=0xc8 if self.mode == MpuMode::Intelligent => {
                self.timebase = u16::from(command & 0x0f) * 24;
                self.queue_response(&[ACK]);
            }
            0xe0 if self.mode == MpuMode::Intelligent => {
                self.pending_parameter = Some(PendingParameter::Tempo);
                self.queue_response(&[ACK]);
            }
            _ => {}
        }
    }

    pub fn write_data(&mut self, value: u8, guest_tick: u64) {
        if let Some(parameter) = self.pending_parameter.take() {
            match parameter {
                PendingParameter::Tempo => self.tempo = value.clamp(8, 250),
            }
            return;
        }

        let mut completed = Vec::new();
        self.parser.push(value, &mut completed);
        for bytes in completed {
            if self.output.len() == OUTPUT_CAPACITY {
                self.output.pop_front();
            }
            self.output
                .push_back(TimedMidiMessage { guest_tick, bytes });
        }
    }

    pub fn take_message(&mut self) -> Option<TimedMidiMessage> {
        self.output.pop_front()
    }

    pub fn is_uart(&self) -> bool {
        self.mode == MpuMode::Uart
    }

    pub fn timebase(&self) -> u16 {
        self.timebase
    }

    pub fn tempo(&self) -> u8 {
        self.tempo
    }

    fn queue_response(&mut self, bytes: &[u8]) {
        while self.input.len() + bytes.len() > INPUT_CAPACITY {
            self.input.pop_back();
        }
        self.input.extend(bytes.iter().copied());
    }
}

#[derive(Debug, Default)]
struct MidiParser {
    running_status: Option<u8>,
    message: Vec<u8>,
    expected_len: usize,
    sysex: Option<Vec<u8>>,
    sysex_overflow: bool,
}

impl MidiParser {
    fn reset(&mut self) {
        self.running_status = None;
        self.message.clear();
        self.expected_len = 0;
        self.sysex = None;
        self.sysex_overflow = false;
    }

    fn push(&mut self, value: u8, completed: &mut Vec<Vec<u8>>) {
        if value >= 0xf8 {
            completed.push(vec![value]);
            return;
        }

        if self.sysex.is_some() {
            if value == 0xf7 {
                if !self.sysex_overflow {
                    let mut message = self.sysex.take().expect("SysEx is active");
                    message.push(value);
                    completed.push(message);
                } else {
                    self.sysex = None;
                }
                self.sysex_overflow = false;
                return;
            }
            if value < 0x80 {
                let message = self.sysex.as_mut().expect("SysEx is active");
                if message.len() + 1 < SYSEX_CAPACITY {
                    message.push(value);
                } else {
                    self.sysex_overflow = true;
                }
                return;
            }
            self.sysex = None;
            self.sysex_overflow = false;
        }

        if value >= 0x80 {
            self.start_status(value, completed);
            return;
        }

        if self.expected_len == 0 {
            let Some(status) = self.running_status else {
                return;
            };
            self.message.clear();
            self.message.push(status);
            self.expected_len = message_len(status);
        }

        self.message.push(value);
        if self.message.len() == self.expected_len {
            completed.push(self.message.clone());
            if let Some(status) = self.running_status {
                self.message.clear();
                self.message.push(status);
                self.expected_len = message_len(status);
            } else {
                self.message.clear();
                self.expected_len = 0;
            }
        }
    }

    fn start_status(&mut self, status: u8, completed: &mut Vec<Vec<u8>>) {
        self.message.clear();
        self.expected_len = 0;
        match status {
            0x80..=0xef => {
                self.running_status = Some(status);
                self.message.push(status);
                self.expected_len = message_len(status);
            }
            0xf0 => {
                self.running_status = None;
                self.sysex = Some(vec![status]);
            }
            0xf1..=0xf3 => {
                self.running_status = None;
                self.message.push(status);
                self.expected_len = message_len(status);
            }
            0xf4..=0xf7 => {
                self.running_status = None;
                completed.push(vec![status]);
            }
            _ => unreachable!("real-time messages return before status parsing"),
        }
    }
}

fn message_len(status: u8) -> usize {
    match status {
        0xc0..=0xdf | 0xf1 | 0xf3 => 2,
        0x80..=0xbf | 0xe0..=0xef | 0xf2 => 3,
        _ => 1,
    }
}

#[cfg(test)]
#[path = "mpu401_test.rs"]
mod tests;
