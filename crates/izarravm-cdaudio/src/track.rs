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

use crate::decode::decode_into_cancellable;
use crate::probe::{CdAudioError, TrackInfo, probe_info};
use izarravm_core::{AUDIO_FRAME_BYTES, AudioTrackSource};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// The buffer and how much of it is real, together under one lock.
#[derive(Debug, Default)]
struct Filled {
    pcm: Vec<u8>,
    /// Whole sectors at the front of `pcm` that hold decoded audio. Everything
    /// from here on is zero and must not be served: silence at the head of a
    /// track that is about to arrive would be indistinguishable from silence
    /// the composer wrote.
    ready: u32,
}

#[derive(Debug)]
struct Shared {
    filled: Mutex<Filled>,
    /// Set when a worker is running or has finished, so the decode is started
    /// exactly once.
    started: AtomicBool,
    /// Set by `Drop` and checked by the worker at each publish point.
    cancel: AtomicBool,
    /// Set by the worker on its way out, whether it succeeded or failed. Only a
    /// track no worker is still writing into can have its buffer taken away,
    /// and a failed one has to be evictable too or a scratched rip would pin
    /// its buffer for the life of the mount.
    finished: AtomicBool,
    /// Workers actually spawned for this track. `started` is meant to hold this
    /// at one for the track's whole life, and nothing about the audio would
    /// look wrong if it did not -- every worker decodes the same file to the
    /// same bytes. What would go wrong is the machine: `frame` starts a worker
    /// on each miss, and a four-minute track is eighteen thousand misses before
    /// the first sector lands, so the failure is thousands of threads each
    /// decoding the same file, not a sound anyone can hear. Counted so a test
    /// can see it.
    workers: AtomicU32,
}

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
                cancel: AtomicBool::new(false),
                finished: AtomicBool::new(false),
                workers: AtomicU32::new(0),
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
        let bytes = self.info.sectors as usize * AUDIO_FRAME_BYTES;
        {
            let mut filled = self.shared.filled.lock().unwrap_or_else(|e| e.into_inner());
            filled.pcm = vec![0u8; bytes];
            filled.ready = 0;
        }
        // Admitted after the buffer exists and before the worker starts, so
        // this track is the newest resident by the time anything is written
        // into it and cannot evict itself.
        if let Some(registry) = &self.registry {
            registry.admit(&self.shared);
        }
        let shared = Arc::clone(&self.shared);
        let path = self.path.clone();
        let info = self.info;
        let registry = self.registry.clone();
        self.shared.workers.fetch_add(1, Ordering::Relaxed);
        let spawned = std::thread::Builder::new()
            .name("cd-audio-decode".to_string())
            .spawn(move || decode_worker(&path, info, &shared, registry.as_ref()));
        if spawned.is_err() {
            self.shared.workers.fetch_sub(1, Ordering::Relaxed);
            // The thread could not be created, so nothing will ever fill this
            // track. Clearing the flag lets the next `frame()` try again rather
            // than leaving the track permanently silent for a condition that is
            // usually momentary.
            self.shared.started.store(false, Ordering::SeqCst);
        }
    }
}

/// Decode the whole file, publishing each newly finished run of sectors.
fn decode_worker(path: &Path, info: TrackInfo, shared: &Arc<Shared>, registry: Option<&Registry>) {
    // Decode into a scratch buffer and copy under the lock. Holding the mutex
    // across a decode call would block the mixer pull for as long as a packet
    // takes to decode.
    let mut scratch = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];
    let mut published = 0usize;
    let result = decode_into_cancellable(
        path,
        info,
        &mut scratch,
        &mut |ready, new_bytes| {
            let from = published;
            let upto = from + new_bytes.len();
            let mut filled = shared.filled.lock().unwrap_or_else(|e| e.into_inner());
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
        &mut || shared.cancel.load(Ordering::Relaxed),
    );
    if let Err(err) = result {
        // A partial track plays what it has and falls silent for the rest. The
        // device path stays up either way -- the same posture a folder mount
        // takes when a host file disappears under it.
        tracing::warn!("cd audio decode failed for {}: {err}", path.display());
    }
    // Last, and on both paths: this is what makes the buffer evictable, and a
    // track whose decode failed has to become evictable too.
    shared.finished.store(true, Ordering::SeqCst);
    // Then ask again whether anything can now be given up. Eviction at
    // admission alone is not enough: when the third track of a disc starts
    // while the first two are still decoding, nothing is evictable at that
    // moment, and without this the disc simply stays over its bound for the
    // rest of the mount. Playback outruns decoding often enough that this is
    // the ordinary path, not the unlucky one.
    if let Some(registry) = registry {
        registry.evict_excess();
    }
}

/// How many tracks may hold decoded PCM at once.
///
/// Two, because that is what boundary prefetch needs -- the track being played
/// and the one being read ahead of it -- not because two is a good cache size.
/// A full CD is roughly 750 MB decoded and a front-panel play walks the whole
/// disc, so something has to bound it, while a general LRU would be more
/// machinery than this access pattern asks for.
const MAX_RESIDENT: usize = 2;

/// The residency bookkeeping shared by one mount's tracks.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<Vec<Arc<Shared>>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
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

    /// Note that `started` has just begun decoding, and take the buffer back
    /// from the oldest track that is no longer among the most recent few.
    fn admit(&self, started: &Arc<Shared>) {
        {
            let mut resident = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            resident.retain(|s| !Arc::ptr_eq(s, started));
            resident.push(Arc::clone(started));
        }
        self.evict_excess();
    }

    /// Take the buffer back from the oldest tracks until no more than
    /// [`MAX_RESIDENT`] hold one, skipping any a worker is still writing into.
    fn evict_excess(&self) {
        let mut resident = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        while resident.len() > MAX_RESIDENT {
            // Only a track that has stopped decoding can be evicted. Taking the
            // buffer from a live worker is safe in itself -- it copies under the
            // same lock and checks the length -- but clearing `started`
            // underneath one would let a second worker begin on the same track.
            let Some(index) = resident
                .iter()
                .position(|s| s.finished.load(Ordering::SeqCst))
            else {
                // Nothing evictable. Decoding outruns playback by so wide a
                // margin that two live workers do not arise in practice, and if
                // they ever do, a third buffer is a better answer than a stall.
                break;
            };
            let evicted = resident.remove(index);
            let mut filled = evicted.filled.lock().unwrap_or_else(|e| e.into_inner());
            filled.pcm = Vec::new();
            filled.ready = 0;
            // The track is now indistinguishable from one never touched, so the
            // next `frame()` decodes it again rather than serving silence for
            // the rest of the mount.
            evicted.finished.store(false, Ordering::SeqCst);
            evicted.started.store(false, Ordering::SeqCst);
        }
    }
}

impl Drop for DecodedTrack {
    fn drop(&mut self) {
        // Unmounting or replacing a disc must not leave a thread decoding it.
        // The worker notices at its next publish point, so it does at most one
        // more chunk of work before it stops.
        self.shared.cancel.store(true, Ordering::Relaxed);
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
