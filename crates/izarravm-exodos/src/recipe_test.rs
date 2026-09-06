// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[test]
fn the_generic_schedule_stays_clear_of_the_steady_window() {
    // The classification window is the last 60 of 120 guest seconds, and an
    // injection schedule puts a knee where it ends.
    let recipe = Recipe::generic();
    assert!(recipe.keys.iter().all(|step| step.guest_ms < 55_000));
}

#[test]
fn renders_guest_milliseconds_as_strictly_increasing_cycle_offsets() {
    let recipe = Recipe::generic();
    let spec = recipe
        .to_inject_keys_within(166_000_000, u64::MAX)
        .expect("a schedule");
    let mut previous = 0u64;
    for step in spec.split(';') {
        let (cycles, payload) = step.split_once(':').expect("cycles:payload");
        let cycles: u64 = cycles.parse().expect("a number");
        assert!(cycles > previous, "{spec}");
        previous = cycles;
        assert!(!payload.is_empty());
    }
    // 6,000 guest ms at 166 MHz.
    assert!(spec.starts_with("996000000:1;"));
}

#[test]
fn the_same_recipe_lands_at_the_same_guest_time_on_both_personas() {
    let recipe = Recipe::generic();
    let at_586 = recipe
        .to_inject_keys_within(166_000_000, u64::MAX)
        .expect("a schedule");
    let at_486 = recipe
        .to_inject_keys_within(66_000_000, u64::MAX)
        .expect("a schedule");
    let first = |spec: &str, hz: u64| {
        let cycles: u64 = spec
            .split(':')
            .next()
            .and_then(|c| c.parse().ok())
            .expect("a number");
        cycles * 1000 / hz
    };
    assert_eq!(first(&at_586, 166_000_000), first(&at_486, 66_000_000));
}

#[test]
fn drops_steps_the_budget_would_never_reach() {
    // 20 guest seconds at 586.
    let spec = Recipe::generic()
        .to_inject_keys_within(166_000_000, 3_320_000_000)
        .expect("a schedule");
    for step in spec.split(';') {
        let cycles: u64 = step.split(':').next().unwrap().parse().unwrap();
        assert!(cycles < 3_320_000_000, "{spec}");
    }
    assert!(spec.split(';').count() < Recipe::generic().keys.len());
}

#[test]
fn round_trips_through_the_recipe_file_format() {
    let recipe = Recipe {
        notes: "menu and mouse setup".to_string(),
        keys: vec![KeyStep {
            guest_ms: 1234,
            text: "{esc}\\r".to_string(),
        }],
        mouse: vec![MouseStep {
            guest_ms: 5678,
            action: "move:-5,12".to_string(),
        }],
    };
    let encoded = serde_json::to_value(&recipe).expect("serialize");
    assert_eq!(
        encoded,
        serde_json::json!({
            "notes": "menu and mouse setup",
            "keys": [{"guest_ms": 1234, "text": "{esc}\\r"}],
            "mouse": [{"guest_ms": 5678, "action": "move:-5,12"}]
        })
    );
    let back: Recipe = serde_json::from_value(encoded).expect("deserialize");
    assert_eq!(back.notes, "menu and mouse setup");
    assert_eq!(back.keys.len(), 1);
    assert_eq!(
        (back.keys[0].guest_ms, back.keys[0].text.as_str()),
        (1234, "{esc}\\r")
    );
    assert_eq!(back.mouse.len(), 1);
    assert_eq!(
        (back.mouse[0].guest_ms, back.mouse[0].action.as_str()),
        (5678, "move:-5,12")
    );
}

#[test]
fn a_recipe_written_before_mouse_steps_existed_still_reads() {
    let back: Recipe =
        serde_json::from_str(r#"{"keys":[{"guest_ms":1000,"text":"\\r"}]}"#).expect("deserialize");
    assert!(back.mouse.is_empty());
    assert!(back.to_inject_mouse_within(166_000_000, u64::MAX).is_none());
}

#[test]
fn only_the_actions_the_emulator_parses_are_accepted() {
    for good in ["home", "click", "down", "up", "move:320,544", "move:-5, 12"] {
        assert!(is_valid_mouse_action(good), "{good}");
    }
    for bad in ["move 320 544", "move:320", "move:a,b", "wiggle", ""] {
        assert!(!is_valid_mouse_action(bad), "{bad}");
    }
}

#[test]
fn mouse_steps_render_as_their_own_offset_ordered_schedule() {
    let recipe = Recipe {
        notes: String::new(),
        keys: Vec::new(),
        mouse: vec![
            MouseStep {
                guest_ms: 21_000,
                action: "click".to_string(),
            },
            MouseStep {
                guest_ms: 20_000,
                action: "home".to_string(),
            },
        ],
    };
    let spec = recipe
        .to_inject_mouse_within(166_000_000, u64::MAX)
        .expect("a schedule");
    assert_eq!(spec, "3320000000:home;3486000000:click");
    // The budget bound applies to mouse steps too.
    assert!(recipe.to_inject_mouse_within(166_000_000, 1000).is_none());
}
