// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only
//
// DIAGNOSTIC ONLY. Not for merge. Gated on IZARRAVM_DISTIRA_LODDIAG=<path>;
// when the variable is unset every entry point is one relaxed load of a
// OnceLock<bool> and returns.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static ENABLED: OnceLock<bool> = OnceLock::new();

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("IZARRAVM_DISTIRA_LODDIAG").is_some())
}

/// Physical mip level chosen per SAMPLED PIXEL, per TMU.
static SAMPLE_LOD: [[AtomicU64; 16]; 2] =
    [[const { AtomicU64::new(0) }; 16], [const { AtomicU64::new(0) }; 16]];
/// Mip level named by a texture APERTURE WRITE, per TMU.
static UPLOAD_LOD: [[AtomicU64; 16]; 2] =
    [[const { AtomicU64::new(0) }; 16], [const { AtomicU64::new(0) }; 16]];

/// Aperture-write coverage of TMU memory, one bit per byte, per TMU.
static WRITTEN: OnceLock<[Box<[AtomicU64]>; 2]> = OnceLock::new();
const TEX_BYTES: usize = 2 * 1024 * 1024;

fn written() -> &'static [Box<[AtomicU64]>; 2] {
    WRITTEN.get_or_init(|| {
        std::array::from_fn(|_| {
            (0..TEX_BYTES / 64)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice()
        })
    })
}

/// Texel fetches from a byte no aperture write ever covered, per TMU.
static UNWRITTEN_READS: [AtomicU64; 2] = [const { AtomicU64::new(0) }, const { AtomicU64::new(0) }];
static READS: [AtomicU64; 2] = [const { AtomicU64::new(0) }, const { AtomicU64::new(0) }];
/// Aperture writes whose computed address ran past the 2 MB TMU and wrapped.
static UPLOAD_WRAPS: [AtomicU64; 2] = [const { AtomicU64::new(0) }, const { AtomicU64::new(0) }];
static UPLOAD_MAX: [AtomicU64; 2] = [const { AtomicU64::new(0) }, const { AtomicU64::new(0) }];

pub(super) fn note_upload_bytes(tmu: usize, offset: usize, len: usize, unmasked: usize) {
    if !enabled() {
        return;
    }
    let tmu = tmu.min(1);
    if unmasked >= TEX_BYTES {
        UPLOAD_WRAPS[tmu].fetch_add(1, Ordering::Relaxed);
    }
    UPLOAD_MAX[tmu].fetch_max(unmasked as u64, Ordering::Relaxed);
    let map = &written()[tmu];
    for byte in offset..offset + len {
        let byte = byte & (TEX_BYTES - 1);
        map[byte / 64].fetch_or(1 << (byte % 64), Ordering::Relaxed);
    }
}

pub(super) fn note_read(tmu: usize, offset: usize) {
    if !enabled() {
        return;
    }
    let tmu = tmu.min(1);
    READS[tmu].fetch_add(1, Ordering::Relaxed);
    let byte = offset & (TEX_BYTES - 1);
    if written()[tmu][byte / 64].load(Ordering::Relaxed) & (1 << (byte % 64)) == 0 {
        UNWRITTEN_READS[tmu].fetch_add(1, Ordering::Relaxed);
    }
}

type Census = Mutex<BTreeMap<(usize, u32, u32), u64>>;
static TRIANGLE_MODES: OnceLock<Census> = OnceLock::new();

fn triangle_modes() -> &'static Census {
    TRIANGLE_MODES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn note_sample_lod(tmu: usize, lod: u32) {
    if !enabled() {
        return;
    }
    SAMPLE_LOD[tmu.min(1)][(lod as usize).min(15)].fetch_add(1, Ordering::Relaxed);
}

pub(super) fn note_upload_lod(tmu: usize, lod: u32) {
    if !enabled() {
        return;
    }
    UPLOAD_LOD[tmu.min(1)][(lod as usize).min(15)].fetch_add(1, Ordering::Relaxed);
}

/// One entry per TRIANGLE per TMU: the `textureMode` and `tLOD` in force.
pub(super) fn note_triangle(tmu: usize, texture_mode: u32, texture_lod: u32) {
    if !enabled() {
        return;
    }
    let mut census = triangle_modes().lock().expect("lod diag census");
    *census
        .entry((tmu.min(1), texture_mode, texture_lod))
        .or_insert(0) += 1;
}

fn hist(rows: &[[AtomicU64; 16]; 2], tmu: usize) -> String {
    (0..16)
        .map(|lod| rows[tmu][lod].load(Ordering::Relaxed))
        .enumerate()
        .filter(|&(_, count)| count != 0)
        .map(|(lod, count)| format!("{lod}={count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Writes the census to the path in `IZARRAVM_DISTIRA_LODDIAG`. Called once,
/// at end of run.
pub fn dump() {
    let Some(path) = std::env::var_os("IZARRAVM_DISTIRA_LODDIAG") else {
        return;
    };
    let mut out = String::new();
    for tmu in 0..2 {
        out.push_str(&format!("upload_lod tmu{tmu}: {}\n", hist(&UPLOAD_LOD, tmu)));
    }
    for tmu in 0..2 {
        out.push_str(&format!("sample_lod tmu{tmu}: {}\n", hist(&SAMPLE_LOD, tmu)));
    }
    for tmu in 0..2 {
        out.push_str(&format!(
            "tmu{tmu}: reads={} unwritten_reads={} upload_wraps={} upload_max={:#x}
",
            READS[tmu].load(Ordering::Relaxed),
            UNWRITTEN_READS[tmu].load(Ordering::Relaxed),
            UPLOAD_WRAPS[tmu].load(Ordering::Relaxed),
            UPLOAD_MAX[tmu].load(Ordering::Relaxed),
        ));
    }
    let census = triangle_modes().lock().expect("lod diag census");
    let mut rows: Vec<_> = census.iter().collect();
    rows.sort_by_key(|&(_, &count)| std::cmp::Reverse(count));
    out.push_str("tmu textureMode tLOD lodmin lodmax lodbias split odd aspect triangles\n");
    for (&(tmu, mode, lod), &count) in rows {
        let lodmin = (lod & 0x3f) as f64 / 4.0;
        let lodmax = ((lod >> 6) & 0x3f) as f64 / 4.0;
        let bias_raw = ((lod >> 12) & 0x3f) as i32;
        let bias = f64::from(if bias_raw & 0x20 != 0 {
            bias_raw | !0x3f
        } else {
            bias_raw
        }) / 4.0;
        let split = (lod >> 19) & 1;
        let odd = (lod >> 18) & 1;
        let aspect = (lod >> 21) & 3;
        out.push_str(&format!(
            "{tmu} {mode:#010x} {lod:#010x} {lodmin} {lodmax} {bias} {split} {odd} {aspect} {count}\n"
        ));
    }
    let _ = std::fs::write(path, out);
}
