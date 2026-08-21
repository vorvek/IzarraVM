// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use izarravm_input::{ControllerDevice, ControllerDeviceMatcher};

type HardwareId = (u16, u16);
const FAILED_RETRY: Duration = Duration::from_secs(1);

pub(super) trait ControllerNameProvider {
    fn bus_names(&mut self) -> Result<Vec<(String, String)>, ()>;
}

#[derive(Default)]
pub(super) struct PlatformControllerNameProvider;

#[cfg(not(windows))]
impl ControllerNameProvider for PlatformControllerNameProvider {
    fn bus_names(&mut self) -> Result<Vec<(String, String)>, ()> {
        Ok(Vec::new())
    }
}

#[cfg(windows)]
impl ControllerNameProvider for PlatformControllerNameProvider {
    fn bus_names(&mut self) -> Result<Vec<(String, String)>, ()> {
        platform_bus_names()
    }
}

pub(super) struct ControllerNameResolver<P = PlatformControllerNameProvider> {
    provider: P,
    generation: Option<u64>,
    failed_retry: Option<(u64, Instant)>,
    names: BTreeMap<HardwareId, String>,
}

impl Default for ControllerNameResolver {
    fn default() -> Self {
        Self {
            provider: PlatformControllerNameProvider,
            generation: None,
            failed_retry: None,
            names: BTreeMap::new(),
        }
    }
}

impl<P: ControllerNameProvider> ControllerNameResolver<P> {
    pub(super) fn refresh(&mut self, generation: u64) {
        self.refresh_at(generation, Instant::now());
    }

    fn refresh_at(&mut self, generation: u64, now: Instant) {
        if self.generation == Some(generation) {
            return;
        }
        if self
            .failed_retry
            .is_some_and(|(failed, retry)| failed == generation && now < retry)
        {
            return;
        }
        match self.provider.bus_names() {
            Ok(records) => {
                self.names = unique_bus_names(records);
                self.generation = Some(generation);
                self.failed_retry = None;
            }
            Err(()) => {
                self.names.clear();
                self.generation = None;
                self.failed_retry = Some((generation, now + FAILED_RETRY));
            }
        }
    }

    pub(super) fn display_devices(&self, devices: &[ControllerDevice]) -> Vec<ControllerDevice> {
        devices
            .iter()
            .cloned()
            .map(|mut device| {
                device.matcher = self.resolved_matcher(&device.matcher);
                device
            })
            .collect()
    }

    pub(super) fn resolved_matcher(
        &self,
        matcher: &ControllerDeviceMatcher,
    ) -> ControllerDeviceMatcher {
        let mut resolved = matcher.clone();
        if let Some(name) = self.hardware_name(matcher) {
            resolved.name = name.to_owned();
        }
        resolved
    }

    pub(super) fn hardware_name(&self, matcher: &ControllerDeviceMatcher) -> Option<&str> {
        if matcher.backend != "gilrs-wgi" {
            return None;
        }
        let hardware = (matcher.vendor_id?, matcher.product_id?);
        self.names.get(&hardware).map(String::as_str)
    }
}

fn unique_bus_names(records: Vec<(String, String)>) -> BTreeMap<HardwareId, String> {
    let mut candidates = BTreeMap::<HardwareId, BTreeSet<String>>::new();
    for (instance_id, name) in records {
        let Some(hardware) = parse_vid_pid(&instance_id) else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() {
            candidates
                .entry(hardware)
                .or_default()
                .insert(name.to_owned());
        }
    }
    candidates
        .into_iter()
        .filter_map(|(hardware, names)| {
            let names = collapse_controller_wrappers(names);
            (names.len() == 1).then(|| (hardware, names.into_iter().next().unwrap()))
        })
        .collect()
}

fn collapse_controller_wrappers(mut names: BTreeSet<String>) -> BTreeSet<String> {
    let duplicates = names
        .iter()
        .filter_map(|name| {
            let inner = name.strip_prefix("Controller (")?.strip_suffix(')')?;
            (inner.is_empty() || names.contains(inner)).then(|| name.clone())
        })
        .collect::<Vec<_>>();
    for duplicate in duplicates {
        names.remove(&duplicate);
    }
    names
}

