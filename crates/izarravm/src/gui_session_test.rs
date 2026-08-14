// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::prefs::MAX_VOLUME;
use izarravm_core::{MidiPortId, VideoCard};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct TestScratch(PathBuf);

impl TestScratch {
    fn new(label: &str) -> Self {
        let serial = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "izarravm-gui-session-{label}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn test_spec(scratch: &TestScratch) -> SessionSpec {
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[..2].copy_from_slice(&[0xEB, 0xFE]);
    rom[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0x00, 0x00, 0xF0]);
    SessionSpec {
        profile: MachineProfile::gsw_386(1, VideoCard::Vega),
        rom,
        c_drive: scratch.path().to_path_buf(),
        midi_config: MidiConfig::default(),
        glide_ovl: None,
        test_pattern: false,
        sink: None,
        rtc_setup: crate::cmos::RtcSetup::from_c_root(scratch.path()),
        gain: SharedGain::new(1.0),
        finalization_probe: None,
    }
}

fn wait_for(
    session: &mut GuiSession,
    mut predicate: impl FnMut(&SessionUpdate) -> bool,
) -> SessionUpdate {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let update = session.poll();
        if predicate(&update) {
            return update;
        }
        assert!(Instant::now() < deadline, "session update timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn request_kind_preserves_fifo_operation_identity() {
    let source = FloppySource(PathBuf::from("disk.img"));
    let floppy = PreparedFloppy::new("disk.img".into(), source, vec![0; 737_280]).unwrap();

    assert_eq!(
        SessionRequest::MountFloppy(floppy).kind(),
        SessionRequestKind::MountFloppy
    );
    assert_eq!(
        SessionRequest::EjectFloppy.kind(),
        SessionRequestKind::EjectFloppy
    );
    assert_eq!(
        SessionRequest::MidiConfig(MidiConfig::default()).kind(),
        SessionRequestKind::MidiConfig
    );
}

#[test]
fn prepared_floppy_rejects_an_unknown_geometry_before_submission() {
    let error = PreparedFloppy::new(
        "bad.img".into(),
        FloppySource(PathBuf::from("bad.img")),
        vec![0; 512],
    )
    .unwrap_err();

    assert!(error.to_string().contains("unsupported floppy image size"));
}

#[test]
fn cue_file_names_returns_every_file_in_sheet_order() {
    let cue = concat!(
        "FILE \"track01.bin\" BINARY\n",
        "  TRACK 01 MODE1/2352\n",
        "    INDEX 01 00:00:00\n",
        "FILE track02.bin BINARY\n",
        "  TRACK 02 AUDIO\n",
        "    INDEX 00 00:00:00\n",
        "    INDEX 01 00:02:00\n",
        "FILE \"track03.bin\" BINARY\n",
        "  TRACK 03 AUDIO\n",
        "    INDEX 01 00:00:00\n",
    );

    assert_eq!(
        cue_file_names(cue),
        vec!["track01.bin", "track02.bin", "track03.bin"]
    );
}

#[test]
fn cue_file_names_sees_the_first_file_line_behind_a_utf8_bom() {
    // This loader has its own FILE scanner, independent of `parse_cue`, and it
    // was blind to a BOM in its own way: `trimmed[..4]` over a BOM'd first line
    // is the three BOM bytes plus one 'F', which is not "FILE", so the line is
    // skipped and the file it names is never read from disk.
    //
    // Fixing `parse_cue` alone does not make the mount right, it only changes
    // the failure's shape: the parser would then correctly list both files
    // while this scanner still handed over only the second one, and the mount
    // would die with "CUE names a file that was not supplied". Both scanners
    // have to see the line.
    let cue = concat!(
        "\u{feff}FILE \"track01.bin\" BINARY\n",
        "  TRACK 01 MODE1/2352\n",
        "    INDEX 01 00:00:00\n",
        "FILE \"track02.bin\" BINARY\n",
        "  TRACK 02 AUDIO\n",
        "    INDEX 01 00:00:00\n",
    );

    assert_eq!(cue_file_names(cue), vec!["track01.bin", "track02.bin"]);
}

#[test]
fn cue_file_names_reads_a_line_whose_fourth_byte_is_mid_character() {
    // `trimmed[..4]` slices *bytes*, and the length guard above it only proves
    // there are four bytes to take, not that byte 4 begins a character. A REM
    // comment written with an em-dash right after the keyword puts a
    // three-byte sequence at bytes 3..6, so byte 4 lands inside it and the
    // slice panics -- taking the whole mount down over a comment line that
    // carries no information the loader wanted.
    let cue = concat!(
        "REM\u{2014}ripped by some tool\n",
        "FILE \"track01.bin\" BINARY\n",
        "  TRACK 01 AUDIO\n",
        "    INDEX 01 00:00:00\n",
    );

    assert_eq!(cue_file_names(cue), vec!["track01.bin"]);
}

#[test]
fn cue_file_names_is_empty_when_the_sheet_has_no_file_line() {
    let cue = concat!("  TRACK 01 MODE1/2352\n", "    INDEX 01 00:00:00\n",);

    assert!(cue_file_names(cue).is_empty());
}

#[test]
fn load_cd_image_from_path_mounts_every_file_the_cue_names() {
    // A two-FILE sheet: a MODE1/2048 data track in one file, an AUDIO track
    // in another. This exercises the actual user-facing path end to end --
    // directory joining relative to the CUE, one read per named file, and
    // the hand-off to `CdImage::from_cue_files` -- not just `cue_file_names`
    // in isolation.
    const DATA_SECTOR: usize = 2048;
    const RAW_SECTOR: usize = 2352;
    let scratch = TestScratch::new("cue-multi-file");
    let cue_path = scratch.path().join("disc.cue");
    std::fs::write(
        &cue_path,
        "FILE \"data.bin\" BINARY\n\
         TRACK 01 MODE1/2048\n\
         INDEX 01 00:00:00\n\
         FILE \"audio.bin\" BINARY\n\
         TRACK 02 AUDIO\n\
         INDEX 01 00:00:00\n",
    )
    .unwrap();
    let mut data = vec![0u8; 2 * DATA_SECTOR];
    data[0] = 0xD1;
    std::fs::write(scratch.path().join("data.bin"), &data).unwrap();
    let mut audio = vec![0u8; 3 * RAW_SECTOR];
    audio[0] = 0xA2;
    std::fs::write(scratch.path().join("audio.bin"), &audio).unwrap();

    let image = load_cd_image_from_path(&cue_path).unwrap();

    assert_eq!(image.track_count(), 2);
    assert_eq!(
        (image.tracks()[0].start_lba, image.tracks()[0].sectors),
        (0, 2)
    );
    assert_eq!(
        (image.tracks()[1].start_lba, image.tracks()[1].sectors),
        (2, 3)
    );
    assert_eq!(image.read_data_sector(0).unwrap()[0], 0xD1);
    assert_eq!(image.read_audio_frame(2).unwrap()[0], 0xA2);
}

#[test]
fn load_cd_image_from_path_errors_naming_the_missing_file() {
    let scratch = TestScratch::new("cue-missing-file");
    let cue_path = scratch.path().join("disc.cue");
    std::fs::write(
        &cue_path,
        "FILE \"present.bin\" BINARY\n\
         TRACK 01 AUDIO\n\
         INDEX 01 00:00:00\n\
         FILE \"absent.bin\" BINARY\n\
         TRACK 02 AUDIO\n\
         INDEX 01 00:00:00\n",
    )
    .unwrap();
    std::fs::write(scratch.path().join("present.bin"), vec![0u8; 2352]).unwrap();
    // absent.bin is deliberately never written.

    let error = load_cd_image_from_path(&cue_path).unwrap_err();

    assert!(
        error.to_string().contains("absent.bin"),
        "error should name the missing file: {error}"
    );
}

#[test]
fn startup_snapshot_contains_initial_media() {
    let scratch = TestScratch::new("initial-media");
    let source = FloppySource(scratch.path().join("boot.img"));
    std::fs::write(&source.0, vec![0u8; 737_280]).unwrap();
    let media = PreparedInitialMedia {
        floppy: Some(PreparedFloppy::from_source(source.clone()).unwrap()),
        cd: None,
    };

    let mut session = GuiSession::start(test_spec(&scratch), media).unwrap();
    let update = session.poll();

    assert!(update.snapshot.powered);
    assert_eq!(update.snapshot.generation, 1);
    assert_eq!(update.snapshot.floppy_label.as_deref(), Some("boot.img"));
    assert_eq!(update.snapshot.floppy_source, Some(source));
}

#[test]
fn generation_installs_initial_media_before_its_first_guest_tick() {
    let scratch = TestScratch::new("generation-initial-media");
    let floppy_source = FloppySource(scratch.path().join("boot.img"));
    std::fs::write(&floppy_source.0, vec![0u8; 737_280]).unwrap();
    let cd_path = scratch.path().join("disc.iso");
    std::fs::write(&cd_path, vec![0u8; 2048]).unwrap();
    let media = PreparedInitialMedia {
        floppy: Some(PreparedFloppy::from_source(floppy_source.clone()).unwrap()),
        cd: Some(PreparedCd::from_source(CdSource::Image(cd_path.clone())).unwrap()),
    };
    let mut generation = MachineGeneration::build(test_spec(&scratch), 1).unwrap();

    generation.initialize(media).unwrap();

    assert_eq!(generation.machine.master_ticks(), 0);
    assert_eq!(
        generation.floppy.as_ref().map(|media| &media.source),
        Some(&floppy_source)
    );
    assert_eq!(
        generation.cd.as_ref().map(|media| &media.source),
        Some(&CdSource::Image(cd_path))
    );
    assert!(generation.machine.cd_audio_state().media_present);
    generation.finalize();
}

#[test]
fn poll_acknowledges_the_frame_slot_before_the_worker_publishes_a_newer_frame() {
    let scratch = TestScratch::new("frame-mailbox");
    let mut session =
        GuiSession::start(test_spec(&scratch), PreparedInitialMedia::default()).unwrap();
    let first = wait_for(&mut session, |update| update.newest_frame.is_some())
        .newest_frame
        .unwrap();
    let worker = session.worker.as_ref().unwrap();
    assert_eq!(
        worker
            .publication
            .consumed_frame_seq
            .load(Ordering::Acquire),
        first.seq
    );

    let second = wait_for(&mut session, |update| {
        update
            .newest_frame
            .as_ref()
            .is_some_and(|frame| frame.seq > first.seq)
    })
    .newest_frame
    .unwrap();

    assert_eq!(second.generation, first.generation);
    assert!(second.seq > first.seq);
    assert_eq!(
        session
            .worker
            .as_ref()
            .unwrap()
            .publication
            .consumed_frame_seq
            .load(Ordering::Acquire),
        second.seq
    );
}

#[test]
fn requests_apply_in_fifo_order_and_publish_the_final_state() {
    let scratch = TestScratch::new("fifo");
    let mut session =
        GuiSession::start(test_spec(&scratch), PreparedInitialMedia::default()).unwrap();
    let first = session.request(SessionRequest::CdLinkedLevel(3)).unwrap();
    let second = session.request(SessionRequest::CdLinkedLevel(19)).unwrap();
    let mut applied = Vec::new();

    let update = wait_for(&mut session, |update| {
        applied.extend(update.events.iter().filter_map(|event| match event {
            SessionEvent::Applied { request_id, .. } => Some(*request_id),
            _ => None,
        }));
        applied.len() == 2
    });

    assert_eq!(applied, vec![first, second]);
    assert_eq!(update.snapshot.cd_audio.left_level, 19);
    assert_eq!(update.snapshot.cd_audio.right_level, 19);
}

#[test]
fn request_publication_couples_the_snapshot_and_correlated_event() {
    let scratch = TestScratch::new("atomic-request-publication");
    let spec = test_spec(&scratch);
    let initial = SessionSnapshot::powered_off(
        spec.c_drive,
        spec.midi_config,
        spec.profile.cpu,
        spec.profile.memory_mib,
    );
    let mut snapshot = initial.clone();
    let source = FloppySource(scratch.path().join("disk.img"));
    snapshot.floppy_label = Some("disk.img".into());
    snapshot.floppy_source = Some(source.clone());
    let event = SessionEvent::Applied {
        request_id: RequestId(41),
        kind: SessionRequestKind::MountFloppy,
        state: AppliedState::Floppy(Some(source.clone())),
    };
    let publication = Publication {
        state: Mutex::new(PublishedState {
            snapshot: initial,
            frame: None,
            events: Vec::new(),
        }),
        consumed_frame_seq: AtomicU64::new(u64::MAX),
    };

    publish_request_result(&publication, snapshot, event.clone());
    let (published, frame, events) = take_publication_update(&publication, None);

    assert_eq!(published.floppy_source, Some(source));
    assert!(frame.is_none());
    assert_eq!(events, vec![event]);
    assert!(take_publication_update(&publication, None).2.is_empty());
}

#[test]
fn failed_floppy_writeback_rejects_replacement_and_restores_the_old_mount() {
    let scratch = TestScratch::new("floppy-transaction");
    let old_source = FloppySource(scratch.path().join("missing").join("old.img"));
    let mut old =
        PreparedFloppy::new("old.img".into(), old_source.clone(), vec![0x11; 737_280]).unwrap();
    old.writeback_pending = true;
    let mut session = GuiSession::start(
        test_spec(&scratch),
        PreparedInitialMedia {
            floppy: Some(old),
            cd: None,
        },
    )
    .unwrap();
    let replacement = PreparedFloppy::new(
        "new.img".into(),
        FloppySource(scratch.path().join("new.img")),
        vec![0x22; 737_280],
    )
    .unwrap();
    let request_id = session
        .request(SessionRequest::MountFloppy(replacement))
        .unwrap();

    let update = wait_for(&mut session, |update| {
        update.events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Rejected {
                    request_id: rejected,
                    kind: SessionRequestKind::MountFloppy,
                    ..
                } if *rejected == request_id
            )
        })
    });

    assert_eq!(update.snapshot.floppy_label.as_deref(), Some("old.img"));
    assert_eq!(update.snapshot.floppy_source, Some(old_source));
}

