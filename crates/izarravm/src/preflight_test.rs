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

/// This box (like every CI runner) has AVX2, so the check must return
/// normally rather than exiting the test process.
#[test]
fn require_avx2_returns_normally_on_this_host() {
    require_avx2();
}

/// Cross-check the hand-rolled `cpuid`/`xgetbv` detector against the
/// standard library's own answer. This is legitimately allowed to use
/// `std::is_x86_feature_detected!` -- the module comment explains why
/// `avx2_present_at_runtime` cannot -- because a mismatch here would mean
/// the two disagree about this host, which is worth knowing regardless of
/// which one folded to a constant.
#[test]
fn avx2_present_at_runtime_matches_the_standard_library() {
    assert_eq!(
        avx2_present_at_runtime(),
        std::is_x86_feature_detected!("avx2")
    );
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
