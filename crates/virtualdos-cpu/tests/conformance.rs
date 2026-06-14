use serde::Deserialize;
use virtualdos_bus::{BusAccessKind, BusError, BusWidth, CpuBus};

const MEM_SIZE: usize = 0x0100_0000; // 16 MiB covers the 24-bit address bus used by the suite

struct FlatBus {
    memory: Vec<u8>,
}

impl FlatBus {
    fn new() -> Self {
        Self {
            memory: vec![0; MEM_SIZE],
        }
    }

    fn read_u8(&self, address: u32) -> u8 {
        self.memory[(address as usize) & (MEM_SIZE - 1)]
    }

    fn write_u8(&mut self, address: u32, value: u8) {
        self.memory[(address as usize) & (MEM_SIZE - 1)] = value;
    }
}

impl CpuBus for FlatBus {
    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        _kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        let mut value = 0u32;
        for offset in 0..width.bytes() {
            value |= u32::from(self.read_u8(address.wrapping_add(offset))) << (offset * 8);
        }
        Ok(value)
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        _kind: BusAccessKind,
    ) -> Result<(), BusError> {
        for offset in 0..width.bytes() {
            self.write_u8(address.wrapping_add(offset), (value >> (offset * 8)) as u8);
        }
        Ok(())
    }

    fn read_io(&mut self, _port: u16, _width: BusWidth) -> Result<u32, BusError> {
        Ok(0)
    }

    fn write_io(&mut self, _port: u16, _width: BusWidth, _value: u32) -> Result<(), BusError> {
        Ok(())
    }

    fn interrupt_acknowledge(&mut self, _vector: u8, _ax: u16) -> Result<(), BusError> {
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct CpuTest {
    name: String,
    #[serde(default)]
    bytes: Vec<u8>,
    initial: TestState,
    #[serde(rename = "final")]
    final_state: TestState,
    #[serde(default)]
    exception: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct TestState {
    eax: Option<u32>,
    ebx: Option<u32>,
    ecx: Option<u32>,
    edx: Option<u32>,
    esi: Option<u32>,
    edi: Option<u32>,
    ebp: Option<u32>,
    esp: Option<u32>,
    eip: Option<u32>,
    eflags: Option<u32>,
    cr0: Option<u32>,
    cr3: Option<u32>,
    cs: Option<Seg>,
    ds: Option<Seg>,
    es: Option<Seg>,
    ss: Option<Seg>,
    fs: Option<Seg>,
    gs: Option<Seg>,
    #[serde(default)]
    ram: Vec<[u32; 2]>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Seg {
    Selector(u32),
    Descriptor {
        sel: Option<u32>,
        base: Option<u32>,
        limit: Option<u32>,
    },
}

impl Seg {
    fn selector(&self) -> u16 {
        match self {
            Seg::Selector(value) => *value as u16,
            Seg::Descriptor { sel, .. } => sel.unwrap_or(0) as u16,
        }
    }
}

fn load_tests(text: &str) -> Vec<CpuTest> {
    serde_json::from_str(text).expect("test vectors should deserialize")
}

#[test]
fn parses_synthetic_fixture() {
    let text = include_str!("fixtures/conformance_sample.json");
    let tests = load_tests(text);

    assert_eq!(tests.len(), 2);
    assert_eq!(tests[0].name, "nop");
    assert_eq!(tests[0].bytes, vec![144]);
    assert_eq!(tests[0].initial.cs.as_ref().unwrap().selector(), 0xf000);
    assert_eq!(tests[0].final_state.eip, Some(257));

    let clc = &tests[1];
    assert_eq!(clc.name, "clc");
    match clc.initial.cs.as_ref().unwrap() {
        Seg::Descriptor { base, .. } => assert_eq!(*base, Some(0x000f_0000)),
        Seg::Selector(_) => panic!("clc fixture should use the descriptor form"),
    }
}

#[test]
fn flat_bus_round_trips_widths() {
    let mut bus = FlatBus::new();

    bus.write_memory(0x10, BusWidth::Dword, 0x1234_5678, BusAccessKind::DataWrite)
        .unwrap();

    assert_eq!(bus.read_u8(0x10), 0x78);
    assert_eq!(bus.read_u8(0x13), 0x12);
    assert_eq!(
        bus.read_memory(0x10, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap(),
        0x1234_5678
    );
}
