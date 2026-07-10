// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_core::MASTER_CLOCK_HZ;
use std::collections::VecDeque;

const ACK: u8 = 0xfe;
const RESET: u8 = 0xff;
const ENTER_UART: u8 = 0x3f;
const REQUEST_VERSION: u8 = 0xac;
const REQUEST_REVISION: u8 = 0xad;
const REQUEST_TEMPO: u8 = 0xaf;
const RX_EMPTY: u8 = 0x80;
const INPUT_CAPACITY: usize = 4_096;
const OUTPUT_CAPACITY: usize = 1_024;
const SYSEX_CAPACITY: usize = 65_536;
const CLOCK_NUMERATOR: u128 = MASTER_CLOCK_HZ as u128 * 60;
const IMMEDIATE_DELAY_TICKS: u64 = MASTER_CLOCK_HZ * 60 / 1_000_000;
const TIMEBASES: [u16; 7] = [48, 72, 96, 120, 144, 168, 192];
const CONDUCTOR_REQUEST_BIT: u16 = 1 << 9;
const END_REQUEST_BIT: u16 = 1 << 12;

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
    ActiveTracks,
    Discard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestTarget {
    Track(u8),
    Conductor,
}

#[derive(Debug)]
struct RequestInput {
    target: RequestTarget,
    timing: Option<u8>,
    bytes: Vec<u8>,
    expected: usize,
}

impl RequestInput {
    fn new(target: RequestTarget) -> Self {
        Self {
            target,
            timing: None,
            bytes: Vec::new(),
            expected: 0,
        }
    }
}

#[derive(Debug)]
enum TrackEvent {
    Midi(Vec<u8>),
    Mark(u8),
}

#[derive(Debug)]
struct ConductorEvent {
    command: u8,
    parameter: Option<u8>,
}

#[derive(Debug)]
enum ImmediateAction {
    Track { track: u8, event: TrackEvent },
    Conductor(ConductorEvent),
}

#[derive(Debug)]
struct ImmediateEvent {
    due_tick: u64,
    action: ImmediateAction,
}

#[derive(Debug, Default)]
struct TrackState {
    counter: u8,
    event: Option<TrackEvent>,
    running_status: Option<u8>,
}

#[derive(Debug, Default)]
struct ConductorState {
    counter: u8,
    event: Option<ConductorEvent>,
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
    configured_tracks: u8,
    active_tracks: u8,
    conductor_enabled: bool,
    conductor_active: bool,
    playing: bool,
    tracks: [TrackState; 8],
    conductor: ConductorState,
    pending_requests: u16,
    request: Option<RequestInput>,
    immediate: Option<ImmediateEvent>,
    now_tick: u64,
    clock_phase: u128,
}

impl Default for Mpu401 {
    fn default() -> Self {
        Self {
            mode: MpuMode::Intelligent,
            input: VecDeque::with_capacity(INPUT_CAPACITY),
            output: VecDeque::with_capacity(OUTPUT_CAPACITY),
            parser: MidiParser::default(),
            timebase: 120,
            tempo: 100,
            pending_parameter: None,
            configured_tracks: 0,
            active_tracks: 0,
            conductor_enabled: false,
            conductor_active: false,
            playing: false,
            tracks: std::array::from_fn(|_| TrackState::default()),
            conductor: ConductorState::default(),
            pending_requests: 0,
            request: None,
            immediate: None,
            now_tick: 0,
            clock_phase: 0,
        }
    }
}

impl Mpu401 {
    /// Read the command-port status. The transmitter is never busy, so only the
    /// active-low receive-ready bit changes.
    pub fn status(&self) -> u8 {
        if self.input.is_empty() { RX_EMPTY } else { 0 }
    }

    pub fn status_at(&mut self, guest_tick: u64) -> u8 {
        self.advance_to(guest_tick);
        self.status()
    }

    pub fn read_data(&mut self) -> u8 {
        self.read_data_at(self.now_tick)
    }

