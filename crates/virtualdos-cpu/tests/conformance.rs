use serde::Deserialize;

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
