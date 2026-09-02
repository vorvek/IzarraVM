// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn test_dir(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "izarravm-screenshot-{label}-{}-{nonce}",
        std::process::id()
    ))
}

fn sample_frame() -> ScreenshotFrame {
    ScreenshotFrame::new(
        Arc::new(vec![0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0x00ff_ffff]),
        2,
        2,
        false,
    )
}

#[test]
fn png_is_created_with_the_guest_pixels() {
    let directory = test_dir("pixels").join("screenshots");
    let path =
        save_png_with_stem(&sample_frame(), &directory, "IzarraVM_test", None, None).unwrap();
    assert_eq!(path, directory.join("IzarraVM_test.png"));

    let decoder = png::Decoder::new(File::open(&path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!((info.width, info.height), (2, 2));
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(
        &pixels[..info.buffer_size()],
        &[
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ]
    );

    std::fs::remove_dir_all(directory.parent().unwrap()).unwrap();
}

/// A screenshot must show the same picture the window did: applying
/// `monitor_gamma` before PNG encode, per channel, alpha untouched.
#[test]
fn a_screenshot_applies_the_monitor_gamma_pref() {
    let directory = test_dir("gamma").join("screenshots");
    let frame = ScreenshotFrame::new(Arc::new(vec![0x0020_2020]), 1, 1, false);
    let path = save_png_with_stem(&frame, &directory, "IzarraVM_gamma", Some(2.4), None).unwrap();

    let decoder = png::Decoder::new(File::open(&path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    // code 32 at gamma 2.4 -> 20, per the golden table in display_transform_test.rs.
    assert_eq!(&pixels[..info.buffer_size()], &[20, 20, 20, 255]);

    std::fs::remove_dir_all(directory.parent().unwrap()).unwrap();
}

#[test]
fn a_same_timestamp_uses_a_collision_suffix() {
    let directory = test_dir("collision");
    let first =
        save_png_with_stem(&sample_frame(), &directory, "IzarraVM_same", None, None).unwrap();
    let second =
        save_png_with_stem(&sample_frame(), &directory, "IzarraVM_same", None, None).unwrap();
    assert_eq!(first.file_name().unwrap(), "IzarraVM_same.png");
    assert_eq!(second.file_name().unwrap(), "IzarraVM_same_001.png");
    assert!(first.is_file());
    assert!(second.is_file());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn an_invalid_frame_does_not_create_the_directory() {
    let directory = test_dir("invalid");
    let frame = ScreenshotFrame::new(Arc::new(vec![0; 3]), 2, 2, false);
    assert!(matches!(
        save_png_with_stem(&frame, &directory, "IzarraVM_bad", None, None),
        Err(ScreenshotError::InvalidFrame { .. })
    ));
    assert!(!directory.exists());
}

/// A Distira screenshot must show the same picture the window did, which means
/// both transforms in the shader's order: the Glide compensation is a
/// signal-domain edit and runs first, then the display model.
#[test]
fn a_distira_screenshot_applies_the_glide_compensation_before_the_monitor_gamma() {
    let directory = test_dir("glide").join("screenshots");
    let frame = ScreenshotFrame::new(Arc::new(vec![0x0080_8080]), 1, 1, true);
    let path = save_png_with_stem(
        &frame,
        &directory,
        "IzarraVM_glide",
        Some(2.4),
        crate::prefs::GlideGamma::Compatible.exponent(),
    )
    .unwrap();

    let decoder = png::Decoder::new(File::open(&path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    // Code 128 compensates to 91 (the glide golden ramp), and 91 at gamma 2.4
    // is 82. Composing the other way round gives 128 -> 121 -> 83, so this
    // pins the ORDER, not merely that both transforms ran.
    assert_eq!(
        crate::display_transform::display_transform(91, Some(2.4)),
        82
    );
    assert_eq!(
        crate::display_transform::glide_compensate(
            crate::display_transform::display_transform(128, Some(2.4)),
            crate::prefs::GlideGamma::Compatible.exponent(),
        ),
        83,
        "the reversed order must differ, or this test would not pin the order"
    );
    assert_eq!(&pixels[..info.buffer_size()], &[82, 82, 82, 255]);

    std::fs::remove_dir_all(directory.parent().unwrap()).unwrap();
}

/// The toggle shapes Distira's output only: a VGA or Margo screenshot saves
/// the same bytes whatever the Glide gamma setting says. `gui.rs` gates the
/// argument on the frame's own owner; this pins the other half, that a `None`
/// exponent really is inert here.
#[test]
fn a_non_distira_screenshot_is_untouched_by_the_glide_setting() {
    let directory = test_dir("glide_vga").join("screenshots");
    let frame = ScreenshotFrame::new(Arc::new(vec![0x0080_8080]), 1, 1, false);
    let path = save_png_with_stem(&frame, &directory, "IzarraVM_vga", None, None).unwrap();

    let decoder = png::Decoder::new(File::open(&path).unwrap());
    let mut reader = decoder.read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!(&pixels[..info.buffer_size()], &[128, 128, 128, 255]);

    std::fs::remove_dir_all(directory.parent().unwrap()).unwrap();
}
