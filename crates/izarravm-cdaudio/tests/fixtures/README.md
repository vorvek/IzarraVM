<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# CD audio test fixtures

A 0.2 s 440 Hz sine, encoded into every container the CD-ROM emulation decodes.
Generated with ffmpeg 8.0; the commands are below so they can be regenerated
exactly. All content is synthesized by the commands themselves, so these carry
the project's own license.

| File | What it exercises |
| --- | --- |
| `tone.wav` | 44.1 kHz stereo baseline; the reference every other fixture's decode is compared against. |
| `tone.ogg` | Ogg Vorbis. |
| `tone.flac` | FLAC with a populated `total_samples` (8820, the exact sample count). |
| `tone.mp3` | LAME MP3 with a Xing header, so encoder delay and padding are declared. |
| `tone-noxing.mp3` | VBR MP3 with no Xing header: the container's own frame count is an extrapolation and must not be trusted. |
| `tone-nolength.flac` | FLAC with `total_samples = 0`, which is legal and must not fail the mount. Byte-identical to `tone.flac` past the STREAMINFO block. |
| `tone-22k-mono.wav` | 22.05 kHz mono: exercises resampling and channel duplication. |
| `tone-opus.ogg` | Opus inside Ogg: sniffs as `OggS` and must be rejected by name. |

## Regenerating

From the repository root:

```bash
mkdir -p crates/izarravm-cdaudio/tests/fixtures && cd crates/izarravm-cdaudio/tests/fixtures && ffmpeg -y -f lavfi -i "sine=frequency=440:duration=0.2:sample_rate=44100" -ac 2 -c:a pcm_s16le tone.wav && ffmpeg -y -i tone.wav -c:a libvorbis -q:a 4 tone.ogg && ffmpeg -y -i tone.wav -c:a flac tone.flac && ffmpeg -y -i tone.wav -c:a libmp3lame -b:a 128k tone.mp3 && ffmpeg -y -i tone.wav -c:a libmp3lame -q:a 5 -write_xing 0 tone-noxing.mp3 && ffmpeg -y -f lavfi -i "sine=frequency=440:duration=0.2:sample_rate=22050" -ac 1 -c:a pcm_s16le tone-22k-mono.wav && ffmpeg -y -i tone.wav -c:a libopus -f ogg tone-opus.ogg && cd -
```

`tone-nolength.flac` is produced separately, through a pipe, so that ffmpeg
cannot seek back and patch STREAMINFO's `total_samples` after the encode:

```bash
ffmpeg -y -i crates/izarravm-cdaudio/tests/fixtures/tone.wav -c:a flac -f flac - > crates/izarravm-cdaudio/tests/fixtures/tone-nolength.flac
```

Check that it really has no declared length before trusting it, because an
ffmpeg that did manage to seek produces a file which looks right and silently
stops exercising the indeterminate-length path:

```bash
python -c "d=open('crates/izarravm-cdaudio/tests/fixtures/tone-nolength.flac','rb').read(); print('total_samples =', int.from_bytes(d[21:26],'big')&0xFFFFFFFFF)"
```

Expected: `total_samples = 0`.

Regenerating any fixture changes its sha256, so `LICENSE_MANIFEST.tsv` has to
be re-pinned in the same commit or `scripts/check_file_policy.py` fails.