#[test]
fn reset_remounts_failed_writeback_bytes_in_the_next_generation() {
    let scratch = TestScratch::new("reset-writeback");
    let source = FloppySource(scratch.path().join("missing").join("disk.img"));
    let mut floppy =
        PreparedFloppy::new("disk.img".into(), source.clone(), vec![0x5A; 737_280]).unwrap();
    floppy.writeback_pending = true;
    let mut session = GuiSession::start(
        test_spec(&scratch),
        PreparedInitialMedia {
            floppy: Some(floppy),
            cd: None,
        },
    )
    .unwrap();

    let report = session.reset().unwrap();
    let update = session.poll();

    assert_eq!(report.generation, 2);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(update.snapshot.floppy_label.as_deref(), Some("disk.img"));
    assert_eq!(update.snapshot.floppy_source, Some(source));
    assert!(!update.events.iter().any(|event| matches!(
        event,
        SessionEvent::WorkerFailed { .. } | SessionEvent::FinalizationFailed { .. }
    )));
}

#[test]
fn reset_boots_without_media_when_the_retained_source_disappears() {
    let scratch = TestScratch::new("reset-missing");
    let source = FloppySource(scratch.path().join("disk.img"));
    std::fs::write(&source.0, vec![0u8; 737_280]).unwrap();
    let floppy = PreparedFloppy::from_source(source.clone()).unwrap();
    let mut session = GuiSession::start(
        test_spec(&scratch),
        PreparedInitialMedia {
            floppy: Some(floppy),
            cd: None,
        },
    )
    .unwrap();
    std::fs::remove_file(&source.0).unwrap();

    let report = session.reset().unwrap();
    let update = session.poll();

    assert_eq!(report.generation, 2);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(update.snapshot.floppy_source, None);
}

