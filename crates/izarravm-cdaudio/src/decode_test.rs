// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Decode a whole fixture into a buffer the probe sized, returning the buffer
/// and the number of output sample frames the decoder produced.
fn decode_all(name: &str) -> (TrackInfo, Vec<u8>, u64) {
    let info = crate::probe_info(&fixture(name)).unwrap().unwrap();
    let mut pcm = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];
    let produced = decode_into(&fixture(name), info, &mut pcm, &mut |_ready, _new| {}).unwrap();
    (info, pcm, produced)
}

/// Peak absolute sample over the first `frames` output frames.
fn peak(pcm: &[u8], frames: usize) -> u16 {
    pcm.as_chunks::<4>()
        .0
        .iter()
        .take(frames)
        .map(|s| i16::from_le_bytes([s[0], s[1]]).unsigned_abs())
        .max()
        .unwrap_or(0)
}

#[test]
fn decoded_length_agrees_with_the_probe() {
    // The invariant the TOC rests on. Asserting that the finished buffer holds
    // `sectors` sectors would be a tautology -- it is allocated at that size --
    // so this compares what the decoder actually produced, before padding or
    // truncation, against the sector count the mount already published.
    for name in ["tone.wav", "tone.ogg", "tone.flac", "tone-nolength.flac"] {
        let (info, _pcm, produced) = decode_all(name);
        assert_eq!(
            produced.div_ceil(SAMPLES_PER_FRAME),
            u64::from(info.sectors),
            "{name} produced {produced} output frames against {} sectors",
            info.sectors
        );
    }
}

#[test]
fn an_mp3s_length_agrees_too() {
    // MPEG is where the two sides could most easily disagree: the stream
    // carries the encoder's delay ahead of the audio and padding after it, and
    // the probe subtracts both from a packet walk while the reader trims them
    // off the decoded buffers. Two routes to one number, and this is the one
    // fixture where that number is not simply the container's own.
    let (info, pcm, produced) = decode_all("tone.mp3");
    assert_eq!(
        produced.div_ceil(SAMPLES_PER_FRAME),
        u64::from(info.sectors)
    );
    // And the audio starts at the top of the track rather than 1105 samples of
    // encoder priming in.
    assert!(peak(&pcm, 512) > 1000, "track opens with the encoder delay");
}

#[test]
fn the_probes_subtraction_and_the_readers_trim_are_not_applied_twice() {
    // symphonia's MPEG reader applies the delay and padding itself before a
    // decoded buffer is handed over -- the first packet of the LAME fixture
    // arrives stamped ts = -1105 and yields 47 of its 1152 frames. The probe
    // arrives at the same 8820 by walking coded frames and subtracting the
    // declared delay and padding.
    //
    // Both are right and doing either twice is wrong, which is not obvious
    // from reading either side: an earlier version of this decoder skipped
    // `delay` samples of its own on top of the reader's trim and lost 1105
    // samples, a whole sector and change, from the front of every LAME track.
    // Exact equality is what says it is applied once.
    let info = crate::probe_info(&fixture("tone.mp3")).unwrap().unwrap();
    let (_info, _pcm, produced) = decode_all("tone.mp3");
    assert_eq!(
        produced, info.frames,
        "the decoder produced {produced} frames against the probe's {}",
        info.frames
    );
}

#[test]
fn a_decode_stops_at_the_length_the_mount_published() {
    // Every fixture here happens to run out exactly when the probe said it
    // would, so nothing above can tell whether the decode is bounded by the
    // promise or merely by the end of the file. That distinction matters when
    // the file has grown since the mount measured it -- a rip being rewritten
    // under a running emulator -- because the TOC is already published and the
    // sectors past it belong to the next track.
    //
    // A frame count shorter than the file forces the two apart.
    let mut info = crate::probe_info(&fixture("tone.wav")).unwrap().unwrap();
    let full = info.frames;
    info.frames = full / 3;
    info.sectors = crate::sectors_for(info.frames, info.sample_rate);
    let mut pcm = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];

    let produced = decode_into(&fixture("tone.wav"), info, &mut pcm, &mut |_, _| {}).unwrap();

    assert_eq!(
        produced, info.frames,
        "decoded {produced} of a {full}-frame file that was promised as {}",
        info.frames
    );
}

