// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use std::{cell::Cell, collections::VecDeque, rc::Rc};

struct TestProvider {
    results: VecDeque<Result<Vec<(String, String)>, ()>>,
    calls: Rc<Cell<usize>>,
}

impl ControllerNameProvider for TestProvider {
    fn bus_names(&mut self) -> Result<Vec<(String, String)>, ()> {
        self.calls.set(self.calls.get() + 1);
        self.results.pop_front().unwrap_or_else(|| Ok(Vec::new()))
    }
}

fn test_resolver(
    results: impl IntoIterator<Item = Result<Vec<(String, String)>, ()>>,
) -> (ControllerNameResolver<TestProvider>, Rc<Cell<usize>>) {
    let calls = Rc::new(Cell::new(0));
    (
        ControllerNameResolver {
            provider: TestProvider {
                results: results.into_iter().collect(),
                calls: calls.clone(),
            },
            generation: None,
            failed_retry: None,
            names: BTreeMap::new(),
        },
        calls,
    )
}

fn keychron_records() -> Vec<(String, String)> {
    vec![(
        "USB\\VID_3434&PID_1061\\A".into(),
        "Keychron Q6 HE 8K".into(),
    )]
}

#[test]
fn vid_pid_parser_is_case_insensitive_and_strict() {
    assert_eq!(
        parse_vid_pid(r"usb\vid_3434&pid_1061&mi_02\device"),
        Some((0x3434, 0x1061))
    );
    assert_eq!(parse_vid_pid(r"USB\VID_343&PID_1061\device"), None);
    assert_eq!(parse_vid_pid("unrelated"), None);
}

#[test]
fn bus_names_require_one_unambiguous_trimmed_value() {
    let names = unique_bus_names(vec![
        (
            "USB\\VID_3434&PID_1061\\A".into(),
            " Keychron Q6 HE 8K ".into(),
        ),
        (
            "HID\\VID_3434&PID_1061\\B".into(),
            "Controller (Keychron Q6 HE 8K)".into(),
        ),
        ("USB\\VID_045E&PID_028E\\A".into(), "8BitDo Pro 2".into()),
        ("HID\\VID_045E&PID_028E\\B".into(), "Other pad".into()),
        ("USB\\VID_1111&PID_2222\\A".into(), " ".into()),
    ]);
    assert_eq!(
        names.get(&(0x3434, 0x1061)).map(String::as_str),
        Some("Keychron Q6 HE 8K")
    );
    assert!(!names.contains_key(&(0x045e, 0x028e)));
    assert!(!names.contains_key(&(0x1111, 0x2222)));
}

#[test]
fn controller_wrappers_only_collapse_against_the_same_plain_name() {
    let names = BTreeSet::from(["Controller (Foo)".to_owned(), "Foo".to_owned()]);
    assert_eq!(
        collapse_controller_wrappers(names),
        BTreeSet::from(["Foo".to_owned()])
    );

    let ambiguous = BTreeSet::from([
        "Bar".to_owned(),
        "Controller (Foo)".to_owned(),
        "Foo".to_owned(),
    ]);
    assert_eq!(
        collapse_controller_wrappers(ambiguous),
        BTreeSet::from(["Bar".to_owned(), "Foo".to_owned()])
    );
}

#[test]
fn controller_wrapper_edge_cases_are_not_guessed() {
    for name in ["Controller (Foo)", "Controller Foo)", "Controller (Foo"] {
        let names = BTreeSet::from([name.to_owned()]);
        assert_eq!(collapse_controller_wrappers(names.clone()), names);
    }
    assert!(collapse_controller_wrappers(BTreeSet::from(["Controller ()".to_owned()])).is_empty());

    let nested = BTreeSet::from([
        "Controller (Controller (Foo))".to_owned(),
        "Controller (Foo)".to_owned(),
    ]);
    assert_eq!(
        collapse_controller_wrappers(nested),
        BTreeSet::from(["Controller (Foo)".to_owned()])
    );
}

#[test]
fn topology_generation_controls_name_cache_invalidation() {
    let (mut resolver, calls) = test_resolver([
        Ok(keychron_records()),
        Ok(vec![(
            "USB\\VID_3434&PID_1061\\B".into(),
            "Keychron replacement".into(),
        )]),
    ]);
    let now = Instant::now();
    resolver.refresh_at(7, now);
    resolver.refresh_at(7, now + Duration::from_secs(30));
    assert_eq!(calls.get(), 1);
    assert_eq!(
        resolver.names.get(&(0x3434, 0x1061)).map(String::as_str),
        Some("Keychron Q6 HE 8K")
    );

    resolver.refresh_at(8, now + Duration::from_millis(1));
    assert_eq!(calls.get(), 2);
    assert_eq!(
        resolver.names.get(&(0x3434, 0x1061)).map(String::as_str),
        Some("Keychron replacement")
    );
}

#[test]
fn failed_enumeration_is_bounded_but_new_generations_retry_immediately() {
    let (mut resolver, calls) = test_resolver([Err(()), Err(()), Ok(keychron_records())]);
    let now = Instant::now();
    resolver.refresh_at(7, now);
    assert_eq!(calls.get(), 1);
    assert_eq!(resolver.generation, None);
    resolver.refresh_at(7, now + Duration::from_millis(999));
    assert_eq!(calls.get(), 1);

    resolver.refresh_at(8, now + Duration::from_millis(100));
    assert_eq!(calls.get(), 2);
    assert_eq!(resolver.generation, None);
    resolver.refresh_at(8, now + Duration::from_millis(1_099));
    assert_eq!(calls.get(), 2);
    resolver.refresh_at(8, now + Duration::from_millis(1_100));
    assert_eq!(calls.get(), 3);
    assert_eq!(resolver.generation, Some(8));
    assert_eq!(
        resolver.names.get(&(0x3434, 0x1061)).map(String::as_str),
        Some("Keychron Q6 HE 8K")
    );
}

#[test]
fn hardware_names_require_wgi_and_a_resolved_vid_pid() {
    let (mut resolver, _) = test_resolver([Ok(keychron_records())]);
    resolver.refresh_at(1, Instant::now());
    let wgi = ControllerDeviceMatcher {
        backend: "gilrs-wgi".into(),
        platform: "windows".into(),
        guid: String::new(),
        vendor_id: Some(0x3434),
        product_id: Some(0x1061),
        name: "Generic".into(),
        occurrence: 0,
    };
    assert_eq!(resolver.hardware_name(&wgi), Some("Keychron Q6 HE 8K"));
    let mut xinput = wgi.clone();
    xinput.backend = "xinput".into();
    assert_eq!(resolver.hardware_name(&xinput), None);
    let mut unknown = wgi;
    unknown.product_id = Some(0xffff);
    assert_eq!(resolver.hardware_name(&unknown), None);
}
