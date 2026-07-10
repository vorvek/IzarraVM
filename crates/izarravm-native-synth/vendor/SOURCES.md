<!-- This file is part of IzarraVM and is licensed under GNU GPL version 3 only. -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Native synthesizer sources

The archives in this directory are unmodified upstream source snapshots. They
were retrieved on 2026-07-10. Cargo extracts and builds them locally. The build
does not use vcpkg, installed copies of these libraries, or network downloads.

| Archive | Upstream revision | SHA-256 | License |
| --- | --- | --- | --- |
| `fluidsynth-2.5.6.tar.gz` | [FluidSynth v2.5.6](https://github.com/FluidSynth/fluidsynth/tree/e5b11058a619246cea42976dacb76ca54be3d45d) | `0825f024c9cf7a18073739b83612d46542ecbfb349ae9147a1e9f08e2d524407` | LGPL-2.1-or-later |
| `gcem-012ae73c6d0a2cb09ffe86475f5c6fba3926e200.tar.gz` | [GCEM commit 012ae73c](https://github.com/kthohr/gcem/tree/012ae73c6d0a2cb09ffe86475f5c6fba3926e200) | `34ab0ee87a9eb26d3087fa9b49c2572ea8ee03db0c9705b83648301a3a3fc172` | Apache-2.0 |
| `libsndfile-1.2.2.tar.gz` | [libsndfile 1.2.2](https://github.com/libsndfile/libsndfile/tree/2bb834e1920c55325309d374e70bf1583d8f8321) | `ffe12ef8add3eaca876f04087734e6e8e029350082f3251f565fa9da55b52121` | LGPL-2.1-or-later; bundled ALAC is Apache-2.0 |
| `libogg-1.3.6.tar.gz` | [libogg 1.3.6](https://github.com/xiph/ogg/tree/db03f952b25717fd5c4938817c9290837a4ae1b2) | `95b643da661155d79db9de2fca55daed3a8d491039829def246aacb3d9201c81` | BSD-3-Clause |
| `libvorbis-1.3.7.tar.gz` | [libvorbis 1.3.7](https://github.com/xiph/vorbis/tree/0c55fa38933fd4bdb7db7c298b27e7bf2f2c5e98) | `270c76933d0934e42c5ee0a54a36280e2d87af1de3cc3e584806357e237afd13` | BSD-3-Clause |
| `flac-1.4.3.tar.gz` | [FLAC 1.4.3](https://github.com/xiph/flac/tree/28e4f0528c76b296c561e922ba67d43751990599) | `0a4bb82a30609b606650d538a804a7b40205366ce8fc98871b0ecf3fbb0611ee` | Mixed archive; linked libFLAC is BSD-3-Clause |
| `opus-1.5.2.tar.gz` | [Opus 1.5.2](https://github.com/xiph/opus/tree/5ec2f3c915d0529b94a3a302969c673531654824) | `9480e329e989f70d69886ded470c7f8cfe6c0667cc4196d4837ac9e668fb7404` | BSD-3-Clause |
| `munt-2.8.2.tar.gz` | [Munt 2.8.2](https://github.com/munt/munt/tree/cc1a638f5e8df31df7b0a990e4a3ba92c64e97fd) | `ca0a2b881207a88d10c511c441bcf4f7a9fd591bea630032662f02bcb51b1b5d` | Mixed archive; linked libmt32emu is LGPL-2.1-or-later |

FluidSynth's GitHub tag archive leaves its GCEM submodule empty. `build.rs`
fills that directory from the exact commit recorded by the FluidSynth tag.

libsndfile 1.2.2 uses one `HAVE_EXTERNAL_XIPH_LIBS` switch for its Ogg/Vorbis,
FLAC, and Opus readers. Its CMake build requires all four libraries when that
switch is enabled. FluidSynth needs it enabled to read SF3 SoundFonts, so these
sources are built even though IzarraVM uses only the Ogg/Vorbis path directly.

`build.rs` is the relinking script. Run `cargo build -p izarravm-native-synth`
from the repository root to rebuild the static libraries and Rust wrapper from
source. IzarraVM ships the application source needed to modify and relink this
LGPL code. Native build output stays in Cargo's `target` directory and is not
committed.

The `licenses` directory contains every top-level upstream license and notice,
plus libsndfile's bundled ALAC license. The source archives retain copyright
notices that apply to individual files. No Roland ROM image is included. Munt
reads ROM paths supplied by the user.
