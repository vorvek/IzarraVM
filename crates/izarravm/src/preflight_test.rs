// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

/// The rustflags in `.cargo/config.toml` apply to test builds too (they are
/// not release-only), so a test binary built on this branch already targets
/// `x86-64-v3`. If this ever fails, either the flag stopped applying (CI's
/// build fingerprint should have caught that -- see
/// `scripts/run-realtime-gate.ps1`) or someone narrowed the target back down,
/// and either way `require_avx2` no longer matches what the binary needs.
#[test]
#[allow(clippy::assertions_on_constants)]
fn release_build_targets_avx2() {
    assert!(
        cfg!(target_feature = "avx2"),
        "this build no longer targets x86-64-v3; the AVX2 hard requirement \
         and the preflight check it backs are now out of sync"
    );
}

/// Pins the owner-approved wording so a future edit has to go through
/// review, not slip in as a drive-by rename.
#[test]
fn avx2_required_message_is_exact() {
    assert_eq!(
        AVX2_REQUIRED_MESSAGE,
        "This computer's processor does not support AVX2. IzarraVM needs AVX2 to run. \
         The program will close."
    );
}

/// This box (like every CI runner) has AVX2 and the rest of `x86-64-v3`,
/// so the check must return normally rather than exiting the test
/// process. A weak smoke test on its own (see the synthetic-vector tests
/// below for the ones that actually catch a broken decision), but it does
/// still catch a `cpuid`/`xgetbv` call that panics or hangs.
#[test]
fn require_avx2_returns_normally_on_this_host() {
    require_avx2();
}

/// Synthetic vectors for [`avx2_from_cpuid`], the pure AVX2 decision. Each
/// one flips exactly one gate closed and checks the whole thing reads
/// `false`; the last opens every gate and checks `true`. This is the test
/// that a mutant (`avx2_from_cpuid` returning `true` unconditionally, or
/// dropping the `xgetbv`/`XCR0` check) cannot survive -- unlike comparing
/// against `std::is_x86_feature_detected!`, which folds to the same
/// answer as the code under test whenever this crate is itself compiled
/// with `-C target-cpu=x86-64-v3`, so a test built on it can never fail.
mod avx2_from_cpuid_vectors {
    use super::*;

    const OSXSAVE: u32 = 1 << 27;
    const AVX: u32 = 1 << 28;
    const LEAF1_ALL_SET: u32 = OSXSAVE | AVX;
    const XCR0_SSE_AND_AVX: u64 = 0b110;
    const LEAF7_AVX2: u32 = 1 << 5;

    #[test]
    fn osxsave_clear_fails() {
        assert!(!avx2_from_cpuid(AVX, XCR0_SSE_AND_AVX, LEAF7_AVX2));
    }

    #[test]
    fn avx_clear_fails() {
        assert!(!avx2_from_cpuid(OSXSAVE, XCR0_SSE_AND_AVX, LEAF7_AVX2));
    }

    #[test]
    fn xcr0_sse_only_fails() {
        // OS saves SSE state but not AVX state: using a YMM register still
        // raises #UD even though CPUID reports AVX2 support.
        assert!(!avx2_from_cpuid(LEAF1_ALL_SET, 0b010, LEAF7_AVX2));
    }

    #[test]
    fn xcr0_avx_only_fails() {
        // The reverse: AVX state saved but not SSE state. XCR0 must carry
        // both bits together for AVX2 to be usable.
        assert!(!avx2_from_cpuid(LEAF1_ALL_SET, 0b100, LEAF7_AVX2));
    }

    #[test]
    fn leaf7_avx2_bit_clear_fails() {
        // OSXSAVE, AVX and the XCR0 state are all fine, but the CPU
        // silicon itself does not report AVX2 (bit 5) in leaf 7.
        assert!(!avx2_from_cpuid(LEAF1_ALL_SET, XCR0_SSE_AND_AVX, 0));
    }

    #[test]
    fn every_gate_open_passes() {
        assert!(avx2_from_cpuid(LEAF1_ALL_SET, XCR0_SSE_AND_AVX, LEAF7_AVX2));
    }
}

/// Synthetic vectors for [`x86_64_v3_from_cpuid`], the pure decision for
/// the full `x86-64-v3` feature set. Confirms the AVX2 baseline still
/// gates it, then flips each additional v3-only bit closed in turn.
mod x86_64_v3_from_cpuid_vectors {
    use super::*;

    const LEAF1_AVX2_BASELINE: u32 = (1 << 27) | (1 << 28); // OSXSAVE | AVX
    const FMA: u32 = 1 << 12;
    const MOVBE: u32 = 1 << 22;
    const F16C: u32 = 1 << 29;
    const XCR0_SSE_AND_AVX: u64 = 0b110;
    const LEAF7_AVX2: u32 = 1 << 5;
    const BMI1: u32 = 1 << 3;
    const BMI2: u32 = 1 << 8;
    const LEAF7_ALL_SET: u32 = LEAF7_AVX2 | BMI1 | BMI2;
    const LEAF1_ALL_SET: u32 = LEAF1_AVX2_BASELINE | FMA | MOVBE | F16C;
    const LZCNT: u32 = 1 << 5;