    pub fn read_data_at(&mut self, guest_tick: u64) -> u8 {
        self.advance_to(guest_tick);
        let Some(value) = self.input.pop_front() else {
            return 0xff;
        };
        match value {
            0xf0..=0xf7 => self.request = Some(RequestInput::new(RequestTarget::Track(value & 7))),
            0xf9 => self.request = Some(RequestInput::new(RequestTarget::Conductor)),
            _ => {}
        }
        if self.input.is_empty() && self.request.is_none() {
            self.issue_next_request();
        }
        value
    }

    pub fn write_command(&mut self, command: u8) {
        self.write_command_at(command, self.now_tick);
    }

    pub fn write_command_at(&mut self, command: u8, guest_tick: u64) {
        self.advance_to(guest_tick);
        self.pending_parameter = None;
        self.write_command_inner(command, true, guest_tick);
    }

    fn write_command_inner(&mut self, command: u8, acknowledge: bool, guest_tick: u64) {
        if self.mode == MpuMode::Uart && command != RESET {
            return;
        }

        if command <= 0x2f {
            if command & 0x0f < 0x0c {
                match command & 3 {
                    1 => self.queue_output(guest_tick, vec![0xfc]),
                    2 => self.queue_output(guest_tick, vec![0xfb]),
                    3 => self.queue_output(guest_tick, vec![0xfa]),
                    _ => {}
                }
                match command & 0x0c {
                    0x04 => self.stop_playback(guest_tick),
                    0x08 => self.start_playback(),
                    _ => {}
                }
            }
            if acknowledge {
                self.queue_response(&[ACK]);
            }
            return;
        }

        match command {
            ENTER_UART => {
                self.mode = MpuMode::Uart;
                self.parser.reset();
                self.pending_parameter = None;
            }
            0x8e | 0x8f => self.conductor_enabled = command & 1 != 0,
            REQUEST_VERSION if acknowledge => {
                self.queue_response(&[ACK, 0x15]);
                return;
            }
            REQUEST_REVISION if acknowledge => {
                self.queue_response(&[ACK, 0x01]);
                return;
            }
            REQUEST_TEMPO if acknowledge => {
                self.queue_response(&[ACK, self.tempo]);
                return;
            }
            0xb8 => self.clear_play_counters(),
            0xb9 => self.clear_play_map(guest_tick),
            0xc2..=0xc8 => {
                self.timebase = TIMEBASES[usize::from(command - 0xc2)];
                self.restart_clock();
            }
            0xe0 => self.pending_parameter = Some(PendingParameter::Tempo),
            0xec => self.pending_parameter = Some(PendingParameter::ActiveTracks),
            0xe1 | 0xe2 | 0xe4 | 0xe6 | 0xe7 | 0xed | 0xee | 0xef => {
                self.pending_parameter = Some(PendingParameter::Discard);
            }
            RESET => {
                self.reset_protocol();
                if acknowledge {
                    self.queue_response(&[ACK]);
                }
                return;
            }
            _ => {}
        }
        if acknowledge {
            self.queue_response(&[ACK]);
        }
    }

    pub fn write_data(&mut self, value: u8, guest_tick: u64) {
        self.advance_to(guest_tick);
        if let Some(parameter) = self.pending_parameter.take() {
            self.apply_parameter(parameter, value);
            return;
        }
        if self.request.is_some() {
            self.feed_request(value, guest_tick);
            return;
        }

        let mut completed = Vec::new();
        self.parser.push(value, &mut completed);
        for bytes in completed {
            self.queue_output(guest_tick, bytes);
        }
    }