#[test]
fn a_mono_22k_source_is_resampled_and_duplicated() {
    let (_info, pcm, _produced) = decode_all("tone-22k-mono.wav");
    // Both channels carry the same signal.
    let mut compared = 0;
    for sample in pcm.as_chunks::<4>().0.iter().take(2000).skip(200) {
        let l = i16::from_le_bytes([sample[0], sample[1]]);
        let r = i16::from_le_bytes([sample[2], sample[3]]);
        assert_eq!(l, r);
        compared += 1;
    }
    assert!(compared > 0, "no samples were compared");
    // And it is not silence -- a resampler bug returning zeros would satisfy
    // the equality above without decoding anything.
    assert!(peak(&pcm, 2000) > 1000, "decoded tone is silent");
}

#[test]
fn a_decode_never_writes_past_the_buffer_the_mount_sized() {
    // The TOC is published before any of this runs, so a decode that produces
    // more than it promised must lose the surplus rather than take space that
    // belongs to the next track. Half a buffer is handed in to force it.
    let info = crate::probe_info(&fixture("tone.wav")).unwrap().unwrap();
    let half = (info.sectors as usize / 2) * AUDIO_FRAME_BYTES;
    let mut pcm = vec![0u8; half];
    let produced = decode_into(&fixture("tone.wav"), info, &mut pcm, &mut |_, _| {}).unwrap();
    assert_eq!(pcm.len(), half);
    // It still reports everything it decoded, which is what the length
    // agreement is measured against; only the writing is bounded.
    assert!(produced > (half / 4) as u64);
}

#[test]
fn progress_is_reported_as_the_buffer_fills() {
    let info = crate::probe_info(&fixture("tone.ogg")).unwrap().unwrap();
    let mut pcm = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];
    let mut reports = Vec::new();
    decode_into(&fixture("tone.ogg"), info, &mut pcm, &mut |ready, _new| {
        reports.push(ready)
    })
    .unwrap();
    assert!(!reports.is_empty(), "the decoder never reported progress");
    // Monotonic, and the last report covers the whole track.
    assert!(reports.windows(2).all(|w| w[0] <= w[1]));
    assert_eq!(*reports.last().unwrap(), info.sectors);
}

#[test]
fn a_cancelled_decode_stops_where_it_stands() {
    let info = crate::probe_info(&fixture("tone.ogg")).unwrap().unwrap();
    let mut pcm = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];
    let mut reports = Vec::new();
    decode_into_cancellable(
        &fixture("tone.ogg"),
        info,
        &mut pcm,
        &mut |ready, _new| reports.push(ready),
        // Cancel at the first opportunity.
        &mut || true,
    )
    .unwrap();
    assert_eq!(
        reports.len(),
        1,
        "cancellation was not honored at the first publish: {reports:?}"
    );
    // The tail it never reached is untouched, which is silence.
    let tail = &pcm[pcm.len() - AUDIO_FRAME_BYTES..];
    assert!(tail.iter().all(|b| *b == 0));
}

#[test]
fn a_file_that_is_no_longer_a_container_fails_rather_than_decoding_noise() {
    // The mount measured the file; the decode reopens it later, and between
    // the two the user may have replaced it. Refuse rather than treat whatever
    // is there now as audio.
    let swapped = std::env::temp_dir().join(format!(
        "izarravm-cdaudio-swapped-{}.ogg",
        std::process::id()
    ));
    std::fs::write(&swapped, vec![0u8; 4096]).unwrap();
    let info = TrackInfo {
        sample_rate: 44100,
        channels: 2,
        frames: 8820,
        sectors: 15,
    };
    let mut pcm = vec![0u8; 15 * AUDIO_FRAME_BYTES];
    let err = decode_into(&swapped, info, &mut pcm, &mut |_, _| {}).unwrap_err();
    assert!(err.to_string().contains("container"), "message was: {err}");
    std::fs::remove_file(&swapped).ok();
}

