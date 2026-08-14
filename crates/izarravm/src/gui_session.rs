// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use izarravm_audio::{AudioDebugSnapshot, AudioSink, MidiEngine};
use izarravm_core::{GswMode, MASTER_CLOCK_HZ, MidiConfig, MidiStatus};
use izarravm_machine::{
    CdAudioState, CdImage, CueSource, JoystickState, Machine, MachineProfile, StopReason,
};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::error::Error;
use std::fmt;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tracing::{error, warn};

const OPL_NATIVE_HZ: f64 = 49_716.0;
const EMU_SLICE: Duration = Duration::from_millis(1);
const FAST_EMU_QUANTUM_TICKS: u64 = MASTER_CLOCK_HZ / 1000;
const SPEED_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const RUNTIME_PROFILE_INTERVAL: Duration = Duration::from_secs(1);
const RUNTIME_PROFILE_SCHEMA: &str = "izarravm.runtime.v1";

#[derive(Clone)]
pub(super) struct SharedGain(Arc<AtomicU32>);

impl SharedGain {
    pub(super) fn new(gain: f32) -> Self {
        Self(Arc::new(AtomicU32::new(gain.to_bits())))
    }

    pub(super) fn set(&self, gain: f32) {
        self.0.store(gain.to_bits(), Ordering::Relaxed);
    }

    fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

#[derive(Clone)]
pub(super) struct SessionSpec {
    pub(super) profile: MachineProfile,
    pub(super) rom: Vec<u8>,
    pub(super) c_drive: PathBuf,
    pub(super) midi_config: MidiConfig,
    pub(super) glide_ovl: Option<Vec<u8>>,
    pub(super) test_pattern: bool,
    pub(super) sink: Option<AudioSink>,
    pub(super) rtc_setup: crate::cmos::RtcSetup,
    /// The host playback level (the volume knob). Applied to the finished mix
    /// on its way to the sound device, never inside the machine's chain.
    pub(super) gain: SharedGain,
    #[cfg(test)]
    pub(super) finalization_probe: Option<Arc<AtomicU64>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FloppySource(pub(super) PathBuf);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CdSource {
    Image(PathBuf),
    Folder(PathBuf),
}

#[derive(Debug, Clone)]
pub(super) struct PreparedFloppy {
    pub(super) label: String,
    pub(super) source: FloppySource,
    bytes: Vec<u8>,
    writeback_pending: bool,
}

impl PreparedFloppy {
    pub(super) fn new(
        label: String,
        source: FloppySource,
        bytes: Vec<u8>,
    ) -> Result<Self, SessionFailure> {
        if !matches!(
            bytes.len(),
            163_840 | 184_320 | 327_680 | 368_640 | 737_280 | 1_228_800 | 1_474_560
        ) {
            return Err(SessionFailure::new(format!(
                "unsupported floppy image size: {} bytes",
                bytes.len()
            )));
        }
        Ok(Self {
            label,
            source,
            bytes,
            writeback_pending: false,
        })
    }

    pub(super) fn from_source(source: FloppySource) -> Result<Self, SessionFailure> {
        let bytes = std::fs::read(&source.0).map_err(|err| {
            SessionFailure::new(format!(
                "could not read floppy image {}: {err}",
                source.0.display()
            ))
        })?;
        let label = path_label(&source.0);
        Self::new(label, source, bytes)
    }
}

#[derive(Debug, Clone)]
pub(super) struct PreparedCd {
    pub(super) label: String,
    pub(super) source: CdSource,
    image: CdImage,
}

impl PreparedCd {
    pub(super) fn new(label: String, source: CdSource, image: CdImage) -> Self {
        Self {
            label,
            source,
            image,
        }
    }