    pub fn advance_to(&mut self, target_tick: u64) {
        while self.now_tick < target_tick {
            let pulse_ticks = self.playing.then(|| self.ticks_until_clock_pulse());
            let immediate_ticks = self
                .immediate
                .as_ref()
                .map(|event| event.due_tick.saturating_sub(self.now_tick).max(1));
            let Some(step) = pulse_ticks.into_iter().chain(immediate_ticks).min() else {
                self.now_tick = target_tick;
                return;
            };
            let step = step.min(target_tick - self.now_tick);
            if self.playing {
                self.clock_phase += u128::from(step) * self.clock_rate();
            }
            self.now_tick += step;

            let pulse_due = self.playing && self.clock_phase >= CLOCK_NUMERATOR;
            if pulse_due {
                self.clock_phase -= CLOCK_NUMERATOR;
                self.clock_tick();
            }
            if self
                .immediate
                .as_ref()
                .is_some_and(|event| event.due_tick <= self.now_tick)
            {
                let event = self.immediate.take().expect("due MPU event exists");
                self.execute_immediate(event.action);
            }
        }
    }

    pub fn ticks_until_event(&self) -> Option<u64> {
        let immediate = self
            .immediate
            .as_ref()
            .map(|event| event.due_tick.saturating_sub(self.now_tick).max(1));
        let clock = (self.playing
            && self.input.is_empty()
            && (self.active_tracks != 0 || self.conductor_active))
            .then(|| self.ticks_until_clock_pulse());
        immediate.into_iter().chain(clock).min()
    }

    pub fn irq_level(&self) -> bool {
        !self.input.is_empty()
    }

    pub fn take_message(&mut self) -> Option<TimedMidiMessage> {
        self.output.pop_front()
    }

