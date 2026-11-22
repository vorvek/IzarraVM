// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! One CUE audio track: its length, its decoded bytes, and the worker that
//! fills them.
//!
//! Decoding happens on a background thread rather than inline in `frame()`,
//! because `frame()` is called from the mixer pull on the emulation thread. A
//! four-minute track is on the order of a second of CPU; spending that inline
//! would freeze the whole machine at the start of every song, which the player
//! hears as the game stuttering rather than as a gap in the music.
//!
//! `ready` lives inside the same mutex as the buffer, not beside it as an
//! atomic. `frame()` already takes the lock to copy 2352 bytes, so this costs
//! nothing, and it closes a race that is not survivable: with the count read
//! outside the lock, an eviction landing between the read and the copy leaves
//! `frame()` slicing a cleared buffer out of range, and that panic happens on
//! the emulation thread under `panic = "abort"`.

use crate::decode::{STAGE_SECTORS, decode_chunked};
use crate::probe::{CdAudioError, TrackInfo, probe_info};
use izarravm_core::{AUDIO_FRAME_BYTES, AudioTrackSource};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The buffer and how much of it is real, together under one lock.
#[derive(Default)]
struct Filled {
    pcm: Vec<u8>,
    /// Whole sectors at the front of `pcm` that hold decoded audio. Everything
    /// from here on is zero and must not be served: silence at the head of a
    /// track that is about to arrive would be indistinguishable from silence
    /// the composer wrote.
    ready: u32,
    /// Which decode owns `pcm`. Eviction bumps it, and a worker writes only
    /// while its own number is still the current one, so a worker that was told
    /// to stop can never write into the buffer of the decode that replaced it.
    generation: u64,
    /// The worker of this generation has stopped writing: the track is whole,
    /// or the decode failed and what is there is all there will be.
    done: bool,
}

/// Written by hand for the reason the `AudioTrackSource` contract gives: a
/// derived one prints every byte of the buffer the first time anything formats
/// a track, and a track is tens of megabytes of samples. `CdImage` derives
/// `Debug`, so a single `{:?}` of a mounted disc would reach this.
impl std::fmt::Debug for Filled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Filled")
            .field("bytes", &self.pcm.len())
            .field("ready", &self.ready)
            .field("generation", &self.generation)
            .field("done", &self.done)
            .finish()
    }
}

#[derive(Debug)]
struct Shared {
    filled: Mutex<Filled>,
    /// Set when a worker is running or has finished, so the decode is started
    /// exactly once per residency. Eviction clears it.
    started: AtomicBool,
    /// Mirror of `Filled::generation`, written only under the `filled` lock. A
    /// worker reads it without the lock to learn that it has been replaced.
    generation: AtomicU64,
    /// Workers actually spawned for this track. `started` is meant to hold this
    /// at one for the track's whole life, and nothing about the audio would
    /// look wrong if it did not: every worker decodes the same file to the
    /// same bytes. What would go wrong is the machine: `frame` starts a worker
    /// on each miss, and a four-minute track is eighteen thousand misses before
    /// the first sector lands, so the failure is thousands of threads each
    /// decoding the same file, not a sound anyone can hear. Counted so a test
    /// can see it.
    workers: AtomicU32,
    /// Workers of this track that are alive right now. Raised before the thread
    /// spawns and lowered by the worker's guard, so a test cannot read zero
    /// while a worker is still on its way in.
    live: AtomicU32,
    /// When this track was last asked for a frame, on [`TOUCH_CLOCK`]'s scale.
    ///
    /// Eviction takes the least recently touched track, and "touched" has to
    /// mean every read rather than every decode start. A track being played
    /// from a buffer it already holds serves nothing but hits, so it would
    /// never refresh its place, and the next admission would find it looking
    /// like the oldest resident and take the buffer out from under the mixer,
    /// which then re-decodes the song from its beginning, mid-play.
    last_touch: AtomicU64,
}

/// Source of the ordering [`Shared::last_touch`] records. A counter rather than
/// a clock because only the order matters, and a monotonic integer cannot step
/// backwards the way a wall clock can.
static TOUCH_CLOCK: AtomicU64 = AtomicU64::new(0);

/// A CUE audio track backed by an encoded file on the host.
#[derive(Debug)]
pub struct DecodedTrack {
    path: PathBuf,
    info: TrackInfo,
    shared: Arc<Shared>,
    /// The mount's residency bookkeeping, if this track belongs to one. A track
    /// built on its own keeps its buffer for as long as it lives.
    registry: Option<Registry>,
}