    #[test]
    fn avx2_baseline_failure_still_fails_v3() {
        assert!(!x86_64_v3_from_cpuid(0, 0, 0, LZCNT));
    }

    #[test]
    fn fma_clear_fails() {
        let leaf1 = LEAF1_AVX2_BASELINE | MOVBE | F16C;
        assert!(!x86_64_v3_from_cpuid(
            leaf1,
            XCR0_SSE_AND_AVX,
            LEAF7_ALL_SET,
            LZCNT
        ));
    }

    #[test]
    fn movbe_clear_fails() {
        let leaf1 = LEAF1_AVX2_BASELINE | FMA | F16C;
        assert!(!x86_64_v3_from_cpuid(
            leaf1,
            XCR0_SSE_AND_AVX,
            LEAF7_ALL_SET,
            LZCNT
        ));
    }

    #[test]
    fn f16c_clear_fails() {
        let leaf1 = LEAF1_AVX2_BASELINE | FMA | MOVBE;
        assert!(!x86_64_v3_from_cpuid(
            leaf1,
            XCR0_SSE_AND_AVX,
            LEAF7_ALL_SET,
            LZCNT
        ));
    }

    #[test]
    fn bmi1_clear_fails() {
        let leaf7 = LEAF7_AVX2 | BMI2;
        assert!(!x86_64_v3_from_cpuid(
            LEAF1_ALL_SET,
            XCR0_SSE_AND_AVX,
            leaf7,
            LZCNT
        ));
    }

    #[test]
    fn bmi2_clear_fails() {
        let leaf7 = LEAF7_AVX2 | BMI1;
        assert!(!x86_64_v3_from_cpuid(
            LEAF1_ALL_SET,
            XCR0_SSE_AND_AVX,
            leaf7,
            LZCNT
        ));
    }

    #[test]
    fn lzcnt_clear_fails() {
        assert!(!x86_64_v3_from_cpuid(
            LEAF1_ALL_SET,
            XCR0_SSE_AND_AVX,
            LEAF7_ALL_SET,
            0
        ));
    }

    #[test]
    fn every_v3_bit_set_passes() {
        assert!(x86_64_v3_from_cpuid(
            LEAF1_ALL_SET,
            XCR0_SSE_AND_AVX,
            LEAF7_ALL_SET,
            LZCNT
        ));
    }
}

#[cfg(windows)]
#[test]
fn wide_strings_round_trip_the_ascii_source() {
    let message: String = char::decode_utf16(
        AVX2_REQUIRED_MESSAGE_W[..AVX2_REQUIRED_MESSAGE_W.len() - 1]
            .iter()
            .copied(),
    )
    .collect::<Result<_, _>>()
    .expect("valid UTF-16");
    assert_eq!(message, AVX2_REQUIRED_MESSAGE);
    assert_eq!(
        *AVX2_REQUIRED_MESSAGE_W.last().unwrap(),
        0,
        "must be NUL-terminated"
    );

    let title: String = char::decode_utf16(
        AVX2_REQUIRED_TITLE_W[..AVX2_REQUIRED_TITLE_W.len() - 1]
            .iter()
            .copied(),
    )
    .collect::<Result<_, _>>()
    .expect("valid UTF-16");
    assert_eq!(title, AVX2_REQUIRED_TITLE);
    assert_eq!(
        *AVX2_REQUIRED_TITLE_W.last().unwrap(),
        0,
        "must be NUL-terminated"
    );
}

/// `IZARRAVM_NO_DIALOG` must suppress the dialog path regardless of
/// console state -- this is the only piece of `dialog_is_reachable_and_wanted`
/// that is safely testable without an attached/detached console fixture.
#[cfg(windows)]
#[test]
fn no_dialog_env_var_suppresses_the_dialog() {
    // SAFETY (test-only): no other test in this binary reads or writes
    // IZARRAVM_NO_DIALOG, and `cargo test` for this crate runs each test
    // in its own thread but shares the process environment; restoring the
    // prior value below keeps this test from leaking state into others
    // that happen to run around it.
    let previous = std::env::var_os("IZARRAVM_NO_DIALOG");
    unsafe {
        std::env::set_var("IZARRAVM_NO_DIALOG", "1");
    }
    let suppressed = !dialog_is_reachable_and_wanted();
    match previous {
        Some(value) => unsafe { std::env::set_var("IZARRAVM_NO_DIALOG", value) },
        None => unsafe { std::env::remove_var("IZARRAVM_NO_DIALOG") },
    }
    assert!(suppressed, "IZARRAVM_NO_DIALOG=1 must suppress the dialog");
}