#[test]
fn a_decode_that_ends_early_does_not_republish_its_silent_tail() {
    // A file replaced or truncated between mount and play stops far short of
    // the length the TOC promised. The bytes past that point were never written
    // and are already zero in both buffers, so handing them to the callback
    // copies zeros onto zeros -- and `DecodedTrack` does that copy while
    // holding the lock the mixer pull takes, which for a long track is hundreds
    // of megabytes of memcpy stalling the emulation thread.
    let mut info = crate::probe_info(&fixture("tone.wav")).unwrap().unwrap();
    let real_bytes = info.sectors as usize * AUDIO_FRAME_BYTES;
    // Claim ten times the length; the file ends long before it.
    info.frames *= 10;
    info.sectors = crate::sectors_for(info.frames, info.sample_rate);
    let mut pcm = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];

    let mut handed_over = 0usize;
    let mut last_ready = 0u32;
    decode_into(&fixture("tone.wav"), info, &mut pcm, &mut |ready, new| {
        handed_over += new.len();
        last_ready = ready;
    })
    .unwrap();

    // The whole track is still declared ready -- the tail is silence, not
    // pending -- but only the sectors carrying audio were passed along.
    assert_eq!(last_ready, info.sectors);
    assert!(
        handed_over <= real_bytes + AUDIO_FRAME_BYTES,
        "handed over {handed_over} bytes for {real_bytes} bytes of audio in a \
         {} byte buffer",
        pcm.len()
    );
}