#[test]
fn power_cycle_starts_empty_but_shutdown_closes_the_session() {
    let scratch = TestScratch::new("power-cycle");
    let source = FloppySource(scratch.path().join("disk.img"));
    std::fs::write(&source.0, vec![0u8; 737_280]).unwrap();
    let mut session = GuiSession::start(
        test_spec(&scratch),
        PreparedInitialMedia {
            floppy: Some(PreparedFloppy::from_source(source).unwrap()),
            cd: None,
        },
    )
    .unwrap();

    assert!(session.power_off().was_running);
    let midi = MidiConfig {
        external_port: Some(MidiPortId {
            name: "offline test".into(),
            ordinal: 0,
        }),
        ..MidiConfig::default()
    };
    let midi_request = session
        .request(SessionRequest::MidiConfig(midi.clone()))
        .unwrap();
    let offline = session.poll();
    assert!(offline.events.contains(&SessionEvent::Applied {
        request_id: midi_request,
        kind: SessionRequestKind::MidiConfig,
        state: AppliedState::Midi(midi.clone()),
    }));
    session.power_on().unwrap();
    let powered = session.poll();
    assert_eq!(powered.snapshot.generation, 2);
    assert_eq!(powered.snapshot.floppy_source, None);
    assert_eq!(powered.snapshot.midi_config, midi);

    assert!(session.shutdown().was_running);
    assert!(session.power_on().is_err());
    assert!(
        session
            .request(SessionRequest::MidiConfig(MidiConfig::default()))
            .is_err()
    );
}