    pub fn is_uart(&self) -> bool {
        self.mode == MpuMode::Uart
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn timebase(&self) -> u16 {
        self.timebase
    }

    pub fn tempo(&self) -> u8 {
        self.tempo
    }

    fn apply_parameter(&mut self, parameter: PendingParameter, value: u8) {
        match parameter {
            PendingParameter::Tempo => {
                self.tempo = value.clamp(8, 250);
                self.restart_clock();
            }
            PendingParameter::ActiveTracks => self.configured_tracks = value,
            PendingParameter::Discard => {}
        }
    }

    fn start_playback(&mut self) {
        if !self.playing {
            self.clock_phase = 0;
        }
        self.playing = true;
        self.input.clear();
        self.request = None;
    }

    fn stop_playback(&mut self, guest_tick: u64) {
        self.playing = false;
        self.pending_requests = 0;
        self.request = None;
        self.immediate = None;
        for channel in 0..16 {
            self.queue_output(guest_tick, vec![0xb0 | channel, 123, 0]);
        }
    }

    fn clear_play_counters(&mut self) {
        self.active_tracks = self.configured_tracks;
        self.conductor_active = self.conductor_enabled;
        self.pending_requests = 0;
        self.request = None;
        self.immediate = None;
        for track in &mut self.tracks {
            track.counter = 0;
            track.event = None;
        }
        self.conductor.counter = 0;
        self.conductor.event = None;
    }

    fn clear_play_map(&mut self, guest_tick: u64) {
        self.clear_play_counters();
        for track in &mut self.tracks {
            track.running_status = None;
        }
        for channel in 0..16 {
            self.queue_output(guest_tick, vec![0xb0 | channel, 123, 0]);
        }
    }

    fn reset_protocol(&mut self) {
        self.mode = MpuMode::Intelligent;
        self.input.clear();
        self.output.clear();
        self.parser.reset();
        self.timebase = 120;
        self.tempo = 100;
        self.pending_parameter = None;
        self.configured_tracks = 0;
        self.active_tracks = 0;
        self.conductor_enabled = false;
        self.conductor_active = false;
        self.playing = false;
        self.tracks = std::array::from_fn(|_| TrackState::default());
        self.conductor = ConductorState::default();
        self.pending_requests = 0;
        self.request = None;
        self.immediate = None;
        self.clock_phase = 0;
    }

    fn restart_clock(&mut self) {
        if self.playing {
            self.clock_phase = 0;
        }
    }

    fn feed_request(&mut self, value: u8, guest_tick: u64) {
        let mut request = self.request.take().expect("MPU request input exists");
        if request.timing.is_none() {
            if value >= 0xf0 {
                self.issue_next_request();
                return;
            }
            request.timing = Some(value);
            self.request = Some(request);
            return;
        }

        match request.target {
            RequestTarget::Track(track) => {
                self.feed_track_request(request, track, value, guest_tick)
            }
            RequestTarget::Conductor => self.feed_conductor_request(request, value, guest_tick),
        }
    }

    fn feed_track_request(
        &mut self,
        mut request: RequestInput,
        track: u8,
        value: u8,
        guest_tick: u64,
    ) {
        if request.bytes.is_empty() {
            match value {
                0x80..=0xef => {
                    self.tracks[usize::from(track)].running_status = Some(value);
                    request.bytes.push(value);
                    request.expected = message_len(value);
                }
                0xf8 | 0xf9 | 0xfc => {
                    request.bytes.push(value);
                    request.expected = 1;
                }
                0x00..=0x7f => {
                    let Some(status) = self.tracks[usize::from(track)].running_status else {
                        self.issue_next_request();
                        return;
                    };
                    request.bytes.extend([status, value]);
                    request.expected = message_len(status);
                }
                _ => {
                    self.issue_next_request();
                    return;
                }
            }
        } else {
            request.bytes.push(value);
        }

        if request.bytes.len() < request.expected {
            self.request = Some(request);
            return;
        }
        let timing = request.timing.expect("track timing byte exists");
        let event = if request.bytes.len() == 1 {
            TrackEvent::Mark(request.bytes[0])
        } else {
            TrackEvent::Midi(request.bytes)
        };
        self.schedule_track(track, timing, event, guest_tick);
    }

    fn feed_conductor_request(&mut self, mut request: RequestInput, value: u8, guest_tick: u64) {
        request.bytes.push(value);
        if request.bytes.len() == 1 {
            request.expected = if command_has_parameter(value) { 2 } else { 1 };
        }
        if request.bytes.len() < request.expected {
            self.request = Some(request);
            return;
        }
        let timing = request.timing.expect("conductor timing byte exists");
        let event = ConductorEvent {
            command: request.bytes[0],
            parameter: request.bytes.get(1).copied(),
        };
        self.schedule_conductor(timing, event, guest_tick);
    }

    fn schedule_track(&mut self, track: u8, timing: u8, event: TrackEvent, guest_tick: u64) {
        if timing == 0 {
            self.immediate = Some(ImmediateEvent {
                due_tick: guest_tick.saturating_add(IMMEDIATE_DELAY_TICKS),
                action: ImmediateAction::Track { track, event },
            });
        } else {
            let state = &mut self.tracks[usize::from(track)];
            state.counter = timing;
            state.event = Some(event);
            self.issue_next_request();
        }
    }

    fn schedule_conductor(&mut self, timing: u8, event: ConductorEvent, guest_tick: u64) {
        if timing == 0 {
            self.immediate = Some(ImmediateEvent {
                due_tick: guest_tick.saturating_add(IMMEDIATE_DELAY_TICKS),
                action: ImmediateAction::Conductor(event),
            });
        } else {
            self.conductor.counter = timing;
            self.conductor.event = Some(event);
            self.issue_next_request();
        }
    }

    fn execute_immediate(&mut self, action: ImmediateAction) {
        match action {
            ImmediateAction::Track { track, event } => self.finish_track_event(track, event),
            ImmediateAction::Conductor(event) => self.finish_conductor_event(event),
        }
        self.issue_next_request();
    }

    fn clock_tick(&mut self) {
        if !self.input.is_empty() {
            return;
        }
        for track in 0..8u8 {
            if self.active_tracks & (1 << track) == 0 {
                continue;
            }
            let index = usize::from(track);
            let due = if self.tracks[index].event.is_some() {
                self.tracks[index].counter = self.tracks[index].counter.saturating_sub(1);
                (self.tracks[index].counter == 0).then(|| {
                    self.tracks[index]
                        .event
                        .take()
                        .expect("due track event exists")
                })
            } else {
                if !self.track_request_outstanding(track) {
                    self.pending_requests |= 1 << track;
                }
                None
            };
            if let Some(event) = due {
                self.finish_track_event(track, event);
            }
        }

        if self.conductor_active {
            if self.conductor.event.is_some() {
                self.conductor.counter = self.conductor.counter.saturating_sub(1);
                if self.conductor.counter == 0 {
                    let event = self
                        .conductor
                        .event
                        .take()
                        .expect("due conductor event exists");
                    self.finish_conductor_event(event);
                }
            } else if !self.conductor_request_outstanding() {
                self.pending_requests |= CONDUCTOR_REQUEST_BIT;
            }
        }
        self.issue_next_request();
    }

    fn finish_track_event(&mut self, track: u8, event: TrackEvent) {
        match event {
            TrackEvent::Midi(bytes) => self.queue_output(self.now_tick, bytes),
            TrackEvent::Mark(0xfc) => {
                self.queue_output(self.now_tick, vec![0xfc]);
                self.active_tracks &= !(1 << track);
            }
            TrackEvent::Mark(_) => {}
        }
        if self.active_tracks & (1 << track) != 0 {
            self.pending_requests |= 1 << track;
        } else if self.active_tracks == 0 && !self.conductor_active {
            self.pending_requests |= END_REQUEST_BIT;
        }
    }

    fn finish_conductor_event(&mut self, event: ConductorEvent) {
        self.pending_parameter = None;
        self.write_command_inner(event.command, false, self.now_tick);
        if let Some(value) = event.parameter
            && let Some(parameter) = self.pending_parameter.take()
        {
            self.apply_parameter(parameter, value);
        }
        if self.playing && self.conductor_active {
            self.pending_requests |= CONDUCTOR_REQUEST_BIT;
        }
    }

    fn issue_next_request(&mut self) {
        if !self.playing
            || !self.input.is_empty()
            || self.request.is_some()
            || self.immediate.is_some()
        {
            return;
        }
        let Some(bit) = (0..=12).find(|bit| self.pending_requests & (1 << bit) != 0) else {
            return;
        };
        self.pending_requests &= !(1 << bit);
        self.queue_response(&[0xf0 + bit as u8]);
    }

    fn track_request_outstanding(&self, track: u8) -> bool {
        self.pending_requests & (1 << track) != 0
            || self
                .request
                .as_ref()
                .is_some_and(|request| request.target == RequestTarget::Track(track))
            || self.immediate.as_ref().is_some_and(|event| {
                matches!(event.action, ImmediateAction::Track { track: pending, .. } if pending == track)
            })
    }

    fn conductor_request_outstanding(&self) -> bool {
        self.pending_requests & CONDUCTOR_REQUEST_BIT != 0
            || self
                .request
                .as_ref()
                .is_some_and(|request| request.target == RequestTarget::Conductor)
            || self
                .immediate
                .as_ref()
                .is_some_and(|event| matches!(event.action, ImmediateAction::Conductor(_)))
    }

    fn clock_rate(&self) -> u128 {
        u128::from(self.timebase) * u128::from(self.tempo)
    }

    fn ticks_until_clock_pulse(&self) -> u64 {
        let remaining = CLOCK_NUMERATOR.saturating_sub(self.clock_phase);
        u64::try_from(remaining.div_ceil(self.clock_rate()))
            .unwrap_or(u64::MAX)
            .max(1)
    }

    fn queue_response(&mut self, bytes: &[u8]) {
        while self.input.len() + bytes.len() > INPUT_CAPACITY {
            self.input.pop_back();
        }
        self.input.extend(bytes.iter().copied());
    }

    fn queue_output(&mut self, guest_tick: u64, bytes: Vec<u8>) {
        if self.output.len() == OUTPUT_CAPACITY {
            self.output.pop_front();
        }
        self.output
            .push_back(TimedMidiMessage { guest_tick, bytes });
    }
}

fn command_has_parameter(command: u8) -> bool {
    matches!(
        command,
        0xe0 | 0xe1 | 0xe2 | 0xe4 | 0xe6 | 0xe7 | 0xec | 0xed | 0xee | 0xef
    )
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