/// A 44.1 kHz, 16-bit, 6-channel WAVE_FORMAT_EXTENSIBLE file. Channel 0 holds
/// 1000, channel 1 holds -2000 and the rest hold 30000.
fn surround_wav(frames: usize) -> Vec<u8> {
    let channels = 6u16;
    let block = channels * 2;
    let data_len = frames as u32 * u32::from(block);
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(4 + 8 + 40 + 8 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&40u32.to_le_bytes());
    wav.extend_from_slice(&0xFFFEu16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&44_100u32.to_le_bytes());
    wav.extend_from_slice(&(44_100 * u32::from(block)).to_le_bytes());
    wav.extend_from_slice(&block.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(&22u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(&0x3Fu32.to_le_bytes());
    // KSDATAFORMAT_SUBTYPE_PCM
    wav.extend_from_slice(&[
        0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B,
        0x71,
    ]);
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for _ in 0..frames {
        for value in [1000i16, -2000, 30000, 30000, 30000, 30000] {
            wav.extend_from_slice(&value.to_le_bytes());
        }
    }
    wav
}

#[test]
fn a_surround_source_plays_its_front_left_and_front_right() {
    let path = std::env::temp_dir().join(format!(
        "izarravm-cdaudio-surround-{}.wav",
        std::process::id()
    ));
    std::fs::write(&path, surround_wav(4410)).unwrap();
    let info = crate::probe_info(&path).unwrap().unwrap();
    assert_eq!(info.channels, 6);
    let mut pcm = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];
    decode_into(&path, info, &mut pcm, &mut |_ready, _new| {}).unwrap();
    let first = &pcm[..4];
    assert_eq!(i16::from_le_bytes([first[0], first[1]]), 1000);
    assert_eq!(i16::from_le_bytes([first[2], first[3]]), -2000);
    std::fs::remove_file(&path).ok();
}

/// Decode through `decode_chunked` and return every chunk it published, with
/// the sector count that came with it.
fn chunks_of(path: &Path, info: TrackInfo, stage_sectors: u32) -> Vec<(u32, Vec<u8>)> {
    let mut chunks = Vec::new();
    decode_chunked(
        path,
        info,
        info.sectors,
        stage_sectors,
        &mut |ready, chunk| chunks.push((ready, chunk.to_vec())),
        &mut || false,
    )
    .unwrap();
    chunks
}

/// Lay the chunks end to end in a track-sized buffer, as `DecodedTrack` does.
fn assemble(info: TrackInfo, chunks: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut pcm = vec![0u8; info.sectors as usize * AUDIO_FRAME_BYTES];
    let mut at = 0;
    for (_, chunk) in chunks {
        pcm[at..at + chunk.len()].copy_from_slice(chunk);
        at += chunk.len();
    }
    pcm
}

#[test]
fn chunked_decode_matches_the_whole_buffer_decode_for_every_container() {
    let odd_length = std::env::temp_dir().join(format!(
        "izarravm-cdaudio-odd-length-{}.wav",
        std::process::id()
    ));
    // 4410 frames are seven and a half sectors, so the last sector is partial.
    std::fs::write(&odd_length, surround_wav(4410)).unwrap();
    let mut paths: Vec<PathBuf> = [
        "tone.wav",
        "tone.flac",
        "tone.ogg",
        "tone.mp3",
        // Needs resampling, and its length is not a whole number of sectors.
        "tone-22k-mono.wav",
    ]
    .iter()
    .map(|name| fixture(name))
    .collect();
    paths.push(odd_length.clone());

    for path in &paths {
        let info = crate::probe_info(path).unwrap().unwrap();
        // One stage as large as the track never flushes early, so it is the
        // reference for the small stages.
        let whole = assemble(info, &chunks_of(path, info, info.sectors));
        assert!(whole.iter().any(|b| *b != 0), "{path:?} decoded to silence");
        for stage in [1, 2, 5, STAGE_SECTORS] {
            let chunked = assemble(info, &chunks_of(path, info, stage));
            assert!(chunked == whole, "{path:?} differs at {stage} sectors");
        }
    }
    std::fs::remove_file(&odd_length).ok();
}

#[test]
fn the_staging_size_bounds_every_published_chunk() {
    let info = crate::probe_info(&fixture("tone.ogg")).unwrap().unwrap();
    for stage in [1u32, 3, 8] {
        let chunks = chunks_of(&fixture("tone.ogg"), info, stage);
        let bound = stage as usize * AUDIO_FRAME_BYTES;
        assert!(chunks.len() > 1, "stage {stage} published in one piece");
        let (last, body) = chunks.split_last().unwrap();
        for (_, chunk) in body {
            assert!(
                chunk.len() <= bound,
                "chunk of {} over {bound}",
                chunk.len()
            );
            // Mid-track chunks are whole sectors.
            assert_eq!(chunk.len() % AUDIO_FRAME_BYTES, 0);
        }
        assert!(last.1.len() <= bound);
        assert_eq!(
            last.1.len() % AUDIO_FRAME_BYTES,
            0,
            "the padded tail is whole"
        );
        assert_eq!(last.0, info.sectors);
    }
}

#[test]
fn a_partial_last_sector_is_padded_with_silence_in_a_reused_stage() {
    // 4410 frames: the last sector holds 294 frames. A one-sector stage has
    // held nonzero audio before, so a missing pad would leave it in the tail.
    let path = std::env::temp_dir().join(format!(
        "izarravm-cdaudio-partial-tail-{}.wav",
        std::process::id()
    ));
    std::fs::write(&path, surround_wav(4410)).unwrap();
    let info = crate::probe_info(&path).unwrap().unwrap();
    let chunks = chunks_of(&path, info, 1);
    let tail = &chunks.last().unwrap().1;
    assert_eq!(tail.len(), AUDIO_FRAME_BYTES);
    let audio = 294 * 4;
    assert!(tail[..audio].iter().any(|b| *b != 0));
    assert!(tail[audio..].iter().all(|b| *b == 0));
    std::fs::remove_file(&path).ok();
}
