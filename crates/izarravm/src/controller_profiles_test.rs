// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_input::{ControllerConfig, ControllerDeviceMatcher};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct TestScratch(PathBuf);

impl TestScratch {
    fn new(label: &str) -> Self {
        let serial = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "izarravm-controller-profiles-{label}-{}-{serial}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn controller(device_name: &str) -> ControllerConfig {
    ControllerConfig::default_keyboard(ControllerDeviceMatcher {
        backend: "test".into(),
        platform: "test".into(),
        guid: format!("guid-{device_name}"),
        vendor_id: Some(0x1234),
        product_id: Some(0x5678),
        name: device_name.into(),
        occurrence: 0,
    })
}

#[test]
fn profiles_use_the_caller_state_directory_and_round_trip() {
    let scratch = TestScratch::new("round-trip");
    let state_dir = scratch.path().join("resolved-state");
    let store = ControllerProfileStore::new(&state_dir);
    let quake = controller("Quake controller");
    let doom = controller("Doom controller");

    store.save("Quake", &quake).unwrap();
    store.save("Doom", &doom).unwrap();

    assert_eq!(store.list().unwrap(), ["Doom", "Quake"]);
    assert_eq!(store.load("Quake").unwrap(), quake);
    assert_eq!(store.load("Doom").unwrap(), doom);
    assert!(state_dir.join(DIRECTORY_NAME).join("Quake.toml").is_file());
    assert!(!state_dir.join("izarravm.conf").exists());
}

#[test]
fn save_upserts_one_profile_without_leaving_a_temporary_file() {
    let scratch = TestScratch::new("upsert");
    let store = ControllerProfileStore::new(scratch.path());
    store.save("Quake", &controller("First")).unwrap();
    let replacement = controller("Second");

    store.save("Quake", &replacement).unwrap();

    assert_eq!(store.list().unwrap(), ["Quake"]);
    assert_eq!(store.load("Quake").unwrap(), replacement);
    let files = std::fs::read_dir(scratch.path().join(DIRECTORY_NAME))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(files, ["Quake.toml"]);
}

#[test]
#[cfg(unix)]
fn a_profile_with_an_uppercase_extension_uses_its_actual_path() {
    let scratch = TestScratch::new("uppercase-extension");
    let store = ControllerProfileStore::new(scratch.path());
    let original = controller("Original");
    store.save("Quake", &original).unwrap();
    let directory = scratch.path().join(DIRECTORY_NAME);
    let lowercase = directory.join("Quake.toml");
    let uppercase = directory.join("Quake.TOML");
    std::fs::rename(&lowercase, &uppercase).unwrap();

    assert_eq!(store.list().unwrap(), ["Quake"]);
    assert_eq!(store.load("quake").unwrap(), original);

    let replacement = controller("Replacement");
    store.save("QUAKE", &replacement).unwrap();
    assert_eq!(store.load("Quake").unwrap(), replacement);
    assert!(uppercase.is_file());
    assert!(!lowercase.exists());

    store.delete("quake").unwrap();
    assert!(store.list().unwrap().is_empty());
}

#[test]
fn an_interrupted_replacement_recovers_the_previous_profile() {
    let scratch = TestScratch::new("recover");
    let store = ControllerProfileStore::new(scratch.path());
    let previous = controller("Previous");
    store.save("Quake", &previous).unwrap();
    let path = scratch.path().join(DIRECTORY_NAME).join("Quake.toml");
    std::fs::rename(&path, backup_path(&path)).unwrap();

    assert_eq!(store.list().unwrap(), ["Quake"]);
    assert_eq!(store.load("Quake").unwrap(), previous);
    assert!(path.is_file());
    assert!(!backup_path(&path).exists());
}

#[test]
fn create_allocates_a_readable_unique_name() {
    let scratch = TestScratch::new("create");
    let store = ControllerProfileStore::new(scratch.path());
    let current = controller("Current");
    store.save("new profile", &current).unwrap();
    store.save("New Profile 2", &current).unwrap();

    assert_eq!(store.create(&current).unwrap(), "New Profile 3");
    assert_eq!(store.create(&current).unwrap(), "New Profile 4");
    assert_eq!(store.load("New Profile 4").unwrap(), current);
}

#[test]
fn create_named_uses_a_meaningful_name_without_overwriting_it() {
    let scratch = TestScratch::new("create-named");
    let store = ControllerProfileStore::new(scratch.path());
    let quake = controller("Quake");
    let doom = controller("Doom");

    store.create_named("Quake", &quake).unwrap();
    assert!(matches!(
        store.create_named("quake", &doom),
        Err(ControllerProfileError::AlreadyExists { .. })
    ));
    assert_eq!(store.load("Quake").unwrap(), quake);
}

#[test]
fn delete_removes_only_the_selected_profile() {
    let scratch = TestScratch::new("delete");
    let store = ControllerProfileStore::new(scratch.path());
    store.save("Doom", &controller("Doom")).unwrap();
    store.save("Quake", &controller("Quake")).unwrap();

    store.delete("Doom").unwrap();

    assert_eq!(store.list().unwrap(), ["Quake"]);
    assert!(matches!(
        store.load("Doom"),
        Err(ControllerProfileError::NotFound { .. })
    ));
    assert!(matches!(
        store.delete("Doom"),
        Err(ControllerProfileError::NotFound { .. })
    ));
}

#[test]
fn unsafe_and_reserved_names_never_reach_the_file_system() {
    let scratch = TestScratch::new("unsafe-names");
    let store = ControllerProfileStore::new(scratch.path());
    let current = controller("Current");
    let long_name = "x".repeat(MAX_PROFILE_NAME_CHARS + 1);
    let invalid = [
        "",
        " ",
        "../Doom",
        "..\\Doom",
        "Doom/II",
        "Doom\\II",
        "Doom: II",
        "Doom*",
        ".hidden",
        "bad.",
        " bad",
        "bad\nname",
        "CON",
        "con.txt",
        "LPT1",
        &long_name,
    ];

    for name in invalid {
        assert!(
            matches!(
                store.save(name, &current),
                Err(ControllerProfileError::InvalidName { .. })
            ),
            "accepted {name:?}"
        );
    }
    assert!(!scratch.path().join(DIRECTORY_NAME).exists());
}

#[test]
fn a_broken_or_future_profile_is_reported_without_replacing_the_current_mapping() {
    let scratch = TestScratch::new("broken");
    let store = ControllerProfileStore::new(scratch.path());
    std::fs::create_dir_all(scratch.path().join(DIRECTORY_NAME)).unwrap();
    std::fs::write(
        scratch.path().join(DIRECTORY_NAME).join("Broken.toml"),
        "this is not toml",
    )
    .unwrap();
    let current = controller("Still current");

    let loaded = store.load("Broken");

    assert!(matches!(loaded, Err(ControllerProfileError::Parse { .. })));
    assert_eq!(current.device.name, "Still current");

    let future = StoredControllerProfile {
        format_version: PROFILE_FORMAT_VERSION + 1,
        controller: controller("Future"),
    };
    std::fs::write(
        scratch.path().join(DIRECTORY_NAME).join("Future.toml"),
        toml::to_string_pretty(&future).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        store.load("Future"),
        Err(ControllerProfileError::UnsupportedVersion { .. })
    ));
}

#[test]
fn an_absent_profile_directory_lists_as_empty() {
    let scratch = TestScratch::new("absent");
    let store = ControllerProfileStore::new(scratch.path().join("missing"));
    assert!(store.list().unwrap().is_empty());
}