fn parse_vid_pid(instance_id: &str) -> Option<HardwareId> {
    let upper = instance_id.to_ascii_uppercase();
    let parse = |marker: &str| {
        let start = upper.find(marker)? + marker.len();
        let value = upper.get(start..start + 4)?;
        value
            .chars()
            .all(|digit| digit.is_ascii_hexdigit())
            .then(|| u16::from_str_radix(value, 16).ok())?
    };
    Some((parse("VID_")?, parse("PID_")?))
}

#[cfg(windows)]
fn platform_bus_names() -> Result<Vec<(String, String)>, ()> {
    use std::{mem::size_of, ptr};
    use windows_sys::Win32::{
        Devices::{
            DeviceAndDriverInstallation::{
                DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA,
                SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
                SetupDiGetDeviceInstanceIdW, SetupDiGetDevicePropertyW,
            },
            Properties::{DEVPKEY_Device_BusReportedDeviceDesc, DEVPROP_TYPE_STRING, DEVPROPTYPE},
        },
        Foundation::{ERROR_NO_MORE_ITEMS, GetLastError},
    };
    use windows_sys::core::GUID;

    struct DeviceInfoSet(HDEVINFO);

    impl Drop for DeviceInfoSet {
        fn drop(&mut self) {
            unsafe {
                SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }

    unsafe fn instance_id(info: HDEVINFO, data: &SP_DEVINFO_DATA) -> Option<String> {
        let mut buffer = [0_u16; 512];
        let mut required = 0;
        if unsafe {
            SetupDiGetDeviceInstanceIdW(
                info,
                data,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut required,
            )
        } == 0
            || required == 0
            || required as usize > buffer.len()
        {
            return None;
        }
        let length = buffer
            .iter()
            .take(required as usize)
            .position(|value| *value == 0)
            .unwrap_or(required as usize);
        String::from_utf16(&buffer[..length]).ok()
    }

    unsafe fn bus_name(info: HDEVINFO, data: &SP_DEVINFO_DATA) -> Option<String> {
        let mut buffer = [0_u16; 512];
        let mut property_type: DEVPROPTYPE = 0;
        let mut required = 0;
        let byte_capacity = size_of::<u16>() * buffer.len();
        if unsafe {
            SetupDiGetDevicePropertyW(
                info,
                data,
                &DEVPKEY_Device_BusReportedDeviceDesc,
                &mut property_type,
                buffer.as_mut_ptr().cast(),
                byte_capacity as u32,
                &mut required,
                0,
            )
        } == 0
            || property_type != DEVPROP_TYPE_STRING
            || required < size_of::<u16>() as u32
            || required as usize > byte_capacity
            || !(required as usize).is_multiple_of(size_of::<u16>())
        {
            return None;
        }
        let units = required as usize / size_of::<u16>();
        let length = buffer[..units]
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(units);
        String::from_utf16(&buffer[..length]).ok()
    }

    let raw = unsafe {
        SetupDiGetClassDevsW(
            ptr::null(),
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_ALLCLASSES,
        )
    };
    if raw == -1_isize {
        return Err(());
    }
    let info = DeviceInfoSet(raw);
    let mut records = Vec::new();
    let mut index = 0;
    loop {
        let mut data = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ClassGuid: GUID::from_u128(0),
            DevInst: 0,
            Reserved: 0,
        };
        if unsafe { SetupDiEnumDeviceInfo(info.0, index, &mut data) } == 0 {
            if unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS {
                break;
            }
            return Err(());
        }
        index += 1;
        if let (Some(instance_id), Some(name)) =
            unsafe { (instance_id(info.0, &data), bus_name(info.0, &data)) }
        {
            records.push((instance_id, name));
        }
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
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
        assert!(
            collapse_controller_wrappers(BTreeSet::from(["Controller ()".to_owned()])).is_empty()
        );

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
}