    pub(super) fn from_source(source: CdSource) -> Result<Self, SessionFailure> {
        let (label, image) = prepare_cd_source(&source)?;
        Ok(Self::new(label, source, image))
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct PreparedInitialMedia {
    pub(super) floppy: Option<PreparedFloppy>,
    pub(super) cd: Option<PreparedCd>,
}

#[derive(Debug, Clone)]
pub(super) enum GuestInput {
    Keys(Vec<u8>),
    MouseRelative(i32, i32, u8),
    MouseWheel(i32),
    Joystick(Option<JoystickState>),
}

#[derive(Debug, Clone)]
pub(super) enum SessionRequest {
    MountFloppy(PreparedFloppy),
    EjectFloppy,
    MountCd(PreparedCd),
    EjectCd,
    CdPlay,
    CdPause,
    CdStop,
    CdNextTrack,
    CdLinkedLevel(u8),
    MidiConfig(MidiConfig),
}

impl SessionRequest {
    pub(super) fn kind(&self) -> SessionRequestKind {
        match self {
            Self::MountFloppy(_) => SessionRequestKind::MountFloppy,
            Self::EjectFloppy => SessionRequestKind::EjectFloppy,
            Self::MountCd(_) => SessionRequestKind::MountCd,
            Self::EjectCd => SessionRequestKind::EjectCd,
            Self::CdPlay => SessionRequestKind::CdPlay,
            Self::CdPause => SessionRequestKind::CdPause,
            Self::CdStop => SessionRequestKind::CdStop,
            Self::CdNextTrack => SessionRequestKind::CdNextTrack,
            Self::CdLinkedLevel(_) => SessionRequestKind::CdLinkedLevel,
            Self::MidiConfig(_) => SessionRequestKind::MidiConfig,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionRequestKind {
    MountFloppy,
    EjectFloppy,
    MountCd,
    EjectCd,
    CdPlay,
    CdPause,
    CdStop,
    CdNextTrack,
    CdLinkedLevel,
    MidiConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct RequestId(pub(super) u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionEvent {
    Applied {
        request_id: RequestId,
        kind: SessionRequestKind,
        state: AppliedState,
    },
    Rejected {
        request_id: RequestId,
        kind: SessionRequestKind,
        message: String,
    },
    WorkerFailed {
        generation: u64,
        message: String,
    },
    FinalizationFailed {
        generation: u64,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AppliedState {
    Floppy(Option<FloppySource>),
    Cd(Option<CdSource>),
    Midi(MidiConfig),
    Other,
}

#[derive(Debug, Clone)]
pub(super) struct SessionSnapshot {
    pub(super) powered: bool,
    pub(super) generation: u64,
    pub(super) configured_mode: GswMode,
    pub(super) memory_mib: u16,
    pub(super) mode: Option<GswMode>,
    pub(super) speed_ratio: f64,
    pub(super) idle: bool,
    pub(super) refresh_hz: f64,
    pub(super) floppy_accesses: u64,
    pub(super) c_accesses: u64,
    pub(super) cd_accesses: u64,
    pub(super) cd_audio: CdAudioState,
    pub(super) wavetable_status: MidiStatus,
    pub(super) midi_status: MidiStatus,
    pub(super) floppy_label: Option<String>,
    pub(super) floppy_source: Option<FloppySource>,
    pub(super) cd_label: Option<String>,
    pub(super) cd_source: Option<CdSource>,
    pub(super) midi_config: MidiConfig,
    pub(super) serial: String,
    pub(super) c_drive: PathBuf,
}

impl SessionSnapshot {
    fn powered_off(
        c_drive: PathBuf,
        midi_config: MidiConfig,
        configured_mode: GswMode,
        memory_mib: u16,
    ) -> Self {
        Self {
            powered: false,
            generation: 0,
            configured_mode,
            memory_mib,
            mode: None,
            speed_ratio: 0.0,
            idle: false,
            refresh_hz: 60.0,
            floppy_accesses: 0,
            c_accesses: 0,
            cd_accesses: 0,
            cd_audio: CdAudioState::default(),
            wavetable_status: MidiStatus::default(),
            midi_status: MidiStatus::default(),
            floppy_label: None,
            floppy_source: None,
            cd_label: None,
            cd_source: None,
            midi_config,
            serial: String::new(),
            c_drive,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct SessionFrame {
    pub(super) words: Vec<u32>,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) seq: u64,
    pub(super) generation: u64,
}

#[derive(Debug, Clone)]
pub(super) struct SessionUpdate {
    pub(super) snapshot: SessionSnapshot,
    pub(super) newest_frame: Option<SessionFrame>,
    pub(super) events: Vec<SessionEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SessionFailure {
    message: String,
}

impl SessionFailure {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SessionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for SessionFailure {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SessionClosed;

impl fmt::Display for SessionClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GUI session is not running")
    }
}

impl Error for SessionClosed {}

#[derive(Debug, Clone, Default)]
pub(super) struct ResetReport {
    pub(super) generation: u64,
    pub(super) failures: Vec<SessionFailure>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ShutdownReport {
    pub(super) generation: Option<u64>,
    pub(super) was_running: bool,
    pub(super) failures: Vec<SessionFailure>,
}

pub(super) struct GuiSession {
    spec: SessionSpec,
    worker: Option<RunningWorker>,
    snapshot: SessionSnapshot,
    pending_events: VecDeque<SessionEvent>,
    next_request_id: u64,
    next_generation: u64,
    last_frame: Option<(u64, u64)>,
    outstanding: BTreeMap<RequestId, SessionRequestKind>,
    closed: bool,
}

impl GuiSession {
    pub(super) fn start(
        spec: SessionSpec,
        media: PreparedInitialMedia,
    ) -> Result<Self, SessionFailure> {
        let snapshot = SessionSnapshot::powered_off(
            spec.c_drive.clone(),
            spec.midi_config.clone(),
            spec.profile.cpu,
            spec.profile.memory_mib,
        );
        let mut session = Self {
            spec,
            worker: None,
            snapshot,
            pending_events: VecDeque::new(),
            next_request_id: 1,
            next_generation: 1,
            last_frame: None,
            outstanding: BTreeMap::new(),
            closed: false,
        };
        session.start_generation(media)?;
        Ok(session)
    }

    pub(super) fn send_input(&self, input: GuestInput) -> Result<(), SessionClosed> {
        self.worker
            .as_ref()
            .ok_or(SessionClosed)?
            .commands
            .send(WorkerCommand::Input(input))
            .map_err(|_| SessionClosed)
    }

    pub(super) fn request(&mut self, request: SessionRequest) -> Result<RequestId, SessionClosed> {
        if self.closed {
            return Err(SessionClosed);
        }
        if self.worker.as_ref().is_some_and(RunningWorker::is_finished) {
            let worker = self.worker.take().expect("worker checked above");
            self.finish_worker(worker, CompletionDelivery::Event);
        }
        let request_id = RequestId(self.next_request_id);
        if self.worker.is_none() {
            let SessionRequest::MidiConfig(config) = request else {
                return Err(SessionClosed);
            };
            self.next_request_id = self.next_request_id.saturating_add(1);
            self.spec.midi_config = config.clone();
            self.snapshot.midi_config = config.clone();
            self.pending_events.push_back(SessionEvent::Applied {
                request_id,
                kind: SessionRequestKind::MidiConfig,
                state: AppliedState::Midi(config),
            });
            return Ok(request_id);
        }

        let kind = request.kind();
        self.worker
            .as_ref()
            .expect("worker checked above")
            .commands
            .send(WorkerCommand::Request {
                request_id,
                request,
            })
            .map_err(|_| SessionClosed)?;
        self.outstanding.insert(request_id, kind);
        self.next_request_id = self.next_request_id.saturating_add(1);
        Ok(request_id)
    }

    pub(super) fn poll(&mut self) -> SessionUpdate {
        if self.worker.as_ref().is_some_and(RunningWorker::is_finished) {
            let worker = self.worker.take().expect("worker checked above");
            self.finish_worker(worker, CompletionDelivery::Event);
        }
        let mut newest_frame = None;
        if let Some(worker) = &self.worker {
            let (snapshot, frame, events) =
                take_publication_update(&worker.publication, self.last_frame);
            self.snapshot = snapshot;
            if let Some(frame) = frame {
                worker
                    .publication
                    .consumed_frame_seq
                    .store(frame.seq, Ordering::Release);
                self.last_frame = Some((frame.generation, frame.seq));
                newest_frame = Some(frame);
            }
            for event in events {
                self.record_event(event);
            }
        }

        let events: Vec<_> = self.pending_events.drain(..).collect();
        if events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Applied {
                    kind: SessionRequestKind::MidiConfig,
                    ..
                }
            )
        }) {
            self.spec.midi_config = self.snapshot.midi_config.clone();
        }
        SessionUpdate {
            snapshot: self.snapshot.clone(),
            newest_frame,
            events,
        }
    }

    pub(super) fn reset(&mut self) -> Result<ResetReport, SessionFailure> {
        let exit = self.stop_generation().ok_or_else(|| {
            SessionFailure::new("cannot reset a GUI session while it is powered off")
        })?;
        let mut failures = exit.failures;
        if let Some(failure) = exit.failure {
            failures.push(failure);
        }

        let mut prepared = PreparedInitialMedia::default();
        if let Some(floppy) = exit.media.floppy {
            if floppy.writeback_pending {
                prepared.floppy = Some(PreparedFloppy {
                    label: floppy.label,
                    source: floppy.source,
                    bytes: floppy.bytes,
                    writeback_pending: true,
                });
            } else {
                match PreparedFloppy::from_source(floppy.source) {
                    Ok(floppy) => prepared.floppy = Some(floppy),
                    Err(failure) => failures.push(failure),
                }
            }
        }
        if let Some(cd) = exit.media.cd {
            match PreparedCd::from_source(cd.source) {
                Ok(cd) => prepared.cd = Some(cd),
                Err(failure) => failures.push(failure),
            }
        }

        self.start_generation(prepared)?;
        Ok(ResetReport {
            generation: self.snapshot.generation,
            failures,
        })
    }

    pub(super) fn power_off(&mut self) -> ShutdownReport {
        let Some(exit) = self.stop_generation() else {
            return ShutdownReport::default();
        };
        let mut failures = exit.failures;
        if let Some(failure) = exit.failure {
            failures.push(failure);
        }
        ShutdownReport {
            generation: Some(exit.generation),
            was_running: true,
            failures,
        }
    }

    pub(super) fn power_on(&mut self) -> Result<(), SessionFailure> {
        if self.closed {
            return Err(SessionFailure::new("cannot power on a closed GUI session"));
        }
        if self.worker.is_some() {
            return Ok(());
        }
        self.start_generation(PreparedInitialMedia::default())
    }

    pub(super) fn shutdown(&mut self) -> ShutdownReport {
        if self.closed {
            return ShutdownReport::default();
        }
        self.closed = true;
        self.power_off()
    }

    fn start_generation(&mut self, media: PreparedInitialMedia) -> Result<(), SessionFailure> {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.saturating_add(1);
        let worker = RunningWorker::start(self.spec.clone(), media, generation)?;
        self.snapshot = worker.published_snapshot();
        self.last_frame = None;
        self.worker = Some(worker);
        Ok(())
    }

    fn stop_generation(&mut self) -> Option<WorkerExit> {
        let worker = self.worker.take()?;
        Some(self.finish_worker(worker, CompletionDelivery::Report))
    }

    fn finish_worker(&mut self, worker: RunningWorker, delivery: CompletionDelivery) -> WorkerExit {
        let finished = worker.finish();
        for event in finished.events {
            self.record_event(event);
        }
        let exit = finished.exit;
        if delivery == CompletionDelivery::Event {
            if let Some(failure) = &exit.failure {
                self.pending_events.push_back(SessionEvent::WorkerFailed {
                    generation: exit.generation,
                    message: failure.to_string(),
                });
            }
            for failure in &exit.failures {
                self.pending_events
                    .push_back(SessionEvent::FinalizationFailed {
                        generation: exit.generation,
                        message: failure.to_string(),
                    });
            }
        }
        self.spec.midi_config = exit.midi_config.clone();
        let mut powered_off = SessionSnapshot::powered_off(
            self.spec.c_drive.clone(),
            self.spec.midi_config.clone(),
            self.spec.profile.cpu,
            self.spec.profile.memory_mib,
        );
        powered_off.generation = exit.generation;
        self.snapshot = powered_off;
        self.last_frame = None;
        let unresolved = std::mem::take(&mut self.outstanding);
        for (request_id, kind) in unresolved {
            self.pending_events.push_back(SessionEvent::Rejected {
                request_id,
                kind,
                message: "machine generation ended before the request was applied".into(),
            });
        }
        exit
    }

    fn record_event(&mut self, event: SessionEvent) {
        match &event {
            SessionEvent::Applied { request_id, .. }
            | SessionEvent::Rejected { request_id, .. } => {
                self.outstanding.remove(request_id);
            }
            SessionEvent::WorkerFailed { .. } | SessionEvent::FinalizationFailed { .. } => {}
        }
        self.pending_events.push_back(event);
    }
}

impl Drop for GuiSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompletionDelivery {
    Event,
    Report,
}

#[derive(Debug)]
enum WorkerCommand {
    Input(GuestInput),
    Request {
        request_id: RequestId,
        request: SessionRequest,
    },
    Shutdown,
    #[cfg(test)]
    Panic,
    #[cfg(test)]
    Gate {
        entered: mpsc::SyncSender<()>,
        release: Receiver<()>,
    },
}

struct PublishedState {
    snapshot: SessionSnapshot,
    frame: Option<SessionFrame>,
    events: Vec<SessionEvent>,
}

struct Publication {
    state: Mutex<PublishedState>,
    consumed_frame_seq: AtomicU64,
}

struct RunningWorker {
    generation: u64,
    commands: Sender<WorkerCommand>,
    publication: Arc<Publication>,
    join: Option<JoinHandle<WorkerExit>>,
}

impl RunningWorker {
    fn start(
        spec: SessionSpec,
        media: PreparedInitialMedia,
        generation: u64,
    ) -> Result<Self, SessionFailure> {
        let snapshot = SessionSnapshot::powered_off(
            spec.c_drive.clone(),
            spec.midi_config.clone(),
            spec.profile.cpu,
            spec.profile.memory_mib,
        );
        let publication = Arc::new(Publication {
            state: Mutex::new(PublishedState {
                snapshot,
                frame: None,
                events: Vec::new(),
            }),
            consumed_frame_seq: AtomicU64::new(u64::MAX),
        });
        let (command_tx, command_rx) = mpsc::channel();
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let thread_publication = Arc::clone(&publication);
        let join = std::thread::Builder::new()
            .name("izarravm-emu".into())
            .spawn(move || {
                worker_entry(
                    spec,
                    media,
                    generation,
                    command_rx,
                    startup_tx,
                    thread_publication,
                )
            })
            .map_err(|err| {
                SessionFailure::new(format!("could not start emulation thread: {err}"))
            })?;

        match startup_rx.recv() {
            Ok(Ok(_)) => Ok(Self {
                generation,
                commands: command_tx,
                publication,
                join: Some(join),
            }),
            Ok(Err(failure)) => {
                let _ = join.join();
                Err(failure)
            }
            Err(_) => {
                let message = match join.join() {
                    Ok(exit) => exit
                        .failure
                        .map(|failure| failure.to_string())
                        .unwrap_or_else(|| "emulation thread closed during startup".into()),
                    Err(payload) => panic_message(payload),
                };
                Err(SessionFailure::new(message))
            }
        }
    }

    fn published_snapshot(&self) -> SessionSnapshot {
        self.publication
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .snapshot
            .clone()
    }

    fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn finish(mut self) -> FinishedWorker {
        let _ = self.commands.send(WorkerCommand::Shutdown);
        let exit = self.join_worker();
        let events = std::mem::take(
            &mut self
                .publication
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .events,
        );
        FinishedWorker { exit, events }
    }

    fn join_worker(&mut self) -> WorkerExit {
        let Some(join) = self.join.take() else {
            return WorkerExit::closed(self.generation);
        };
        match join.join() {
            Ok(exit) => exit,
            Err(payload) => WorkerExit::failed(
                self.generation,
                SessionFailure::new(format!(
                    "emulation thread panicked outside its lifecycle guard: {}",
                    panic_message(payload)
                )),
            ),
        }
    }
}

impl Drop for RunningWorker {
    fn drop(&mut self) {
        if self.join.is_some() {
            let _ = self.commands.send(WorkerCommand::Shutdown);
            let _ = self.join_worker();
        }
    }
}

struct FinishedWorker {
    exit: WorkerExit,
    events: Vec<SessionEvent>,
}

#[derive(Debug, Default)]
struct RetainedMedia {
    floppy: Option<RetainedFloppy>,
    cd: Option<RetainedCd>,
}

#[derive(Debug)]
struct RetainedFloppy {
    label: String,
    source: FloppySource,
    bytes: Vec<u8>,
    writeback_pending: bool,
}

#[derive(Debug)]
struct RetainedCd {
    source: CdSource,
}

#[derive(Debug)]
struct WorkerExit {
    generation: u64,
    media: RetainedMedia,
    midi_config: MidiConfig,
    failures: Vec<SessionFailure>,
    failure: Option<SessionFailure>,
}

impl WorkerExit {
    fn closed(generation: u64) -> Self {
        Self {
            generation,
            media: RetainedMedia::default(),
            midi_config: MidiConfig::default(),
            failures: Vec::new(),
            failure: None,
        }
    }

    fn failed(generation: u64, failure: SessionFailure) -> Self {
        Self {
            failure: Some(failure),
            ..Self::closed(generation)
        }
    }
}

#[derive(Debug)]
struct MountedFloppy {
    label: String,
    source: FloppySource,
    writeback_pending: bool,
}

#[derive(Debug)]
struct MountedCd {
    label: String,
    source: CdSource,
}

struct MachineGeneration {
    id: u64,
    spec: SessionSpec,
    machine: Machine,
    wavetable: Option<MidiEngine>,
    midi_receiver: Option<MidiEngine>,
    midi_config: MidiConfig,
    floppy: Option<MountedFloppy>,
    cd: Option<MountedCd>,
    runtime_profile: Option<RuntimeProfiler>,
    speed_ratio: f64,
    speed_idle: bool,
    #[cfg(test)]
    finalization_probe: Option<Arc<AtomicU64>>,
}

impl MachineGeneration {
    fn build(spec: SessionSpec, id: u64) -> Result<Self, SessionFailure> {
        let machine = Machine::new(spec.profile.clone(), &spec.rom)
            .map_err(|err| SessionFailure::new(format!("failed to start machine: {err}")))?;
        #[cfg(test)]
        let finalization_probe = spec.finalization_probe.clone();
        Ok(Self {
            id,
            midi_config: spec.midi_config.clone(),
            spec,
            machine,
            wavetable: None,
            midi_receiver: None,
            floppy: None,
            cd: None,
            runtime_profile: None,
            speed_ratio: 0.0,
            speed_idle: false,
            #[cfg(test)]
            finalization_probe,
        })
    }

    fn initialize(&mut self, media: PreparedInitialMedia) -> Result<(), SessionFailure> {
        self.machine.set_fast_post(false);
        let overlays = self
            .spec
            .glide_ovl
            .clone()
            .into_iter()
            .map(|bytes| ("GLIDE2X.OVL".to_string(), bytes))
            .collect();
        // CMOS BEFORE the mount, not after: the persisted NVRAM carries the
        // sound-card routing SNDCTRL.COM saved, and mounting is what writes the
        // matching `SET BLASTER=` line into an emulator-owned AUTOEXEC.BAT. With
        // the mount first, that line is generated from the pre-CMOS profile and
        // the tool's own edit is silently reverted on the next boot.
        self.spec.rtc_setup.apply(&mut self.machine);
        self.machine
            .mount_hdd_folder_with_user_overrides(&self.spec.c_drive, overlays)
            .map_err(|err| {
                SessionFailure::new(format!(
                    "failed to mount C: host folder {}: {err}",
                    self.spec.c_drive.display()
                ))
            })?;
        if let Some(index) = crate::host_keyboard_layout_index() {
            let mut cmos = self.machine.cmos_bytes();
            cmos[0x10] = index;
            cmos[0x13] = crate::codepage_index_for_layout(index);
            self.machine.load_cmos(&cmos);
        }
        if self.spec.test_pattern {
            self.machine.load_margo_test_pattern();
        }
        if let Some(floppy) = media.floppy {
            self.mount_initial_floppy(floppy)?;
        }
        if let Some(cd) = media.cd {
            self.mount_cd(cd);
        }

        let wavetable = MidiEngine::open_wavetable(&self.midi_config);
        let midi_receiver = MidiEngine::open_receiver(&self.midi_config);
        log_midi_status(&self.midi_config, &wavetable, &midi_receiver);
        self.wavetable = Some(wavetable);
        self.midi_receiver = Some(midi_receiver);
        let now = Instant::now();
        self.runtime_profile = runtime_profile_enabled().then(|| {
            RuntimeProfiler::new(
                now,
                self.spec.sink.as_ref().and_then(AudioSink::debug_snapshot),
            )
        });
        Ok(())
    }

    fn mount_initial_floppy(&mut self, floppy: PreparedFloppy) -> Result<(), SessionFailure> {
        self.machine
            .mount_floppy(floppy.bytes)
            .map_err(|err| SessionFailure::new(format!("failed to mount floppy image: {err}")))?;
        self.floppy = Some(MountedFloppy {
            label: floppy.label,
            source: floppy.source,
            writeback_pending: floppy.writeback_pending,
        });
        Ok(())
    }

    fn mount_cd(&mut self, cd: PreparedCd) {
        self.machine.mount_cd(cd.image);
        self.cd = Some(MountedCd {
            label: cd.label,
            source: cd.source,
        });
    }

    fn snapshot(&self) -> SessionSnapshot {
        let (floppy_accesses, c_accesses) = self.machine.drive_access_counts();
        SessionSnapshot {
            powered: true,
            generation: self.id,
            configured_mode: self.spec.profile.cpu,
            memory_mib: self.spec.profile.memory_mib,
            mode: Some(self.machine.active_mode()),
            speed_ratio: self.speed_ratio,
            idle: self.speed_idle,
            refresh_hz: self.machine.display_refresh_hz(),
            floppy_accesses,
            c_accesses,
            cd_accesses: self.machine.cd_access_count(),
            cd_audio: self.machine.cd_audio_state(),
            wavetable_status: self
                .wavetable
                .as_ref()
                .map_or(MidiStatus::default(), MidiEngine::status),
            midi_status: self
                .midi_receiver
                .as_ref()
                .map_or(MidiStatus::default(), MidiEngine::status),
            floppy_label: self.floppy.as_ref().map(|floppy| floppy.label.clone()),
            floppy_source: self.floppy.as_ref().map(|floppy| floppy.source.clone()),
            cd_label: self.cd.as_ref().map(|cd| cd.label.clone()),
            cd_source: self.cd.as_ref().map(|cd| cd.source.clone()),
            midi_config: self.midi_config.clone(),
            serial: self.machine.serial_text(),
            c_drive: self.spec.c_drive.clone(),
        }
    }

    fn apply_request(&mut self, request: SessionRequest) -> Result<(), String> {
        match request {
            SessionRequest::MountFloppy(floppy) => self.replace_floppy(floppy),
            SessionRequest::EjectFloppy => self.eject_floppy(),
            SessionRequest::MountCd(cd) => {
                self.mount_cd(cd);
                Ok(())
            }
            SessionRequest::EjectCd => {
                self.machine.eject_cd();
                self.cd = None;
                Ok(())
            }
            SessionRequest::CdPlay => {
                self.machine.cd_front_panel_play();
                Ok(())
            }
            SessionRequest::CdPause => {
                self.machine.cd_front_panel_pause();
                Ok(())
            }
            SessionRequest::CdStop => {
                self.machine.cd_front_panel_stop();
                Ok(())
            }
            SessionRequest::CdNextTrack => {
                self.machine.cd_front_panel_next_track();
                Ok(())
            }
            SessionRequest::CdLinkedLevel(level) => {
                self.machine.set_cd_linked_level(level);
                Ok(())
            }
            SessionRequest::MidiConfig(config) => {
                let wavetable = self.wavetable.as_mut().expect("MIDI initialized");
                let midi_receiver = self.midi_receiver.as_mut().expect("MIDI initialized");
                wavetable.reconfigure(&config);
                midi_receiver.reconfigure(&config);
                log_midi_status(&config, wavetable, midi_receiver);
                self.midi_config = config;
                Ok(())
            }
        }
    }

    fn replace_floppy(&mut self, floppy: PreparedFloppy) -> Result<(), String> {
        let previous = self.detach_floppy_for_change()?;
        if let Err(err) = self.machine.mount_floppy(floppy.bytes) {
            if let Some((mounted, bytes)) = previous {
                self.machine
                    .mount_floppy(bytes)
                    .expect("previously mounted floppy remains valid");
                self.floppy = Some(mounted);
            }
            return Err(format!("failed to mount floppy image: {err}"));
        }
        self.floppy = Some(MountedFloppy {
            label: floppy.label,
            source: floppy.source,
            writeback_pending: floppy.writeback_pending,
        });
        Ok(())
    }

    fn eject_floppy(&mut self) -> Result<(), String> {
        self.detach_floppy_for_change().map(|_| ())
    }

    fn detach_floppy_for_change(&mut self) -> Result<Option<(MountedFloppy, Vec<u8>)>, String> {
        let Some(mut mounted) = self.floppy.take() else {
            return Ok(None);
        };
        let dirty = self.machine.floppy_dirty() || mounted.writeback_pending;
        let bytes = self
            .machine
            .eject_floppy()
            .expect("mounted floppy has image bytes");
        if dirty && let Err(err) = std::fs::write(&mounted.source.0, &bytes) {
            mounted.writeback_pending = true;
            self.machine
                .mount_floppy(bytes.clone())
                .expect("ejected floppy remains valid");
            self.floppy = Some(mounted);
            return Err(format!(
                "could not write floppy image {}: {err}",
                self.floppy
                    .as_ref()
                    .expect("floppy restored above")
                    .source
                    .0
                    .display()
            ));
        }
        mounted.writeback_pending = false;
        Ok(Some((mounted, bytes)))
    }

    fn finalize(mut self) -> FinalizedGeneration {
        finish_runtime_profile(&mut self.runtime_profile, self.spec.sink.as_ref());
        self.machine.flush_hdd_folder();
        let mut failures = Vec::new();
        let floppy = self.floppy.take().and_then(|mut mounted| {
            let dirty = self.machine.floppy_dirty() || mounted.writeback_pending;
            let bytes = self.machine.eject_floppy()?;
            if dirty {
                match std::fs::write(&mounted.source.0, &bytes) {
                    Ok(()) => mounted.writeback_pending = false,
                    Err(err) => {
                        mounted.writeback_pending = true;
                        failures.push(SessionFailure::new(format!(
                            "could not write floppy image {}: {err}",
                            mounted.source.0.display()
                        )));
                    }
                }
            }
            Some(RetainedFloppy {
                label: mounted.label,
                source: mounted.source,
                bytes,
                writeback_pending: mounted.writeback_pending,
            })
        });
        let cd = self.cd.take().map(|cd| RetainedCd { source: cd.source });
        crate::cmos::save_cmos_file(&self.spec.rtc_setup.cmos_path, &self.machine.cmos_bytes());
        #[cfg(test)]
        if let Some(probe) = &self.finalization_probe {
            probe.fetch_add(1, Ordering::Relaxed);
        }
        FinalizedGeneration {
            media: RetainedMedia { floppy, cd },
            midi_config: self.midi_config,
            failures,
        }
    }
}

struct FinalizedGeneration {
    media: RetainedMedia,
    midi_config: MidiConfig,
    failures: Vec<SessionFailure>,
}

fn log_midi_status(config: &MidiConfig, wavetable: &MidiEngine, midi_receiver: &MidiEngine) {
    if !matches!(
        wavetable.status(),
        MidiStatus::Ready | MidiStatus::MissingSoundFont
    ) {
        warn!(
            status = ?wavetable.status(),
            "P300 FluidSynth unavailable; the guest MPU remains active"
        );
    }
    if midi_receiver.status() != MidiStatus::Ready {
        warn!(
            backend = %config.backend,
            status = ?midi_receiver.status(),
            "P330 receiver unavailable; the guest MPU remains active"
        );
    }
}

enum WorkerStop {
    Shutdown,
    Disconnected,
    StartupFailed(SessionFailure),
}

fn worker_entry(
    spec: SessionSpec,
    media: PreparedInitialMedia,
    generation_id: u64,
    commands: Receiver<WorkerCommand>,
    startup: mpsc::SyncSender<Result<SessionSnapshot, SessionFailure>>,
    publication: Arc<Publication>,
) -> WorkerExit {
    let generation = match MachineGeneration::build(spec, generation_id) {
        Ok(generation) => generation,
        Err(failure) => {
            let _ = startup.send(Err(failure.clone()));
            return WorkerExit::failed(generation_id, failure);
        }
    };
    let mut generation = Some(generation);
    let mut started = false;
    let (guarded, finalized) = run_generation_guarded(
        generation.take().expect("generation owned by guard"),
        |generation| {
            if let Err(failure) = generation.initialize(media) {
                let _ = startup.send(Err(failure.clone()));
                return WorkerStop::StartupFailed(failure);
            }
            let snapshot = generation.snapshot();
            publish_snapshot(&publication, snapshot.clone());
            if startup.send(Ok(snapshot)).is_err() {
                return WorkerStop::Disconnected;
            }
            started = true;
            run_worker(generation, &commands, &publication)
        },
    );

    let failure = match guarded {
        Ok(WorkerStop::StartupFailed(failure)) => Some(failure),
        Ok(WorkerStop::Shutdown | WorkerStop::Disconnected) => None,
        Err(failure) => {
            if !started {
                let _ = startup.send(Err(failure.clone()));
            }
            Some(failure)
        }
    };
    publish_powered_off(&publication, generation_id, &finalized.midi_config);
    WorkerExit {
        generation: generation_id,
        media: finalized.media,
        midi_config: finalized.midi_config,
        failures: finalized.failures,
        failure,
    }
}

fn run_generation_guarded(
    mut generation: MachineGeneration,
    run: impl FnOnce(&mut MachineGeneration) -> WorkerStop,
) -> (Result<WorkerStop, SessionFailure>, FinalizedGeneration) {
    let generation_id = generation.id;
    let result =
        panic::catch_unwind(AssertUnwindSafe(|| run(&mut generation))).map_err(|payload| {
            SessionFailure::new(format!(
                "machine generation {generation_id} panicked: {}",
                panic_message(payload)
            ))
        });
    let finalized = generation.finalize();
    (result, finalized)
}

fn publish_snapshot(publication: &Publication, snapshot: SessionSnapshot) {
    publication
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .snapshot = snapshot;
}

fn publish_request_result(
    publication: &Publication,
    snapshot: SessionSnapshot,
    event: SessionEvent,
) {
    let mut published = publication
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    published.snapshot = snapshot;
    published.events.push(event);
}

fn take_publication_update(
    publication: &Publication,
    last_frame: Option<(u64, u64)>,
) -> (SessionSnapshot, Option<SessionFrame>, Vec<SessionEvent>) {
    let mut published = publication
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = published.snapshot.clone();
    let frame = published.frame.as_ref().and_then(|frame| {
        let key = (frame.generation, frame.seq);
        (last_frame != Some(key)).then(|| frame.clone())
    });
    let events = std::mem::take(&mut published.events);
    (snapshot, frame, events)
}

fn publish_powered_off(publication: &Publication, generation: u64, midi_config: &MidiConfig) {
    let mut published = publication
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = &published.snapshot;
    let mut snapshot = SessionSnapshot::powered_off(
        previous.c_drive.clone(),
        midi_config.clone(),
        previous.configured_mode,
        previous.memory_mib,
    );
    snapshot.generation = generation;
    published.snapshot = snapshot;
    published.frame = None;
}

fn run_worker(
    generation: &mut MachineGeneration,
    commands: &Receiver<WorkerCommand>,
    publication: &Publication,
) -> WorkerStop {
    let mut audio_debt = 0.0;
    let mut speed_wall = Duration::ZERO;
    let mut speed_halted = 0u64;
    let mut speed_advanced = 0u64;
    let mut credit: i64 = 0;
    let mut last_pace = Instant::now();
    let mut last_media = last_pace;
    let mut published_seq = u64::MAX;
    let mut last_frame_gen: Option<u64> = None;
    let mut keys = ScriptedKeys::from_env(generation.machine.master_ticks());

    loop {
        loop {
            let command = match commands.try_recv() {
                Ok(command) => command,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return WorkerStop::Disconnected,
            };
            match command {
                WorkerCommand::Input(input) => apply_guest_input(&mut generation.machine, input),
                WorkerCommand::Request {
                    request_id,
                    request,
                } => {
                    let kind = request.kind();
                    let result = generation.apply_request(request);
                    let snapshot = generation.snapshot();
                    let state = applied_state(kind, &snapshot);
                    let event = match result {
                        Ok(()) => SessionEvent::Applied {
                            request_id,
                            kind,
                            state,
                        },
                        Err(message) => SessionEvent::Rejected {
                            request_id,
                            kind,
                            message,
                        },
                    };
                    publish_request_result(publication, snapshot, event);
                }
                WorkerCommand::Shutdown => return WorkerStop::Shutdown,
                #[cfg(test)]
                WorkerCommand::Panic => panic!("injected worker failure"),
                #[cfg(test)]
                WorkerCommand::Gate { entered, release } => {
                    let _ = entered.send(());
                    let _ = release.recv();
                }
            }
        }

        let run_started = Instant::now();
        let cap = MASTER_CLOCK_HZ / 20;
        credit = refill_credit(credit, run_started.duration_since(last_pace), cap);
        last_pace = run_started;
        let budget = credit.max(0) as u64;
        let mut terminal_stop = false;
        let mut consumed_ticks = 0u64;
        // The same three splits the on-screen speed indicator is built from, kept per
        // slice so the runtime profile can report the indicator beside the true realtime
        // factor. They disagree by construction: the indicator drops halted and stalled
        // ticks, so a guest that idles reads below 100% while the machine is dead on time.
        let mut slice_executed = 0u64;
        let mut slice_halted = 0u64;
        let mut slice_stalled = 0u64;
        if budget > 0 {
            let before = generation.machine.master_ticks();
            let stall_before = generation.machine.io_stall_ticks();
            let halted_before = generation.machine.halted_ticks();
            let approximate = generation.machine.active_mode().uses_approximate_timing();
            let stop = tick_machine_ticks(
                &mut generation.machine,
                execution_budget(credit, approximate),
            );
            terminal_stop = matches!(
                stop,
                Some(
                    StopReason::CpuError(_)
                        | StopReason::DosExit { .. }
                        | StopReason::TestExit { .. }
                )
            );
            let ran = generation.machine.master_ticks().saturating_sub(before);
            let stalled = generation
                .machine
                .io_stall_ticks()
                .saturating_sub(stall_before);
            let halt_top_up =
                halted_device_top_up(budget, ran, matches!(stop, Some(StopReason::Halted)));
            if halt_top_up > 0 {
                generation.machine.advance_devices_ticks(halt_top_up);
            }
            consumed_ticks = ran.saturating_add(halt_top_up);
            let halted = generation
                .machine
                .halted_ticks()
                .saturating_sub(halted_before);
            slice_executed = ran.saturating_sub(stalled).saturating_sub(halted);
            slice_halted = halted.saturating_add(halt_top_up);
            slice_stalled = stalled;
            speed_halted = speed_halted.saturating_add(slice_halted);
            speed_advanced = speed_advanced.saturating_add(consumed_ticks);
        }
        keys.pump(&mut generation.machine);
        let run_finished = Instant::now();
        credit = settle_credit(
            credit,
            consumed_ticks,
            run_finished.duration_since(last_pace),
            cap,
        );
        last_pace = run_finished;
        let dt = run_finished.duration_since(last_media);
        let dt_secs = dt.as_secs_f64();
        speed_wall = speed_wall.saturating_add(dt);
        last_media = run_finished;
        if speed_wall >= SPEED_SAMPLE_INTERVAL {
            (generation.speed_ratio, generation.speed_idle) =
                speed_sample(speed_halted, speed_advanced, speed_wall);
            speed_wall = Duration::ZERO;
            speed_halted = 0;
            speed_advanced = 0;
        }

        let wavetable = generation.wavetable.as_mut().expect("MIDI initialized");
        let midi_receiver = generation.midi_receiver.as_mut().expect("MIDI initialized");
        pump_midi(&mut generation.machine, wavetable, midi_receiver);
        if let Some(sink) = &generation.spec.sink {
            pump_audio(
                &mut generation.machine,
                wavetable,
                midi_receiver,
                sink,
                dt_secs,
                &mut audio_debt,
                generation.spec.gain.get(),
            );
        }
        let audio_finished = generation.runtime_profile.as_ref().map(|_| Instant::now());

        let seq = generation.machine.frame_sequence();
        let published_before = published_seq;
        let frame_gen = generation.machine.presented_frame_generation();
        let consumed_seq = publication.consumed_frame_seq.load(Ordering::Acquire);
        let backpressured = seq != published_seq && consumed_seq != published_seq;
        let new_frame =
            should_publish_frame(seq, published_seq, consumed_seq, frame_gen, last_frame_gen);
        let rendered = new_frame.then(|| generation.machine.presented_frame_argb());
        let frame_produced = rendered.is_some();
        let serial = new_frame.then(|| generation.machine.serial_text());
        let mode = generation.machine.active_mode();
        let refresh_hz = generation.machine.display_refresh_hz();
        let (floppy_accesses, c_accesses) = generation.machine.drive_access_counts();
        let cd_accesses = generation.machine.cd_access_count();
        let cd_audio = generation.machine.cd_audio_state();
        {
            let mut published = publication
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((words, width, height)) = rendered {
                published.frame = Some(SessionFrame {
                    words,
                    width,
                    height,
                    seq,
                    generation: generation.id,
                });
                last_frame_gen = frame_gen;
                published_seq = seq;
            }
            let snapshot = &mut published.snapshot;
            if let Some(serial) = serial {
                snapshot.serial = serial;
            }
            snapshot.powered = true;
            snapshot.generation = generation.id;
            snapshot.mode = Some(mode);
            snapshot.refresh_hz = refresh_hz;
            snapshot.speed_ratio = generation.speed_ratio;
            snapshot.idle = generation.speed_idle;
            snapshot.floppy_accesses = floppy_accesses;
            snapshot.c_accesses = c_accesses;
            snapshot.cd_accesses = cd_accesses;
            snapshot.cd_audio = cd_audio;
            snapshot.wavetable_status = wavetable.status();
            snapshot.midi_status = midi_receiver.status();
        }
        if generation.machine.take_cmos_dirty() {
            crate::cmos::save_cmos_file(
                &generation.spec.rtc_setup.cmos_path,
                &generation.machine.cmos_bytes(),
            );
        }

        let before_sleep = Instant::now();
        credit = refill_credit(credit, before_sleep.duration_since(last_pace), cap);
        last_pace = before_sleep;
        let should_sleep = emulation_should_sleep(credit, terminal_stop);
        if should_sleep {
            std::thread::sleep(EMU_SLICE);
        }
        if let (Some(profile), Some(audio_finished)) =
            (generation.runtime_profile.as_mut(), audio_finished)
        {
            let profile_finished = Instant::now();
            let sleep = if should_sleep {
                profile_finished.duration_since(before_sleep)
            } else {
                Duration::ZERO
            };
            profile.record_slice(
                run_finished.duration_since(run_started),
                audio_finished.duration_since(run_finished),
                before_sleep.duration_since(audio_finished),
                sleep,
                SliceTicks {
                    advanced: consumed_ticks,
                    executed: slice_executed,
                    halted: slice_halted,
                    stalled: slice_stalled,
                },
                credit,
                seq,
                published_before,
                frame_produced,
                backpressured,
                before_sleep,
            );
            profile.maybe_emit(profile_finished, generation.spec.sink.as_ref());
        }
    }
}

fn apply_guest_input(machine: &mut Machine, input: GuestInput) {
    match input {
        GuestInput::Keys(codes) => machine.inject_key_scancodes(&codes),
        GuestInput::MouseRelative(dx, dy, buttons) => {
            machine.inject_mouse_relative(dx, dy, buttons)
        }
        GuestInput::MouseWheel(dz) => machine.inject_mouse_wheel(dz),
        GuestInput::Joystick(state) => machine.set_joystick_state(state),
    }
}

fn applied_state(kind: SessionRequestKind, snapshot: &SessionSnapshot) -> AppliedState {
    match kind {
        SessionRequestKind::MountFloppy | SessionRequestKind::EjectFloppy => {
            AppliedState::Floppy(snapshot.floppy_source.clone())
        }
        SessionRequestKind::MountCd | SessionRequestKind::EjectCd => {
            AppliedState::Cd(snapshot.cd_source.clone())
        }
        SessionRequestKind::MidiConfig => AppliedState::Midi(snapshot.midi_config.clone()),
        SessionRequestKind::CdPlay
        | SessionRequestKind::CdPause
        | SessionRequestKind::CdStop
        | SessionRequestKind::CdNextTrack
        | SessionRequestKind::CdLinkedLevel => AppliedState::Other,
    }
}

fn refill_credit(credit: i64, dt: Duration, cap: u64) -> i64 {
    let wall_ticks =
        (dt.as_nanos() * u128::from(MASTER_CLOCK_HZ) / 1_000_000_000).min(i64::MAX as u128) as i64;
    credit.saturating_add(wall_ticks).min(cap as i64)
}

fn settle_credit(credit: i64, executed_ticks: u64, dt: Duration, cap: u64) -> i64 {
    let executed = i64::try_from(executed_ticks).unwrap_or(i64::MAX);
    refill_credit(credit.saturating_sub(executed), dt, cap)
}

fn execution_budget(credit: i64, approximate: bool) -> u64 {
    let available = credit.max(0) as u64;
    if approximate {
        available.min(FAST_EMU_QUANTUM_TICKS)
    } else {
        available
    }
}

fn halted_device_top_up(budget: u64, executed: u64, halted: bool) -> u64 {
    if halted {
        budget.saturating_sub(executed)
    } else {
        0
    }
}

fn should_publish_frame(
    current_seq: u64,
    published_seq: u64,
    consumed_seq: u64,
    frame_generation: Option<u64>,
    published_generation: Option<u64>,
) -> bool {
    current_seq != published_seq
        && consumed_seq == published_seq
        && !matches!(
            (frame_generation, published_generation),
            (Some(current), Some(published)) if current == published
        )
}

fn emulation_should_sleep(credit: i64, terminal_stop: bool) -> bool {
    terminal_stop || credit <= 0
}

/// The window's speed readout: how much guest time the machine delivered per unit
/// of wall time, capped at 1.0 because the pacing credit will not let it run ahead.
///
/// The ratio counts every tick the guest advanced through, halted ones included.
/// It used to count only ticks that retired work, which made the readout `1 -
/// halted_share`: Prince of Persia idles about 20% of a 486 second waiting on its
/// frame timer, so it displayed "Speed 80%" while the machine was dead on real
/// time, and that reads as a 20% emulator problem that does not exist. A guest
/// that idles is reported by `idle`, which is the affordance for it.
fn speed_sample(halted_ticks: u64, advanced_ticks: u64, wall: Duration) -> (f64, bool) {
    let idle =
        advanced_ticks != 0 && u128::from(halted_ticks) * 10 >= u128::from(advanced_ticks) * 9;
    let wall_ticks = wall.as_secs_f64() * MASTER_CLOCK_HZ as f64;
    let ratio = if wall_ticks > 0.0 {
        (advanced_ticks as f64 / wall_ticks).min(1.0)
    } else {
        0.0
    };
    (ratio, idle)
}

fn duration_ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

/// One scancode every 2 ms of guest time, the spacing `--inject-keys` uses: the
/// guest has to poll INT 16h and consume each code before the next arrives, or
/// the type-ahead buffer swallows it.
const SCRIPTED_KEY_SPACING_TICKS: u64 = MASTER_CLOCK_HZ * 2 / 1000;

/// Deterministic scripted keystrokes for the GUI worker, armed by
/// `IZARRAVM_GUI_INJECT_KEYS`. Profiling the window needs the same reach that
/// `--inject-keys` gives the headless path -- the workloads worth profiling are
/// games whose play sits behind a title screen -- and a human pressing a key
/// cannot be replayed. Steps are `<guest_ms>:<text>` separated by `;`, offsets
/// strictly increasing, and the text spelling is `--inject-keys`'s, `{shift}`
/// and all.
///
/// The offsets are GUEST MILLISECONDS, not the CPU clocks `--inject-keys` takes.
/// The GUI paces to real time, so guest milliseconds are both what the wall
/// clock shows and the one unit that stays put when the persona changes.
struct ScriptedKeys {
    origin_ticks: u64,
    due: VecDeque<(u64, Vec<u8>)>,
    pending: VecDeque<u8>,
    last_key_ticks: u64,
}

impl ScriptedKeys {
    fn from_env(origin_ticks: u64) -> Self {
        let idle = Self {
            origin_ticks,
            due: VecDeque::new(),
            pending: VecDeque::new(),
            last_key_ticks: 0,
        };
        let Ok(spec) = std::env::var("IZARRAVM_GUI_INJECT_KEYS") else {
            return idle;
        };
        match parse_scripted_keys(&spec) {
            Ok(due) => Self { due, ..idle },
            Err(message) => {
                warn!(%message, "ignoring IZARRAVM_GUI_INJECT_KEYS");
                idle
            }
        }
    }

    fn pump(&mut self, machine: &mut Machine) {
        if self.due.is_empty() && self.pending.is_empty() {
            return;
        }
        let elapsed = machine.master_ticks().saturating_sub(self.origin_ticks);
        while self.due.front().is_some_and(|(at, _)| *at <= elapsed) {
            let (_, codes) = self.due.pop_front().expect("front was just checked");
            self.pending.extend(codes);
        }
        if elapsed.saturating_sub(self.last_key_ticks) < SCRIPTED_KEY_SPACING_TICKS {
            return;
        }
        if let Some(code) = self.pending.pop_front() {
            machine.inject_key_scancodes(&[code]);
            self.last_key_ticks = elapsed;
        }
    }
}

fn parse_scripted_keys(spec: &str) -> Result<VecDeque<(u64, Vec<u8>)>, String> {
    let mut steps = VecDeque::new();
    let mut previous: Option<u64> = None;
    for raw in spec.split(';').filter(|step| !step.trim().is_empty()) {
        let (millis, text) = raw
            .split_once(':')
            .ok_or_else(|| format!("step {raw:?} is not <guest_ms>:<text>"))?;
        let at_millis: u64 = millis
            .trim()
            .parse()
            .map_err(|_| format!("step {raw:?} has a non-numeric offset"))?;
        if previous.is_some_and(|last| at_millis <= last) {
            return Err(format!(
                "offsets must strictly increase; {at_millis} does not follow {}",
                previous.unwrap_or_default()
            ));
        }
        previous = Some(at_millis);
        let codes = crate::text_to_scancode_groups(&text.replace("\\r", "\r"))
            .map_err(|error| error.to_string())?
            .concat();
        steps.push_back((at_millis.saturating_mul(MASTER_CLOCK_HZ / 1000), codes));
    }
    Ok(steps)
}

/// One emulation slice's guest time, split the way the on-screen speed indicator
/// splits it. `advanced` is every master tick the machine moved through; the other
/// three partition it into ticks that retired guest work, ticks the CPU sat halted,
/// and ticks it stalled on device I/O.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SliceTicks {
    advanced: u64,
    executed: u64,
    halted: u64,
    stalled: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RuntimeProfileMetrics {
    emulation_work_wall_ns: u64,
    host_audio_mix_queue_wall_ns: u64,
    frame_conversion_publish_wall_ns: u64,
    presentation_backpressure_wall_ns: u64,
    throttle_sleep_wall_ns: u64,
    guest_master_ticks: u64,
    executed_master_ticks: u64,
    halted_master_ticks: u64,
    stalled_master_ticks: u64,
    current_pacing_credit_ticks: i64,
    max_catchup_credit_ticks: u64,
    max_throttle_ahead_ticks: u64,
    frames_produced: u64,
    frames_skipped: u64,
}

impl RuntimeProfileMetrics {
    fn record_work(&mut self, emulation: Duration, audio: Duration, frame: Duration) {
        self.emulation_work_wall_ns = self
            .emulation_work_wall_ns
            .saturating_add(duration_ns(emulation));
        self.host_audio_mix_queue_wall_ns = self
            .host_audio_mix_queue_wall_ns
            .saturating_add(duration_ns(audio));
        self.frame_conversion_publish_wall_ns = self
            .frame_conversion_publish_wall_ns
            .saturating_add(duration_ns(frame));
    }

    fn record_backpressure(&mut self, duration: Duration) {
        self.presentation_backpressure_wall_ns = self
            .presentation_backpressure_wall_ns
            .saturating_add(duration_ns(duration));
    }

    fn record_sleep(&mut self, duration: Duration) {
        self.throttle_sleep_wall_ns = self
            .throttle_sleep_wall_ns
            .saturating_add(duration_ns(duration));
    }

    fn record_ticks(&mut self, ticks: SliceTicks) {
        self.guest_master_ticks = self.guest_master_ticks.saturating_add(ticks.advanced);
        self.executed_master_ticks = self.executed_master_ticks.saturating_add(ticks.executed);
        self.halted_master_ticks = self.halted_master_ticks.saturating_add(ticks.halted);
        self.stalled_master_ticks = self.stalled_master_ticks.saturating_add(ticks.stalled);
    }

    fn observe_credit(&mut self, credit: i64) {
        self.current_pacing_credit_ticks = credit;
        self.max_catchup_credit_ticks = self.max_catchup_credit_ticks.max(credit.max(0) as u64);
        if credit < 0 {
            self.max_throttle_ahead_ticks =
                self.max_throttle_ahead_ticks.max(credit.unsigned_abs());
        }
    }

    fn record_frame(&mut self, current_seq: u64, published_seq: u64, produced: bool) {
        if !produced {
            return;
        }
        self.frames_produced = self.frames_produced.saturating_add(1);
        if published_seq != u64::MAX && current_seq > published_seq {
            self.frames_skipped = self
                .frames_skipped
                .saturating_add(current_seq - published_seq - 1);
        }
    }
}

#[derive(Debug, Serialize)]
struct RuntimeProfileReport {
    schema: &'static str,
    scope: &'static str,
    interval_index: u64,
    wall_ns: u64,
    emulation_work_wall_ns: u64,
    host_audio_mix_queue_wall_ns: u64,
    frame_conversion_publish_wall_ns: u64,
    active_work_wall_ns: u64,
    presentation_backpressure_wall_ns: u64,
    throttle_sleep_wall_ns: u64,
    guest_master_ticks: u64,
    executed_master_ticks: u64,
    halted_master_ticks: u64,
    stalled_master_ticks: u64,
    wall_master_ticks: u64,
    guest_realtime_factor: f64,
    /// What the window's speed readout shows: executed ticks over wall ticks, capped
    /// at 1.0. Compare it against `guest_realtime_factor` -- when the machine is on
    /// time this reads `1 - halted_share`, so an idling guest looks slow when it is not.
    speed_indicator_ratio: f64,
    halted_share_of_guest_time: f64,
    uncapped_wall_guest_lag_ticks: i64,
    uncapped_wall_guest_lag_seconds: f64,
    total_guest_realtime_factor: f64,
    uncapped_total_wall_guest_lag_ticks: i64,
    uncapped_total_wall_guest_lag_seconds: f64,
    current_pacing_credit_ticks: i64,
    current_catchup_credit_ticks: u64,
    current_throttle_ahead_ticks: u64,
    max_catchup_credit_ticks: u64,
    max_throttle_ahead_ticks: u64,
    current_catchup_credit_seconds: f64,
    current_throttle_ahead_seconds: f64,
    max_catchup_credit_seconds: f64,
    max_throttle_ahead_seconds: f64,
    frames_produced: u64,
    frames_skipped: u64,
    audio_debug_available: bool,
    audio_frames_produced: u64,
    audio_frames_consumed: u64,
    audio_queue_lifetime_min_depth: usize,
    audio_queue_lifetime_max_depth: usize,
    audio_underruns_after_prefill: u64,
    audio_overruns: u64,
    audio_late_callbacks: u64,
    audio_callback_lateness_us: u64,
    audio_lifetime_max_callback_lateness_us: u64,
}

impl RuntimeProfileReport {
    fn new(
        scope: &'static str,
        interval_index: u64,
        wall: Duration,
        metrics: RuntimeProfileMetrics,
        total_wall: Duration,
        total_metrics: RuntimeProfileMetrics,
        audio: Option<AudioDebugSnapshot>,
    ) -> Self {
        let audio_debug_available = audio.is_some();
        let audio = audio.unwrap_or_default();
        let current_catchup_credit_ticks = metrics.current_pacing_credit_ticks.max(0) as u64;
        let current_throttle_ahead_ticks = if metrics.current_pacing_credit_ticks < 0 {
            metrics.current_pacing_credit_ticks.unsigned_abs()
        } else {
            0
        };
        let ticks_per_second = MASTER_CLOCK_HZ as f64;
        let wall_master_ticks = master_ticks_for_duration(wall);
        let total_wall_master_ticks = master_ticks_for_duration(total_wall);
        let uncapped_wall_guest_lag_ticks =
            signed_tick_difference(wall_master_ticks, metrics.guest_master_ticks);
        let uncapped_total_wall_guest_lag_ticks =
            signed_tick_difference(total_wall_master_ticks, total_metrics.guest_master_ticks);
        let active_work_wall_ns = metrics
            .emulation_work_wall_ns
            .saturating_add(metrics.host_audio_mix_queue_wall_ns)
            .saturating_add(metrics.frame_conversion_publish_wall_ns);
        Self {
            schema: RUNTIME_PROFILE_SCHEMA,
            scope,
            interval_index,
            wall_ns: duration_ns(wall),
            emulation_work_wall_ns: metrics.emulation_work_wall_ns,
            host_audio_mix_queue_wall_ns: metrics.host_audio_mix_queue_wall_ns,
            frame_conversion_publish_wall_ns: metrics.frame_conversion_publish_wall_ns,
            active_work_wall_ns,
            presentation_backpressure_wall_ns: metrics.presentation_backpressure_wall_ns,
            throttle_sleep_wall_ns: metrics.throttle_sleep_wall_ns,
            guest_master_ticks: metrics.guest_master_ticks,
            executed_master_ticks: metrics.executed_master_ticks,
            halted_master_ticks: metrics.halted_master_ticks,
            stalled_master_ticks: metrics.stalled_master_ticks,
            wall_master_ticks,
            guest_realtime_factor: realtime_factor(metrics.guest_master_ticks, wall_master_ticks),
            speed_indicator_ratio: realtime_factor(
                metrics.executed_master_ticks,
                wall_master_ticks,
            )
            .min(1.0),
            halted_share_of_guest_time: realtime_factor(
                metrics.halted_master_ticks,
                metrics.guest_master_ticks,
            ),
            uncapped_wall_guest_lag_ticks,
            uncapped_wall_guest_lag_seconds: uncapped_wall_guest_lag_ticks as f64
                / ticks_per_second,
            total_guest_realtime_factor: realtime_factor(
                total_metrics.guest_master_ticks,
                total_wall_master_ticks,
            ),
            uncapped_total_wall_guest_lag_ticks,
            uncapped_total_wall_guest_lag_seconds: uncapped_total_wall_guest_lag_ticks as f64
                / ticks_per_second,
            current_pacing_credit_ticks: metrics.current_pacing_credit_ticks,
            current_catchup_credit_ticks,
            current_throttle_ahead_ticks,
            max_catchup_credit_ticks: metrics.max_catchup_credit_ticks,
            max_throttle_ahead_ticks: metrics.max_throttle_ahead_ticks,
            current_catchup_credit_seconds: current_catchup_credit_ticks as f64 / ticks_per_second,
            current_throttle_ahead_seconds: current_throttle_ahead_ticks as f64 / ticks_per_second,
            max_catchup_credit_seconds: metrics.max_catchup_credit_ticks as f64 / ticks_per_second,
            max_throttle_ahead_seconds: metrics.max_throttle_ahead_ticks as f64 / ticks_per_second,
            frames_produced: metrics.frames_produced,
            frames_skipped: metrics.frames_skipped,
            audio_debug_available,
            audio_frames_produced: audio.frames_produced,
            audio_frames_consumed: audio.frames_consumed,
            audio_queue_lifetime_min_depth: audio.queue_min_depth,
            audio_queue_lifetime_max_depth: audio.queue_max_depth,
            audio_underruns_after_prefill: audio.underruns_after_prefill,
            audio_overruns: audio.overruns,
            audio_late_callbacks: audio.late_callbacks,
            audio_callback_lateness_us: audio.callback_lateness_us,
            audio_lifetime_max_callback_lateness_us: audio.max_callback_lateness_us,
        }
    }
}

fn master_ticks_for_duration(duration: Duration) -> u64 {
    (duration
        .as_nanos()
        .saturating_mul(u128::from(MASTER_CLOCK_HZ))
        / 1_000_000_000)
        .min(u128::from(u64::MAX)) as u64
}

fn signed_tick_difference(left: u64, right: u64) -> i64 {
    if left >= right {
        i64::try_from(left - right).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(right - left).unwrap_or(i64::MAX)
    }
}

fn realtime_factor(guest_ticks: u64, wall_ticks: u64) -> f64 {
    if wall_ticks == 0 {
        0.0
    } else {
        guest_ticks as f64 / wall_ticks as f64
    }
}

fn audio_snapshot_delta(
    current: AudioDebugSnapshot,
    previous: AudioDebugSnapshot,
) -> AudioDebugSnapshot {
    AudioDebugSnapshot {
        frames_produced: current
            .frames_produced
            .saturating_sub(previous.frames_produced),
        frames_consumed: current
            .frames_consumed
            .saturating_sub(previous.frames_consumed),
        queue_min_depth: current.queue_min_depth,
        queue_max_depth: current.queue_max_depth,
        low_water_writes: current
            .low_water_writes
            .saturating_sub(previous.low_water_writes),
        underruns_after_prefill: current
            .underruns_after_prefill
            .saturating_sub(previous.underruns_after_prefill),
        overruns: current.overruns.saturating_sub(previous.overruns),
        late_callbacks: current
            .late_callbacks
            .saturating_sub(previous.late_callbacks),
        callback_lateness_us: current
            .callback_lateness_us
            .saturating_sub(previous.callback_lateness_us),
        max_callback_lateness_us: current.max_callback_lateness_us,
    }
}

fn audio_snapshot_since(
    current: Option<AudioDebugSnapshot>,
    baseline: Option<AudioDebugSnapshot>,
) -> Option<AudioDebugSnapshot> {
    match (current, baseline) {
        (Some(current), Some(baseline)) => Some(audio_snapshot_delta(current, baseline)),
        (Some(current), None) => Some(current),
        (None, _) => None,
    }
}

struct RuntimeProfiler {
    started: Instant,
    interval_started: Instant,
    interval_index: u64,
    interval: RuntimeProfileMetrics,
    total: RuntimeProfileMetrics,
    initial_audio: Option<AudioDebugSnapshot>,
    last_audio: Option<AudioDebugSnapshot>,
    backpressure_started: Option<Instant>,
}

impl RuntimeProfiler {
    fn new(now: Instant, audio: Option<AudioDebugSnapshot>) -> Self {
        Self {
            started: now,
            interval_started: now,
            interval_index: 0,
            interval: RuntimeProfileMetrics::default(),
            total: RuntimeProfileMetrics::default(),
            initial_audio: audio,
            last_audio: audio,
            backpressure_started: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_slice(
        &mut self,
        emulation: Duration,
        audio: Duration,
        frame: Duration,
        sleep: Duration,
        ticks: SliceTicks,
        credit: i64,
        current_seq: u64,
        published_seq: u64,
        frame_produced: bool,
        backpressured: bool,
        now: Instant,
    ) {
        self.interval.record_work(emulation, audio, frame);
        self.total.record_work(emulation, audio, frame);
        self.interval.record_sleep(sleep);
        self.total.record_sleep(sleep);
        self.interval.record_ticks(ticks);
        self.total.record_ticks(ticks);
        self.interval.observe_credit(credit);
        self.total.observe_credit(credit);
        self.interval
            .record_frame(current_seq, published_seq, frame_produced);
        self.total
            .record_frame(current_seq, published_seq, frame_produced);

        match (self.backpressure_started, backpressured) {
            (None, true) => self.backpressure_started = Some(now),
            (Some(start), false) => {
                let duration = now.duration_since(start);
                self.interval.record_backpressure(duration);
                self.total.record_backpressure(duration);
                self.backpressure_started = None;
            }
            _ => {}
        }
    }

    fn settle_backpressure(&mut self, now: Instant) {
        let Some(start) = self.backpressure_started else {
            return;
        };
        let duration = now.duration_since(start);
        self.interval.record_backpressure(duration);
        self.total.record_backpressure(duration);
        self.backpressure_started = Some(now);
    }

    fn maybe_emit(&mut self, now: Instant, sink: Option<&AudioSink>) {
        let wall = now.duration_since(self.interval_started);
        if wall < RUNTIME_PROFILE_INTERVAL {
            return;
        }
        self.settle_backpressure(now);
        let audio = sink.and_then(AudioSink::debug_snapshot);
        let audio_delta = audio_snapshot_since(audio, self.last_audio);
        emit_runtime_profile(RuntimeProfileReport::new(
            "interval",
            self.interval_index,
            wall,
            self.interval,
            now.duration_since(self.started),
            self.total,
            audio_delta,
        ));
        self.interval = RuntimeProfileMetrics::default();
        self.interval
            .observe_credit(self.total.current_pacing_credit_ticks);
        self.interval_started = now;
        self.interval_index = self.interval_index.saturating_add(1);
        self.last_audio = audio;
    }

    fn finish(&mut self, now: Instant, sink: Option<&AudioSink>) {
        self.settle_backpressure(now);
        let audio =
            audio_snapshot_since(sink.and_then(AudioSink::debug_snapshot), self.initial_audio);
        let wall = now.duration_since(self.started);
        emit_runtime_profile(RuntimeProfileReport::new(
            "final",
            self.interval_index,
            wall,
            self.total,
            wall,
            self.total,
            audio,
        ));
    }
}

fn emit_runtime_profile(report: RuntimeProfileReport) {
    match serde_json::to_string(&report) {
        Ok(line) => eprintln!("{line}"),
        Err(err) => error!(%err, "could not serialize runtime profile"),
    }
}

fn runtime_profile_enabled() -> bool {
    std::env::var("IZARRAVM_RUNTIME_PROFILE").as_deref() == Ok("1")
}

fn finish_runtime_profile(profile: &mut Option<RuntimeProfiler>, sink: Option<&AudioSink>) {
    if let Some(profile) = profile {
        profile.finish(Instant::now(), sink);
    }
}

fn tick_machine_ticks(machine: &mut Machine, master_ticks: u64) -> Option<StopReason> {
    match machine.run_master_ticks(master_ticks) {
        Ok(StopReason::CycleLimit { .. }) => None,
        Ok(reason) => Some(reason),
        Err(err) => Some(StopReason::CpuError(err.to_string())),
    }
}

/// What the audio pump needs from the machine, and nothing else.
///
/// The pump's job is a wiring job -- take the guest's own wavetable level to
/// the MIDI engines, mix them into the machine's frame, apply the host playback
/// level, queue it -- and wiring is exactly what a `Machine` cannot be asked
/// about in a unit test: reaching `0x50`/`0x51` from outside means booting DOS
/// and running SNDMIXER. Reviewers flagged `set_gain` here as untested three
/// times for that reason. Behind this seam a fake reports a known gain and a
/// known frame, so the whole pump is checked in microseconds.
pub(super) trait AudioMachine {
    /// Guest time, for placing native synthesis on the guest's clock.
    fn master_ticks(&self) -> u64;
    /// (Left, Right) linear gain for the MIDI legs, from the card's wavetable
    /// volume registers.
    fn midi_gain(&self) -> (f32, f32);
    /// One wall-clock window of the machine's own mix.
    fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)>;
}

impl AudioMachine for Machine {
    fn master_ticks(&self) -> u64 {
        Machine::master_ticks(self)
    }

    fn midi_gain(&self) -> (f32, f32) {
        Machine::midi_gain(self)
    }

    fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
        Machine::render_audio(self, native_samples)
    }
}

/// Render one wall-clock window and queue it for the sound device.
///
/// `gain` is the HOST playback level -- the volume knob, which stands for the
/// powered speakers the machine's line-out feeds. It is applied last, to the
/// finished mix, and it is the only level the host owns: everything inside the
/// chain (the card's output stage, the PC speaker's leg, the balance between
/// sources) is guest state on the CT1745 register file and is set from DOS with
/// SNDMIXER.COM.
fn pump_audio(
    machine: &mut impl AudioMachine,
    wavetable: &mut MidiEngine,
    midi_receiver: &mut MidiEngine,
    sink: &AudioSink,
    wall_dt: f64,
    debt: &mut f64,
    gain: f32,
) {
    *debt += wall_dt * OPL_NATIVE_HZ;
    let mut samples = debt.floor() as usize;
    *debt -= samples as f64;
    let max_samples = OPL_NATIVE_HZ as usize / 2;
    if samples > max_samples {
        samples = max_samples;
        *debt = 0.0;
    }
    if samples == 0 {
        return;
    }
    let guest_tick = machine.master_ticks();
    let mut pcm = machine.render_audio(samples);
    // The card's wavetable volume (CT1745 extension 0x50/0x51) is the guest's
    // control over the MIDI legs. It has to be applied here rather than inside
    // render_audio because native synthesis is staged on the host clock and
    // joins the mix only after the machine's own summing node has run.
    let midi_gain = machine.midi_gain();
    wavetable.set_gain(midi_gain);
    midi_receiver.set_gain(midi_gain);
    wavetable.render(&mut pcm, guest_tick);
    midi_receiver.render(&mut pcm, guest_tick);
    // Last, and after both MIDI legs have added themselves, so the knob covers
    // everything the host is about to play.
    apply_speaker_gain(&mut pcm, gain);
    sink.queue(&pcm);
}

/// Apply the host playback level to a finished frame buffer, in place.
///
/// The knob goes above unity, so this multiply can leave full scale, and what
/// happens then is the whole point of the function: it SATURATES. A sample that
/// wants to be louder than the sink can carry is pinned at the rail, which is
/// what a driven amplifier does. Letting the value wrap instead -- which is what
/// a plain narrowing multiply in integer arithmetic gives -- would turn a loud
/// passage's peaks inside out and read as violent distortion rather than as the
/// clipping it is.
///
/// The machine's own summing node already clamps its mix; this is the host-side
/// multiply that happens after it, on the machine's PCM and the MIDI engines'
/// additions alike.
fn apply_speaker_gain(pcm: &mut [(i16, i16)], gain: f32) {
    let scale = |sample: i16| -> i16 {
        (sample as f32 * gain)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16
    };
    for (l, r) in pcm {
        *l = scale(*l);
        *r = scale(*r);
    }
}

fn pump_midi(machine: &mut Machine, wavetable: &mut MidiEngine, midi_receiver: &mut MidiEngine) {
    while let Some(message) = machine.take_wavetable_midi_message() {
        wavetable.send(&message);
    }
    while let Some(message) = machine.take_midi_message() {
        midi_receiver.send(&message);
    }
}

fn path_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn prepare_cd_source(source: &CdSource) -> Result<(String, CdImage), SessionFailure> {
    match source {
        CdSource::Image(path) => {
            let image = load_cd_image_from_path(path)?;
            Ok((path_label(path), image))
        }
        CdSource::Folder(path) => {
            let built = izarravm_machine::build_cd_folder(path).map_err(|err| {
                SessionFailure::new(format!(
                    "could not build CD image from {}: {err}",
                    path.display()
                ))
            })?;
            let image = CdImage::from_folder(built).map_err(|err| {
                SessionFailure::new(format!(
                    "could not mount {} as a CD image: {err}",
                    path.display()
                ))
            })?;
            Ok((format!("{} (folder)", path_label(path)), image))
        }
    }
}

fn load_cd_image_from_path(path: &Path) -> Result<CdImage, SessionFailure> {
    let is_cue = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("cue"));
    if is_cue {
        let cue = std::fs::read_to_string(path).map_err(|err| {
            SessionFailure::new(format!("could not read CUE {}: {err}", path.display()))
        })?;
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        // One scanner, shared with the mount. See `cue_file_list` for why this
        // is not a second reading of the sheet done locally.
        let names: Vec<String> = izarravm_machine::cue_file_list(&cue)
            .map_err(|err| {
                SessionFailure::new(format!("could not read CUE {}: {err}", path.display()))
            })?
            .into_iter()
            .map(|(name, _file_type)| name)
            .collect();
        if names.is_empty() {
            // No FILE line: the sibling .bin is the whole disc.
            let bin_path = path.with_extension("bin");
            let bin = std::fs::read(&bin_path).map_err(|err| {
                SessionFailure::new(format!("could not read BIN {}: {err}", bin_path.display()))
            })?;
            return CdImage::from_cue(&cue, bin).map_err(SessionFailure::new);
        }
        // A CUE that names the same file across two separate FILE sections is
        // rejected by `CdImage::from_cue_files` (a repeated section is not the
        // legitimate two-tracks-one-file layout, which uses a single FILE
        // section instead). So this dedup is not load-bearing for correctness
        // -- the mount would already fail if this loop read the file twice
        // and handed in two entries. It stays because it is still correct and
        // still cheap: it saves the redundant disk read/copy for a large
        // image before `from_cue_files` ever sees the list.
        let mut seen = HashSet::with_capacity(names.len());
        let mut files = Vec::with_capacity(names.len());
        let mut folder = CueFolder::new(dir);
        // One registry for the whole disc: it is what bounds how many of
        // this sheet's tracks hold decoded audio at the same time.
        let registry = izarravm_cdaudio::Registry::new();
        for name in names {
            if !seen.insert(name.to_ascii_lowercase()) {
                // The sheet named this file again for another track; it was
                // already read once above.
                continue;
            }
            let file_path = folder.resolve(&name);
            // Look at the file before reading it. An encoded audio track is
            // measured here, never loaded: Betrayal at Krondor names 62 Ogg
            // files totalling 155 MB, which decode to about 1.5 GB, and the
            // mount has no reason to hold any of it. A file that is not a
            // container at all comes back as None and takes the original path,
            // byte for byte as before.
            let probed = izarravm_cdaudio::probe(&registry, &file_path).map_err(|err| {
                SessionFailure::new(format!(
                    "could not use {} named by {}: {err}",
                    file_path.display(),
                    path.display()
                ))
            })?;
            let source = match probed {
                Some(source) => CueSource::Audio(source),
                None => {
                    let bytes = std::fs::read(&file_path).map_err(|err| {
                        SessionFailure::new(format!(
                            "could not read {} named by {}: {err}",
                            file_path.display(),
                            path.display()
                        ))
                    })?;
                    CueSource::Raw(bytes)
                }
            };
            files.push((name, source));
        }
        CdImage::from_cue_sources(&cue, files).map_err(SessionFailure::new)
    } else {
        let bytes = std::fs::read(path).map_err(|err| {
            SessionFailure::new(format!("could not read CD image {}: {err}", path.display()))
        })?;
        CdImage::from_iso(bytes).map_err(SessionFailure::new)
    }
}

/// Resolves the names a CUE sheet writes against the files that are actually on
/// disk, tolerating a difference in case.
///
/// Rippers do not preserve case between the sheet and the files beside it, and
/// they do not have to: the sheets in circulation were written on Windows,
/// where the filesystem does not care. Betrayal at Krondor's CD1.cue names
/// `CD1.iso` and `track02.ogg` for files stored as `cd1.iso` and `Track02.ogg`
/// -- both of its shapes differ, so on any case-sensitive filesystem the disc
/// does not mount at all. `from_cue_sources` already matches sheet names to
/// supplied names case-insensitively; this is the same tolerance one layer
/// down, where the name meets the host.
///
/// The directory is listed at most once, and only if a literal name misses, so
/// a well-formed disc pays nothing. Betrayal at Krondor names 63 files, and a
/// listing per name would be 63 scans of a folder that never changes.
struct CueFolder<'a> {
    dir: &'a Path,
    /// Lowercased file name to the entry's real name. Built on the first miss.
    by_lowercase: Option<HashMap<String, std::ffi::OsString>>,
}

impl<'a> CueFolder<'a> {
    fn new(dir: &'a Path) -> Self {
        Self {
            dir,
            by_lowercase: None,
        }
    }

    /// The path `name` refers to. Falls back to the literal join when nothing
    /// in the directory matches, so the error a caller raises afterwards names
    /// the file the sheet asked for rather than one nobody wrote down.
    ///
    /// On a case-insensitive host the first line answers everything and the
    /// rest is unreachable, which is why the matching below is a function of
    /// its own: it is the half that cannot be reached from a test on Windows,
    /// and the half where a mistake costs a Linux user the whole disc.
    fn resolve(&mut self, name: &str) -> PathBuf {
        let literal = self.dir.join(name);
        if literal.exists() {
            return literal;
        }
        let index = self
            .by_lowercase
            .get_or_insert_with(|| index_by_lowercase(self.dir));
        match spelled_as(index, name) {
            Some(real) => self.dir.join(real),
            None => literal,
        }
    }
}

/// A directory's entries, keyed by their lowercased names and valued by the
/// spelling actually on disk.
///
/// An unreadable directory indexes as empty rather than failing: the caller's
/// next step is to open the file it resolved, and the error from that names the
/// file the user is missing, which is more use than one naming its folder.
fn index_by_lowercase(dir: &Path) -> HashMap<String, std::ffi::OsString> {
    let mut index = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let real = entry.file_name();
            index.insert(real.to_string_lossy().to_lowercase(), real);
        }
    }
    index
}

/// How `name` is spelled among `entries`, ignoring case. Both sides are
/// lowercased by the same rule, which is the whole of the matching and the only
/// place it can go wrong.
fn spelled_as<'a>(
    entries: &'a HashMap<String, std::ffi::OsString>,
    name: &str,
) -> Option<&'a std::ffi::OsString> {
    entries.get(&name.to_lowercase())
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else {
        "unknown panic".into()
    }
}

#[cfg(test)]
#[path = "gui_session_test.rs"]
mod tests;
