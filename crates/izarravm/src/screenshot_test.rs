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
    )
}

#[test]
fn screenshots_directory_is_inside_the_state_directory() {
    let state = Path::new("/home/user/.izarravm");
    assert_eq!(
        screenshots_dir(state),
        PathBuf::from("/home/user/.izarravm/screenshots")
    );
}

#[test]
fn png_is_created_with_the_guest_pixels() {
    let directory = test_dir("pixels").join("screenshots");
    let path = save_png_with_stem(&sample_frame(), &directory, "IzarraVM_test", None).unwrap();
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
    let frame = ScreenshotFrame::new(Arc::new(vec![0x0020_2020]), 1, 1);
    let path = save_png_with_stem(&frame, &directory, "IzarraVM_gamma", Some(2.4)).unwrap();

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
    let first = save_png_with_stem(&sample_frame(), &directory, "IzarraVM_same", None).unwrap();
    let second = save_png_with_stem(&sample_frame(), &directory, "IzarraVM_same", None).unwrap();
    assert_eq!(first.file_name().unwrap(), "IzarraVM_same.png");
    assert_eq!(second.file_name().unwrap(), "IzarraVM_same_001.png");
    assert!(first.is_file());
    assert!(second.is_file());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn an_invalid_frame_does_not_create_the_directory() {
    let directory = test_dir("invalid");
    let frame = ScreenshotFrame::new(Arc::new(vec![0; 3]), 2, 2);
    assert!(matches!(
        save_png_with_stem(&frame, &directory, "IzarraVM_bad", None),
        Err(ScreenshotError::InvalidFrame { .. })
    ));
    assert!(!directory.exists());
}