#[test]
fn explicit_power_off_and_shutdown_report_finalization_failures_without_events() {
    for (label, shutdown) in [("power-off-report", false), ("shutdown-report", true)] {
        let scratch = TestScratch::new(label);
        let source = FloppySource(scratch.path().join("missing").join("disk.img"));
        let mut floppy =
            PreparedFloppy::new("disk.img".into(), source, vec![0x5A; 737_280]).unwrap();
        floppy.writeback_pending = true;
        let mut session = GuiSession::start(
            test_spec(&scratch),
            PreparedInitialMedia {
                floppy: Some(floppy),
                cd: None,
            },
        )
        .unwrap();

        let report = if shutdown {
            session.shutdown()
        } else {
            session.power_off()
        };
        assert_eq!(report.failures.len(), 1);
        assert!(!session.poll().events.iter().any(|event| matches!(
            event,
            SessionEvent::WorkerFailed { .. } | SessionEvent::FinalizationFailed { .. }
        )));
    }
}

#[test]
fn worker_failure_is_reported_and_the_generation_is_finalized() {
    let scratch = TestScratch::new("worker-failure");
    let mut session =
        GuiSession::start(test_spec(&scratch), PreparedInitialMedia::default()).unwrap();
    session
        .worker
        .as_ref()
        .unwrap()
        .commands
        .send(WorkerCommand::Panic)
        .unwrap();

    let update = wait_for(&mut session, |update| {
        update
            .events
            .iter()
            .any(|event| matches!(event, SessionEvent::WorkerFailed { generation: 1, .. }))
    });

    assert!(!update.snapshot.powered);
    assert!(session.worker.is_none());
}

#[test]
fn reset_shutdown_and_drop_finalize_each_generation_once() {
    let scratch = TestScratch::new("session-finalizers");
    let probe = Arc::new(AtomicU64::new(0));
    let mut spec = test_spec(&scratch);
    spec.finalization_probe = Some(Arc::clone(&probe));
    let mut session = GuiSession::start(spec, PreparedInitialMedia::default()).unwrap();

    session.reset().unwrap();
    assert_eq!(probe.load(Ordering::Relaxed), 1);
    session.shutdown();
    assert_eq!(probe.load(Ordering::Relaxed), 2);
    session.shutdown();
    drop(session);
    assert_eq!(probe.load(Ordering::Relaxed), 2);

    let mut spec = test_spec(&scratch);
    spec.finalization_probe = Some(Arc::clone(&probe));
    let session = GuiSession::start(spec, PreparedInitialMedia::default()).unwrap();
    drop(session);
    assert_eq!(probe.load(Ordering::Relaxed), 3);
}

