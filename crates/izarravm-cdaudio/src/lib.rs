// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

#![forbid(unsafe_code)]

//! Decoding the OGG, MP3, WAV, and FLAC files a CUE sheet may name for its
//! AUDIO tracks into the raw Red Book frames the disc model serves.
//!
//! See `dev_docs/2026-08-14-cd-audio-decoding-design.md`.

mod sniff;

pub use sniff::{Container, SNIFF_BYTES, sniff};