impl DecodedTrack {
    /// Measure `path` and prepare a track for it.
    ///
    /// Nothing is decoded here: this runs at mount time, once per audio file,
    /// and a disc's worth of decoding at mount would cost seconds and hundreds
    /// of megabytes for music the guest may never ask for.
    pub fn new(path: PathBuf) -> Result<Self, CdAudioError> {
        let info = probe_info(&path)?.ok_or_else(|| CdAudioError::Decode {
            path: path.display().to_string(),
            message: "is not an audio container".to_string(),
        })?;
        Ok(Self::with_info(path, info))
    }

    /// Prepare a track from a measurement already taken. The mount has just
    /// probed every file the sheet names, and probing each a second time here
    /// would double a cost that is already the whole of a disc's mount time.
    pub fn with_info(path: PathBuf, info: TrackInfo) -> Self {
        Self {
            path,
            info,
            registry: None,
            shared: Arc::new(Shared {
                filled: Mutex::new(Filled::default()),
                started: AtomicBool::new(false),
                generation: AtomicU64::new(0),
                workers: AtomicU32::new(0),
                live: AtomicU32::new(0),
                last_touch: AtomicU64::new(0),
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes this track currently holds decoded. Zero before its first touch
    /// and after an eviction.
    pub fn resident_bytes(&self) -> usize {
        self.shared
            .filled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pcm
            .len()
    }

    /// Kick the worker off if it is not already running.
    ///
    /// Called from `frame()`, so it must not block: everything expensive
    /// happens on the new thread.
    fn ensure_started(&self) {
        if self.shared.started.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(registry) = &self.registry {
            // Admitted first, so that a full thread cap cannot keep old tracks
            // resident: the least recently touched one loses its buffer and
            // its worker is told to stop even when no new thread can start.
            registry.admit(&self.shared);
            if registry.live_workers() >= MAX_LIVE_WORKERS {
                // No thread and no buffer now. The track stays admitted and
                // the next touch tries again, so this frame is silence.
                self.shared.started.store(false, Ordering::SeqCst);
                return;
            }
        }
        let bytes = self.info.sectors as usize * AUDIO_FRAME_BYTES;
        let my_gen = {
            let mut filled = self.shared.filled.lock().unwrap_or_else(|e| e.into_inner());
            filled.pcm = vec![0u8; bytes];
            filled.ready = 0;
            filled.done = false;
            filled.generation
        };
        let path = self.path.clone();
        let info = self.info;
        #[cfg(test)]
        let gate = self.registry.as_ref().and_then(|r| r.gate.clone());
        self.shared.workers.fetch_add(1, Ordering::Relaxed);
        // Raised here, before the spawn, so a count read right after `frame`
        // returns already includes the worker. The guard lowers both counts,
        // also when the spawn fails and the closure is dropped unrun.
        self.shared.live.fetch_add(1, Ordering::SeqCst);
        let registry_live = self.registry.as_ref().map(|r| Arc::clone(&r.live));
        if let Some(live) = &registry_live {
            live.fetch_add(1, Ordering::SeqCst);
        }
        let guard = WorkerGuard {
            shared: Some(Arc::clone(&self.shared)),
            registry_live,
        };
        let spawned = std::thread::Builder::new()
            .name("cd-audio-decode".to_string())
            .spawn(move || {
                if let Some(shared) = guard.shared.as_ref() {
                    decode_worker(
                        &path,
                        info,
                        shared,
                        my_gen,
                        #[cfg(test)]
                        gate.as_deref(),
                    );
                }
            });
        if spawned.is_err() {
            self.shared.workers.fetch_sub(1, Ordering::Relaxed);
            // Nothing will ever fill this track, so withdraw it: it must not
            // keep a full-size buffer that no worker will write. Releasing also
            // clears `started`, which lets the next `frame()` try again rather
            // than leaving the track silent for a condition that is usually
            // momentary. The closure was dropped unrun, so its guard has
            // already lowered the live counts.
            if let Some(registry) = &self.registry {
                registry.forget(&self.shared);
            }
            release(&self.shared);
        }
    }
}

/// Owned by a worker thread. Lowers the live counts when the worker ends, and
/// only after it has let go of the track state, so a registry count of zero
/// means no worker still holds any track.
struct WorkerGuard {
    shared: Option<Arc<Shared>>,
    registry_live: Option<Arc<AtomicUsize>>,
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.take() {
            // The count lives inside `Shared`, so it has to go down first.
            shared.live.fetch_sub(1, Ordering::SeqCst);
            drop(shared);
        }
        if let Some(live) = &self.registry_live {
            live.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Give back a track's buffer and tell its worker to stop.
///
/// The generation bump is what stops the worker: it writes only while its own
/// number is current. Clearing `started` last lets the next miss start a new
/// worker with the new number, so two workers can exist on one track for a
/// moment but only one of them can write.
fn release(shared: &Shared) {
    let mut filled = shared.filled.lock().unwrap_or_else(|e| e.into_inner());
    filled.pcm = Vec::new();
    filled.ready = 0;
    filled.done = false;
    filled.generation += 1;
    shared
        .generation
        .store(filled.generation, Ordering::Release);
    shared.started.store(false, Ordering::SeqCst);
}

/// Decode the whole file, publishing each newly finished run of sectors.
fn decode_worker(
    path: &Path,
    info: TrackInfo,
    shared: &Arc<Shared>,
    my_gen: u64,
    #[cfg(test)] gate: Option<&Gate>,
) {
    let stopped = || shared.generation.load(Ordering::Acquire) != my_gen;
    #[cfg(test)]
    if let Some(gate) = gate {
        gate.wait(&stopped);
    }
    // A worker replaced before it began has nothing to decode, and must not
    // open the file: on a slow share that open is the cost eviction avoids.
    if stopped() {
        return;
    }
    // Decode through a small staging buffer and copy each chunk under the
    // lock. Holding the mutex across a decode call would block the mixer pull
    // for as long as a packet takes to decode.
    let mut published = 0usize;
    let result = decode_chunked(
        path,
        info,
        info.sectors,
        STAGE_SECTORS,
        &mut |ready, new_bytes| {
            let from = published;
            let upto = from + new_bytes.len();
            let mut filled = shared.filled.lock().unwrap_or_else(|e| e.into_inner());
            if filled.generation != my_gen {
                return;
            }
            // Only what is newly finished is copied, so filling a track costs
            // one pass over its bytes rather than one per report. A four-minute
            // track reports some 280 times against a 42 MB buffer; recopying
            // the prefix each time would be gigabytes of memcpy to move 42 MB
            // of audio, on a thread that is racing the play head.
            if let Some(dst) = filled.pcm.get_mut(from..upto) {
                dst.copy_from_slice(new_bytes);
                filled.ready = ready;
                published = upto;
            }
        },
        &mut || stopped(),
    );
    if let Err(err) = result {
        // A partial track plays what it has and falls silent for the rest. The
        // device path stays up either way, the same posture a folder mount
        // takes when a host file disappears under it.
        tracing::warn!("cd audio decode failed for {}: {err}", path.display());
    }
    let mut filled = shared.filled.lock().unwrap_or_else(|e| e.into_inner());
    if filled.generation == my_gen {
        filled.done = true;
    }
}

/// How many tracks may hold decoded PCM at once.
///
/// Two, because that is what boundary prefetch needs: the track being played
/// and the one being read ahead of it, not because two is a good cache size.
/// A full CD is roughly 750 MB decoded and a front-panel play walks the whole
/// disc, so something has to bound it, while a general LRU would be more
/// machinery than this access pattern asks for.
const MAX_RESIDENT: usize = 2;

/// How many decode threads one mount may have alive at once.
///
/// A worker told to stop can still sit in a slow read (a network share, a
/// sleeping disk) before it reaches its next stop check. Without a cap, a
/// guest that scans the disc over such a share could pile up threads faster
/// than they exit.
const MAX_LIVE_WORKERS: usize = 4;

/// The residency bookkeeping shared by one mount's tracks.
///
/// Recency is the order of `last_touch`, which only the emulation thread
/// writes, through `frame`. A caller that touches a track for another reason
/// moves that track in the order.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<Vec<Arc<Shared>>>>,
    /// Workers of this mount that are alive right now.
    live: Arc<AtomicUsize>,
    #[cfg(test)]
    gate: Option<Arc<Gate>>,
}

/// Holds test workers at their start so a test can keep several alive at once
/// with fixtures that would otherwise decode in microseconds.
#[cfg(test)]
#[derive(Debug)]
struct Gate {
    open: Mutex<bool>,
    changed: std::sync::Condvar,
    /// A stuck gate ignores the stop signal. It stands for a worker blocked in
    /// a slow read before its first cancel check.
    stuck: bool,
}

#[cfg(test)]
impl Gate {
    fn new(stuck: bool) -> Arc<Self> {
        Arc::new(Self {
            open: Mutex::new(false),
            changed: std::sync::Condvar::new(),
            stuck,
        })
    }

    fn wait(&self, stop: &dyn Fn() -> bool) {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        while !*open {
            if !self.stuck && stop() {
                return;
            }
            open = self
                .changed
                .wait_timeout(open, std::time::Duration::from_millis(2))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Workers of this mount that are alive right now. A worker told to stop
    /// still counts until it has actually exited.
    pub fn live_workers(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// A registry whose workers wait at their start until
    /// [`Registry::open_gate`], or until they are told to stop.
    #[cfg(test)]
    fn gated() -> Self {
        Self {
            gate: Some(Gate::new(false)),
            ..Self::default()
        }
    }

    /// As [`Registry::gated`], but the workers ignore the stop signal.
    #[cfg(test)]
    fn gated_stuck() -> Self {
        Self {
            gate: Some(Gate::new(true)),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn open_gate(&self) {
        if let Some(gate) = &self.gate {
            *gate.open.lock().unwrap_or_else(|e| e.into_inner()) = true;
            gate.changed.notify_all();
        }
    }

    /// Prepare a track that participates in this mount's residency.
    pub fn track(&self, path: PathBuf) -> Result<DecodedTrack, CdAudioError> {
        let mut track = DecodedTrack::new(path)?;
        track.registry = Some(self.clone());
        Ok(track)
    }

    /// As [`Registry::track`], from a measurement the mount already took.
    pub fn track_with_info(&self, path: PathBuf, info: TrackInfo) -> DecodedTrack {
        let mut track = DecodedTrack::with_info(path, info);
        track.registry = Some(self.clone());
        track
    }

    /// Make `started` the newest resident, and take the buffer back from the
    /// least recently touched tracks until no more than [`MAX_RESIDENT`] hold
    /// one. A track whose worker is still running loses its buffer too: the
    /// worker stops at its next check.
    fn admit(&self, started: &Arc<Shared>) {
        let mut resident = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        resident.retain(|s| !Arc::ptr_eq(s, started));
        resident.push(Arc::clone(started));
        while resident.len() > MAX_RESIDENT {
            // Never the track just admitted. Its `last_touch` is the newest
            // anyway; the filter keeps that from depending on the clock.
            let Some(index) = resident
                .iter()
                .enumerate()
                .filter(|(_, s)| !Arc::ptr_eq(s, started))
                .min_by_key(|(_, s)| s.last_touch.load(Ordering::Relaxed))
                .map(|(index, _)| index)
            else {
                break;
            };
            let evicted = resident.remove(index);
            release(&evicted);
        }
    }

    /// Drop one track from the residency list without touching its buffer.
    fn forget(&self, shared: &Arc<Shared>) {
        let mut resident = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        resident.retain(|s| !Arc::ptr_eq(s, shared));
    }
}

impl Drop for DecodedTrack {
    fn drop(&mut self) {
        // Unmounting or replacing a disc must not leave a thread decoding it or
        // a buffer behind. The buffer goes now. The worker sees the new
        // generation at its next packet and exits, without anyone waiting for
        // it: a join here would stall the emulation thread on eject.
        if let Some(registry) = &self.registry {
            registry.forget(&self.shared);
        }
        release(&self.shared);
    }
}

impl AudioTrackSource for DecodedTrack {
    fn sectors(&self) -> u32 {
        self.info.sectors
    }

    fn frame(&self, index: u32) -> Option<[u8; AUDIO_FRAME_BYTES]> {
        if index >= self.info.sectors {
            return None;
        }
        // Before the read, and on the hit path too: this is what keeps a track
        // the mixer is currently drawing from out of the eviction candidate
        // set. One relaxed increment, on a path that is about to take a mutex
        // anyway.
        self.shared.last_touch.store(
            TOUCH_CLOCK.fetch_add(1, Ordering::Relaxed),
            Ordering::Relaxed,
        );
        let frame = {
            let filled = self.shared.filled.lock().unwrap_or_else(|e| e.into_inner());
            if index < filled.ready {
                let off = index as usize * AUDIO_FRAME_BYTES;
                // `get` rather than indexing: if the buffer and the count ever
                // disagree this is silence, not a dead process. `frame()` runs
                // on the emulation thread and the build aborts on panic.
                filled.pcm.get(off..off + AUDIO_FRAME_BYTES).map(|slice| {
                    let mut out = [0u8; AUDIO_FRAME_BYTES];
                    out.copy_from_slice(slice);
                    out
                })
            } else {
                None
            }
        };
        // Started only after the read, and outside the lock. Reading first is
        // what makes the first touch of a track deterministically absent rather
        // than a race against however fast the worker gets going, and the
        // caller's contract is already that an absent frame is silence it
        // steps over.
        if frame.is_none() {
            self.ensure_started();
        }
        frame
    }
}

#[cfg(test)]
#[path = "track_test.rs"]
mod tests;