#[test]
fn panic_rejects_an_outstanding_request_after_completed_events() {
    let scratch = TestScratch::new("panic-order");
    let source = FloppySource(scratch.path().join("missing").join("disk.img"));
    let mut floppy = PreparedFloppy::new("disk.img".into(), source, vec![0x5A; 737_280]).unwrap();
    floppy.writeback_pending = true;
    let mut session = GuiSession::start(
        test_spec(&scratch),
        PreparedInitialMedia {
            floppy: Some(floppy),
            cd: None,
        },
    )
    .unwrap();
    let (entered_tx, entered_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::channel();
    session
        .worker
        .as_ref()
        .unwrap()
        .commands
        .send(WorkerCommand::Gate {
            entered: entered_tx,
            release: release_rx,
        })
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let completed = session.request(SessionRequest::CdStop).unwrap();
    session
        .worker
        .as_ref()
        .unwrap()
        .commands
        .send(WorkerCommand::Panic)
        .unwrap();
    let pending = session.request(SessionRequest::CdPlay).unwrap();
    release_tx.send(()).unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut events = Vec::new();
    loop {
        events.extend(session.poll().events);
        if events.iter().any(|event| {
            matches!(
                event,
                SessionEvent::Rejected { request_id, .. } if *request_id == pending
            )
        }) {
            break;
        }
        assert!(Instant::now() < deadline, "session update timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
    let completed_at = events
        .iter()
        .position(|event| {
            matches!(event, SessionEvent::Applied { request_id, .. } if *request_id == completed)
        })
        .unwrap();
    let failure_at = events
        .iter()
        .position(|event| matches!(event, SessionEvent::WorkerFailed { .. }))
        .unwrap();
    let finalization_at = events
        .iter()
        .position(|event| matches!(event, SessionEvent::FinalizationFailed { .. }))
        .unwrap();
    let rejected_at = events
        .iter()
        .position(|event| {
            matches!(event, SessionEvent::Rejected { request_id, .. } if *request_id == pending)
        })
        .unwrap();

    assert!(completed_at < failure_at);
    assert!(failure_at < finalization_at);
    assert!(finalization_at < rejected_at);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, SessionEvent::WorkerFailed { generation: 1, .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                SessionEvent::FinalizationFailed { generation: 1, .. }
            ))
            .count(),
        1
    );
}

#[test]
fn applied_media_state_survives_power_off_snapshot_clearing() {
    let scratch = TestScratch::new("power-off-applied-state");
    let source = FloppySource(scratch.path().join("disk.img"));
    std::fs::write(&source.0, vec![0u8; 737_280]).unwrap();
    let floppy = PreparedFloppy::from_source(source.clone()).unwrap();
    let mut session =
        GuiSession::start(test_spec(&scratch), PreparedInitialMedia::default()).unwrap();
    let request_id = session
        .request(SessionRequest::MountFloppy(floppy))
        .unwrap();

    session.power_off();
    let update = session.poll();

    assert!(!update.snapshot.powered);
    assert_eq!(update.snapshot.floppy_source, None);
    assert!(update.events.contains(&SessionEvent::Applied {
        request_id,
        kind: SessionRequestKind::MountFloppy,
        state: AppliedState::Floppy(Some(source)),
    }));
}

#[test]
fn panic_guard_runs_the_generation_finalizer_once() {
    let scratch = TestScratch::new("panic-finalizer");
    let mut generation = MachineGeneration::build(test_spec(&scratch), 7).unwrap();
    let probe = Arc::new(AtomicU64::new(0));
    generation.finalization_probe = Some(Arc::clone(&probe));

    let (result, _) = run_generation_guarded(generation, |_| panic!("guard test"));

    assert!(result.is_err());
    assert_eq!(probe.load(Ordering::Relaxed), 1);
}

#[test]
fn frame_slot_waits_for_poll_acknowledgement_before_replacement() {
    assert!(should_publish_frame(10, u64::MAX, u64::MAX, None, None));
    assert!(!should_publish_frame(11, 10, u64::MAX, None, None));
    assert!(should_publish_frame(11, 10, 10, None, None));
}

#[test]
fn refill_credit_clamps_a_stall() {
    let cap = MASTER_CLOCK_HZ / 20;
    // From empty, a normal ~15 ms slice yields its full wall-time worth.
    assert_eq!(
        refill_credit(0, Duration::from_millis(15), cap),
        (MASTER_CLOCK_HZ * 15 / 1000) as i64
    );
    // A long stall is clamped to the cap, so the backlog is forgiven, not banked.
    assert_eq!(
        refill_credit(0, Duration::from_millis(500), cap),
        cap as i64
    );
}

#[test]
fn pacing_sleeps_only_when_the_guest_is_caught_up() {
    assert!(emulation_should_sleep(-1, false));
    assert!(emulation_should_sleep(0, false));
    assert!(!emulation_should_sleep(1, false));
    assert!(emulation_should_sleep(1, true));
}

#[test]
fn fast_modes_run_one_bounded_quantum_while_accurate_modes_use_the_credit() {
    let credit = (MASTER_CLOCK_HZ / 20) as i64;
    assert_eq!(execution_budget(credit, true), FAST_EMU_QUANTUM_TICKS);
    assert_eq!(execution_budget(credit, false), credit as u64);
    assert_eq!(execution_budget(-1, true), 0);
}

#[test]
fn only_halted_guests_receive_device_only_top_up() {
    let budget = MASTER_CLOCK_HZ / 20;
    let executed = FAST_EMU_QUANTUM_TICKS;
    assert_eq!(halted_device_top_up(budget, executed, false), 0);
    assert_eq!(
        halted_device_top_up(budget, executed, true),
        budget - executed
    );
}

#[test]
fn pacing_settlement_distinguishes_ahead_and_behind_execution() {
    let cap = MASTER_CLOCK_HZ / 20;
    let quantum = FAST_EMU_QUANTUM_TICKS;

    let ahead = settle_credit(quantum as i64, quantum * 2, Duration::ZERO, cap);
    assert!(ahead < 0);
    assert!(emulation_should_sleep(ahead, false));

    let behind = settle_credit(quantum as i64, quantum, Duration::from_millis(3), cap);
    assert_eq!(behind, (MASTER_CLOCK_HZ * 3 / 1000) as i64);
    assert!(!emulation_should_sleep(behind, false));
}

#[test]
fn pacing_settlement_caps_catch_up_and_preserves_io_debt() {
    let cap = MASTER_CLOCK_HZ / 20;
    assert_eq!(
        settle_credit(0, 0, Duration::from_millis(500), cap),
        cap as i64
    );

    let credit = FAST_EMU_QUANTUM_TICKS as i64;
    let disk_jump = MASTER_CLOCK_HZ / 5;
    let after_disk = settle_credit(credit, disk_jump, Duration::from_millis(1), cap);
    assert!(after_disk < 0);
    assert_eq!(execution_budget(after_disk, true), 0);
}

#[test]
fn runtime_profile_accumulates_host_phase_times() {
    let mut metrics = RuntimeProfileMetrics::default();
    metrics.record_work(
        Duration::from_nanos(2),
        Duration::from_nanos(3),
        Duration::from_nanos(5),
    );
    metrics.record_work(
        Duration::from_nanos(7),
        Duration::from_nanos(11),
        Duration::from_nanos(13),
    );
    metrics.record_sleep(Duration::from_nanos(17));

    assert_eq!(metrics.emulation_work_wall_ns, 9);
    assert_eq!(metrics.host_audio_mix_queue_wall_ns, 14);
    assert_eq!(metrics.frame_conversion_publish_wall_ns, 18);
    assert_eq!(metrics.throttle_sleep_wall_ns, 17);
}

#[test]
fn runtime_profile_tracks_catchup_credit_and_throttle_ahead_independently() {
    let mut metrics = RuntimeProfileMetrics::default();
    metrics.observe_credit(120);
    metrics.observe_credit(-75);
    metrics.observe_credit(40);
    metrics.observe_credit(-200);

    assert_eq!(metrics.current_pacing_credit_ticks, -200);
    assert_eq!(metrics.max_catchup_credit_ticks, 120);
    assert_eq!(metrics.max_throttle_ahead_ticks, 200);
}

#[test]
fn runtime_profile_counts_latest_frame_publication_and_backpressure() {
    let mut metrics = RuntimeProfileMetrics::default();
    metrics.record_frame(0, u64::MAX, true);
    metrics.record_frame(12, 10, true);
    metrics.record_frame(13, 12, false);
    metrics.record_backpressure(Duration::from_millis(25));

    assert_eq!(metrics.frames_produced, 2);
    assert_eq!(metrics.frames_skipped, 1);
    assert_eq!(
        metrics.presentation_backpressure_wall_ns,
        duration_ns(Duration::from_millis(25))
    );
}

#[test]
fn runtime_profile_json_has_stable_fields_and_derived_values() {
    let metrics = RuntimeProfileMetrics {
        emulation_work_wall_ns: 2,
        host_audio_mix_queue_wall_ns: 3,
        frame_conversion_publish_wall_ns: 5,
        guest_master_ticks: MASTER_CLOCK_HZ / 2,
        current_pacing_credit_ticks: -(MASTER_CLOCK_HZ as i64 / 4),
        max_catchup_credit_ticks: MASTER_CLOCK_HZ / 2,
        max_throttle_ahead_ticks: MASTER_CLOCK_HZ / 4,
        frames_produced: 7,
        frames_skipped: 2,
        ..RuntimeProfileMetrics::default()
    };
    let audio = AudioDebugSnapshot {
        underruns_after_prefill: 11,
        overruns: 13,
        late_callbacks: 17,
        ..AudioDebugSnapshot::default()
    };
    let total_metrics = RuntimeProfileMetrics {
        guest_master_ticks: MASTER_CLOCK_HZ * 3 / 2,
        ..metrics
    };
    let value = serde_json::to_value(RuntimeProfileReport::new(
        "interval",
        4,
        Duration::from_secs(1),
        metrics,
        Duration::from_secs(2),
        total_metrics,
        Some(audio),
    ))
    .unwrap();

    assert_eq!(value["schema"], RUNTIME_PROFILE_SCHEMA);
    assert_eq!(value["scope"], "interval");
    assert_eq!(value["active_work_wall_ns"], 10);
    assert_eq!(value["guest_realtime_factor"], 0.5);
    assert_eq!(value["uncapped_wall_guest_lag_ticks"], MASTER_CLOCK_HZ / 2);
    assert_eq!(value["uncapped_wall_guest_lag_seconds"], 0.5);
    assert_eq!(value["total_guest_realtime_factor"], 0.75);
    assert_eq!(
        value["uncapped_total_wall_guest_lag_ticks"],
        MASTER_CLOCK_HZ / 2
    );
    assert_eq!(value["current_catchup_credit_ticks"], 0);
    assert_eq!(value["current_throttle_ahead_ticks"], MASTER_CLOCK_HZ / 4);
    assert_eq!(value["current_throttle_ahead_seconds"], 0.25);
    assert_eq!(value["max_catchup_credit_seconds"], 0.5);
    assert_eq!(value["frames_produced"], 7);
    assert_eq!(value["frames_skipped"], 2);
    assert_eq!(value["audio_underruns_after_prefill"], 11);
    assert_eq!(value["audio_overruns"], 13);
    assert_eq!(value["audio_late_callbacks"], 17);
    assert!(value.get("audio_queue_lifetime_min_depth").is_some());
    assert!(
        value
            .get("audio_lifetime_max_callback_lateness_us")
            .is_some()
    );
}

#[test]
fn runtime_profile_final_audio_counts_start_at_profile_enable() {
    let baseline = AudioDebugSnapshot {
        frames_produced: 100,
        underruns_after_prefill: 7,
        overruns: 3,
        ..AudioDebugSnapshot::default()
    };
    let current = AudioDebugSnapshot {
        frames_produced: 140,
        underruns_after_prefill: 9,
        overruns: 8,
        ..AudioDebugSnapshot::default()
    };

    let delta = audio_snapshot_since(Some(current), Some(baseline)).unwrap();
    assert_eq!(delta.frames_produced, 40);
    assert_eq!(delta.underruns_after_prefill, 2);
    assert_eq!(delta.overruns, 5);
}

#[test]
fn frame_publish_waits_for_ack_then_selects_the_newest_frame() {
    let published_seq = 10;
    let published_generation = Some(100);

    assert!(!should_publish_frame(
        11,
        published_seq,
        9,
        Some(101),
        published_generation,
    ));
    assert!(!should_publish_frame(
        12,
        published_seq,
        9,
        Some(102),
        published_generation,
    ));
    assert!(should_publish_frame(
        12,
        published_seq,
        published_seq,
        Some(102),
        published_generation,
    ));
}

#[test]
fn frame_publish_skips_static_graphics_after_ack() {
    assert!(should_publish_frame(0, u64::MAX, u64::MAX, Some(1), None));
    assert!(!should_publish_frame(11, 10, 10, Some(7), Some(7)));
    assert!(should_publish_frame(11, 10, 10, None, None));
}

#[test]
fn speed_sample_marks_exactly_ninety_percent_hlt_as_idle() {
    let wall = Duration::from_secs(1);
    assert!(!speed_sample(899, 1_000, wall).1);
    assert!(speed_sample(900, 1_000, wall).1);
    assert!(!speed_sample(0, 1_000, wall).1, "I/O wait is not HLT idle");
}

#[test]
fn speed_sample_caps_active_throughput_at_realtime() {
    let (ratio, idle) = speed_sample(0, MASTER_CLOCK_HZ.saturating_mul(2), Duration::from_secs(1));
    assert_eq!(ratio, 1.0);
    assert!(!idle);
}

#[test]
fn speed_sample_reports_realtime_for_a_guest_that_halts() {
    // Prince of Persia on a 486 idles ~20% of every second waiting on its frame
    // timer while the machine keeps perfect real time. Measured: guest realtime
    // factor 1.000 against a readout of 0.801, which is exactly 1 - the halted
    // share. The readout must follow delivered guest time, not retired work.
    let (ratio, idle) = speed_sample(MASTER_CLOCK_HZ / 5, MASTER_CLOCK_HZ, Duration::from_secs(1));
    assert_eq!(ratio, 1.0);
    assert!(!idle, "20% halted is not idle");
}

#[test]
fn disk_overshoot_holds_the_guest() {
    let cap = MASTER_CLOCK_HZ / 20;
    // A read that ran ~190 ms past its budget leaves credit deep in debt.
    let mut credit: i64 = -(MASTER_CLOCK_HZ as i64) / 5;
    // One short slice cannot lift it out of debt, so the guest's budget stays
    // zero: it waits in wall-clock time.
    credit = refill_credit(credit, Duration::from_millis(1), cap);
    assert!(credit < 0);
    assert_eq!(credit.max(0) as u64, 0, "no budget while in disk debt");
    // After enough wall time the debt clears and the guest runs again.
    credit = refill_credit(credit, Duration::from_millis(500), cap);
    assert!(credit > 0, "debt repaid once wall-clock catches up");
}

#[test]
fn live_mode_switch_debits_credit_in_master_ticks() {
    let mut rom = vec![0u8; izarravm_machine::BIOS_ROM_SIZE];
    rom[..15].copy_from_slice(&[
        0xB0, 0x01, 0xE6, 0xE1, // 486
        0xB0, 0x00, 0xE6, 0xE1, // 386
        0xB0, 0x03, 0xE6, 0xE1, // 386-slow
        0xFA, 0xEB, 0xFE, // cli; jmp $
    ]);
    rom[0xFFF0..0xFFF5].copy_from_slice(&[0xEA, 0x00, 0x00, 0x00, 0xF0]);
    let mut machine = Machine::new(
        MachineProfile::gsw_386(1, izarravm_core::VideoCard::Vega),
        rom,
    )
    .unwrap();
    machine.set_mode(GswMode::Gsw586);
    let budget = MASTER_CLOCK_HZ / 1000;
    let mut credit = budget as i64;
    let before = machine.master_ticks();

    assert_eq!(tick_machine_ticks(&mut machine, budget), None);
    let ran = machine.master_ticks() - before;
    credit -= i64::try_from(ran).unwrap();

    assert_eq!(machine.active_mode(), GswMode::Gsw386Slow);
    assert!(credit <= 0, "the full fixed-time budget was debited");
    assert!(
        credit > -(100 * 900),
        "credit debt is only final-instruction overshoot, not mixed clock units"
    );
}

/// The audio pump carries the GUEST's wavetable level to the MIDI engines.
///
/// This link had no test and reviewers flagged it repeatedly, for a structural
/// reason: the level lives in CT1745 registers `0x50`/`0x51`, the only way to
/// move them is a guest `OUT`, and both registers power on at unity -- so a
/// test driving a real `Machine` reads (1.0, 1.0) whether the pump wires them
/// up or not, and passes on a deleted line. The `AudioMachine` seam exists to
/// make the wiring observable: this fake reports a level no register default
/// could produce, and both engines have to be carrying it afterwards.
#[test]
fn the_audio_pump_hands_the_guests_wavetable_level_to_both_midi_engines() {
    /// Reports one distinctive gain and one distinctive frame.
    struct FakeMachine {
        gain: (f32, f32),
        frame: (i16, i16),
        rendered: usize,
    }

    impl AudioMachine for FakeMachine {
        fn master_ticks(&self) -> u64 {
            0
        }

        fn midi_gain(&self) -> (f32, f32) {
            self.gain
        }

        fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
            self.rendered += native_samples;
            vec![self.frame; native_samples]
        }
    }

    let mut machine = FakeMachine {
        // Asymmetric on purpose: a pump that passed one channel to both, or
        // swapped them, would survive a symmetric level.
        gain: (0.25, 0.75),
        frame: (1_000, -2_000),
        rendered: 0,
    };
    let config = MidiConfig::default();
    let mut wavetable = MidiEngine::open_wavetable(&config);
    let mut receiver = MidiEngine::open_receiver(&config);
    assert_eq!(
        (wavetable.gain(), receiver.gain()),
        ((1.0, 1.0), (1.0, 1.0)),
        "engines start at unity, so the assertions below cannot pass by default"
    );

    let sink = AudioSink::detached();
    let mut debt = 0.0;
    // 10 ms of wall time, at unity host gain.
    pump_audio(
        &mut machine,
        &mut wavetable,
        &mut receiver,
        &sink,
        0.01,
        &mut debt,
        1.0,
    );

    assert!(machine.rendered > 0, "the pump must have rendered a window");
    assert_eq!(
        wavetable.gain(),
        (0.25, 0.75),
        "the P300 wavetable leg takes the card's 0x50/0x51 level"
    );
    assert_eq!(
        receiver.gain(),
        (0.25, 0.75),
        "so does the P330 receiver: both are the card's MIDI legs"
    );

    let queued = sink.take_queued_frames();
    assert_eq!(
        queued.len(),
        machine.rendered,
        "every frame the machine rendered reaches the sink"
    );
    assert!(
        queued.iter().all(|frame| *frame == (1_000, -2_000)),
        "at unity host gain the machine's own mix is queued untouched"
    );
}

/// The host volume knob is applied to the finished mix, and to nothing else.
///
/// It stands for the powered speakers the machine's line-out feeds, which is
/// why it is the LAST thing in the chain and why the headless capture
/// deliberately does not record it. A gain that leaked into the machine instead
/// would change what `render_audio` returns, and the WAV with it.
#[test]
fn the_host_volume_knob_scales_the_finished_mix_only() {
    struct FlatMachine;

    impl AudioMachine for FlatMachine {
        fn master_ticks(&self) -> u64 {
            0
        }

        fn midi_gain(&self) -> (f32, f32) {
            (1.0, 1.0)
        }

        fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
            vec![(8_000, -8_000); native_samples]
        }
    }

    let config = MidiConfig::default();
    let mut wavetable = MidiEngine::open_wavetable(&config);
    let mut receiver = MidiEngine::open_receiver(&config);
    let sink = AudioSink::detached();
    let mut debt = 0.0;
    pump_audio(
        &mut FlatMachine,
        &mut wavetable,
        &mut receiver,
        &sink,
        0.01,
        &mut debt,
        0.5,
    );

    let queued = sink.take_queued_frames();
    assert!(!queued.is_empty());
    assert!(
        queued.iter().all(|frame| *frame == (4_000, -4_000)),
        "the knob halves the mix on its way out, symmetrically"
    );
}

/// Past unity the knob can ask for more than the sink can carry, and what it
/// gets then is the rail, not a wrapped sample.
///
/// The multiply is the last thing in the host chain and it is the one place a
/// gain above 1.0 can leave full scale. A narrowing integer multiply would wrap:
/// 20000 * 5 is 100000, which truncated to 16 bits is -31072 -- a loud passage's
/// positive peaks would come back out inverted and near the negative rail, which
/// is a far uglier noise than the clipping it is pretending not to be. Drive it
/// well past the rail in both directions and require saturation.
#[test]
fn the_host_volume_knob_saturates_instead_of_wrapping_above_unity() {
    // Chosen so a 16-bit wrap lands somewhere unmistakable rather than near the
    // rail by luck: 20_000 * 5 = 100_000, and 100_000 as i16 is -31_072.
    let mut pcm = vec![(20_000i16, -20_000i16), (1_000, -1_000), (0, 0)];
    apply_speaker_gain(&mut pcm, 5.0);

    assert_eq!(
        pcm[0],
        (i16::MAX, i16::MIN),
        "a sample driven past full scale is pinned at the rail, not wrapped"
    );
    assert_eq!(
        pcm[1],
        (5_000, -5_000),
        "a sample with room to grow is simply amplified"
    );
    assert_eq!(pcm[2], (0, 0), "silence stays silence at any gain");

    // The whole travel of the knob is safe, not just the value above.
    for frame in [(i16::MAX, i16::MIN), (i16::MIN, i16::MAX)] {
        let mut rails = vec![frame];
        apply_speaker_gain(&mut rails, MAX_VOLUME);
        let (l, r) = rails[0];
        assert!(
            (l == i16::MAX || l == i16::MIN) && (r == i16::MAX || r == i16::MIN),
            "full-scale input at the top of the knob stays at a rail, got {l},{r}"
        );
        assert_eq!(
            l.signum(),
            frame.0.signum(),
            "and on the same side of zero it went in on"
        );
        assert_eq!(r.signum(), frame.1.signum());
    }
}

/// The gain the pump applies covers the MIDI legs, because it runs after them.
///
/// The engines add themselves INTO the machine's buffer, so a knob applied
/// before that call would play the synths at a level nobody set. There is no way
/// to make a `MidiEngine` produce a sample in a test -- an adapter needs a real
/// synth behind it -- so this pins the seam the other way round: everything the
/// pump queues has been through `apply_speaker_gain`, whoever put it there.
#[test]
fn the_pump_applies_the_knob_after_the_midi_legs_have_been_added() {
    struct FlatMachine;

    impl AudioMachine for FlatMachine {
        fn master_ticks(&self) -> u64 {
            0
        }

        fn midi_gain(&self) -> (f32, f32) {
            (1.0, 1.0)
        }

        fn render_audio(&mut self, native_samples: usize) -> Vec<(i16, i16)> {
            vec![(20_000, -20_000); native_samples]
        }
    }

    let config = MidiConfig::default();
    let mut wavetable = MidiEngine::open_wavetable(&config);
    let mut receiver = MidiEngine::open_receiver(&config);
    let sink = AudioSink::detached();
    let mut debt = 0.0;
    pump_audio(
        &mut FlatMachine,
        &mut wavetable,
        &mut receiver,
        &sink,
        0.01,
        &mut debt,
        MAX_VOLUME,
    );

    let queued = sink.take_queued_frames();
    assert!(!queued.is_empty());
    assert!(
        queued.iter().all(|frame| *frame == (i16::MAX, i16::MIN)),
        "the buffer the engines rendered into is what the knob then scales, \
         saturating"
    );
}
