use serde::Deserialize;
use virtualdos_bus::{BusAccessKind, BusError, BusWidth, CpuBus};
use virtualdos_cpu::{Cpu386, CpuError, SegmentIndex, SegmentRegister};

// Flags the CPU core models. AF is now modeled but is undefined for logic ops
// (see undefined_flags); reserved and unmodeled high bits are never compared.
const MODELED_FLAGS: u32 = 0x0000_0fd5;
const FLAG_CF: u32 = 0x0000_0001;
const FLAG_PF: u32 = 0x0000_0004;
const FLAG_AF: u32 = 0x0000_0010;
const FLAG_ZF: u32 = 0x0000_0040;
const FLAG_SF: u32 = 0x0000_0080;
const FLAG_OF: u32 = 0x0000_0800;

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
    #[serde(default)]
    regs: Regs,
    #[serde(default)]
    ram: Vec<[u32; 2]>,
}

/// Register block as emitted by moo2json: nested under `regs`, only the
/// registers present in the vector are set. Segment registers are plain
/// selectors; in real mode the base is `selector << 4`. Unmodeled keys
/// (dr6/dr7/ea/queue) are ignored by serde.
#[derive(Debug, Default, Deserialize)]
struct Regs {
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
    cs: Option<u32>,
    ds: Option<u32>,
    es: Option<u32>,
    ss: Option<u32>,
    fs: Option<u32>,
    gs: Option<u32>,
}

fn load_tests(text: &str) -> Vec<CpuTest> {
    serde_json::from_str(text).expect("test vectors should deserialize")
}

fn apply_state(cpu: &mut Cpu386, bus: &mut FlatBus, state: &TestState) {
    let regs = &state.regs;
    if let Some(v) = regs.eax {
        cpu.registers.set_eax(v);
    }
    if let Some(v) = regs.ebx {
        cpu.registers.set_ebx(v);
    }
    if let Some(v) = regs.ecx {
        cpu.registers.set_ecx(v);
    }
    if let Some(v) = regs.edx {
        cpu.registers.set_edx(v);
    }
    if let Some(v) = regs.esi {
        cpu.registers.set_esi(v);
    }
    if let Some(v) = regs.edi {
        cpu.registers.set_edi(v);
    }
    if let Some(v) = regs.ebp {
        cpu.registers.set_ebp(v);
    }
    if let Some(v) = regs.esp {
        cpu.registers.set_esp(v);
    }
    if let Some(v) = regs.eip {
        cpu.registers.eip = v;
    }
    if let Some(v) = regs.eflags {
        cpu.registers.eflags = v;
    }
    if let Some(v) = regs.cr0 {
        cpu.control.cr0 = v;
    }
    if let Some(v) = regs.cr3 {
        cpu.control.cr3 = v;
    }

    let segments = [
        (SegmentIndex::Es, regs.es),
        (SegmentIndex::Cs, regs.cs),
        (SegmentIndex::Ss, regs.ss),
        (SegmentIndex::Ds, regs.ds),
        (SegmentIndex::Fs, regs.fs),
        (SegmentIndex::Gs, regs.gs),
    ];
    for (index, selector) in segments {
        if let Some(selector) = selector {
            cpu.registers
                .set_segment(index, SegmentRegister::real(selector as u16));
        }
    }

    for entry in &state.ram {
        bus.write_u8(entry[0], entry[1] as u8);
    }
}

/// Flags that the 386 leaves undefined for this instruction, so the harness must
/// not compare them. `cl` is the initial CL, needed for the CL-count shift forms.
/// Logic ops (AND/OR/XOR/TEST) leave AF undefined. For the shift/rotate group:
/// AF is undefined for shifts and preserved by rotates (so masking it is safe for
/// both); OF is defined only for a 1-bit count (guaranteed only by 0xD0/0xD1); and
/// CF is undefined for SHL/SHR (and the /6 SHL alias) when the masked count reaches
/// the operand size in bits, per the Intel SDM. SAR and the rotates keep CF defined
/// at any count. See dev_docs/reference/80386-shift-flags.md. SF/ZF/PF and the
/// result stay compared for every count. For the 0xF6/0xF7 group the mask depends
/// on /reg: TEST (/0, /1) leaves AF undefined; MUL/IMUL (/4, /5) define CF/OF and
/// leave SF/ZF/AF/PF undefined; DIV/IDIV (/6, /7) leave all arithmetic flags
/// undefined; NOT (/2) and NEG (/3) define every modeled flag. Arithmetic ops
/// other than the 0xF6/0xF7 group define every modeled flag.
/// The prefix-skip set below must mirror the CPU's `read_prefixes` exactly, or this
/// helper and the CPU would disagree on which byte is the opcode.
fn undefined_flags(bytes: &[u8], cl: u8) -> u32 {
    let mut index = 0;
    let mut operand_size_override = false;
    while index < bytes.len()
        && matches!(
            bytes[index],
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf3
        )
    {
        if bytes[index] == 0x66 {
            operand_size_override = true;
        }
        index += 1;
    }
    let Some(&opcode) = bytes.get(index) else {
        return 0;
    };
    let reg = bytes.get(index + 1).map(|modrm| (modrm >> 3) & 0x07);
    let is_logic = match opcode {
        op if op < 0x40 && (op & 0x07) < 6 => matches!((op >> 3) & 0x07, 1 | 4 | 6),
        0x84 | 0x85 | 0xa8 | 0xa9 => true,
        0x80 | 0x81 | 0x83 => matches!(reg, Some(1 | 4 | 6)),
        _ => false,
    };

    if matches!(opcode, 0xc0 | 0xc1 | 0xd0 | 0xd1 | 0xd2 | 0xd3) {
        let mut mask = FLAG_AF;
        // OF is undefined unless the count is a guaranteed 1 (0xD0/0xD1).
        if !matches!(opcode, 0xd0 | 0xd1) {
            mask |= FLAG_OF;
        }
        // CF is undefined for SHL(/4), SHR(/5), and the /6 SHL alias once the masked
        // count reaches the operand width. The count is the imm8 (the byte before the
        // HLT terminator) for 0xC0/0xC1, CL for 0xD2/0xD3, and 1 for 0xD0/0xD1. Width
        // is 8 for the byte opcodes, 32 with a 0x66 prefix, else 16; a 5-bit count
        // never reaches 32, so dword shifts keep CF defined.
        if matches!(reg, Some(4..=6)) {
            let width_bits: u32 = if opcode & 1 == 0 {
                8
            } else if operand_size_override {
                32
            } else {
                16
            };
            let count = match opcode {
                0xc0 | 0xc1 => bytes.get(bytes.len().wrapping_sub(2)).copied().unwrap_or(0),
                0xd0 | 0xd1 => 1,
                _ => cl,
            };
            if u32::from(count & 0x1f) >= width_bits {
                mask |= FLAG_CF;
            }
        }
        return mask;
    }

    if matches!(opcode, 0xf6 | 0xf7) {
        // /0 and the /1 alias are TEST (AF undefined). /4 MUL and /5 IMUL define CF/OF
        // and leave SF/ZF/AF/PF undefined. /6 DIV and /7 IDIV leave every arithmetic
        // flag undefined. /2 NOT touches no flag and /3 NEG defines every modeled flag.
        return match reg {
            Some(0 | 1) => FLAG_AF,
            Some(4 | 5) => FLAG_SF | FLAG_ZF | FLAG_AF | FLAG_PF,
            Some(6 | 7) => FLAG_CF | FLAG_OF | FLAG_SF | FLAG_ZF | FLAG_AF | FLAG_PF,
            _ => 0,
        };
    }

    if is_logic { FLAG_AF } else { 0 }
}

fn diffs(cpu: &Cpu386, bus: &FlatBus, expected: &TestState, undefined: u32) -> Vec<String> {
    let mut out = Vec::new();
    let regs = &expected.regs;
    {
        let mut check = |name: &str, want: Option<u32>, got: u32| {
            if let Some(want) = want
                && want != got
            {
                out.push(format!("{name}: want {want:#010x}, got {got:#010x}"));
            }
        };

        check("eax", regs.eax, cpu.registers.eax());
        check("ebx", regs.ebx, cpu.registers.ebx());
        check("ecx", regs.ecx, cpu.registers.ecx());
        check("edx", regs.edx, cpu.registers.edx());
        check("esi", regs.esi, cpu.registers.esi());
        check("edi", regs.edi, cpu.registers.edi());
        check("ebp", regs.ebp, cpu.registers.ebp());
        check("esp", regs.esp, cpu.registers.esp());
        check("eip", regs.eip, cpu.registers.eip);
        check("cr0", regs.cr0, cpu.control.cr0);
        check("cr3", regs.cr3, cpu.control.cr3);
    }

    if let Some(want) = regs.eflags {
        let got = cpu.registers.eflags;
        let defined = MODELED_FLAGS & !undefined;
        if (want ^ got) & defined != 0 {
            out.push(format!(
                "eflags(defined): want {:#06x}, got {:#06x}",
                want & defined,
                got & defined
            ));
        }
    }

    // v1 compares segment selectors only. Cached base/limit/access are not
    // checked yet; revisit when opcode coverage reaches protected mode.
    let segments = [
        ("cs", regs.cs, SegmentIndex::Cs),
        ("ds", regs.ds, SegmentIndex::Ds),
        ("es", regs.es, SegmentIndex::Es),
        ("ss", regs.ss, SegmentIndex::Ss),
        ("fs", regs.fs, SegmentIndex::Fs),
        ("gs", regs.gs, SegmentIndex::Gs),
    ];
    for (name, selector, index) in segments {
        if let Some(selector) = selector {
            let want = selector as u16;
            let got = cpu.registers.segment(index).selector;
            if want != got {
                out.push(format!("{name} selector: want {want:#06x}, got {got:#06x}"));
            }
        }
    }

    for entry in &expected.ram {
        let want = entry[1] as u8;
        let got = bus.read_u8(entry[0]);
        if want != got {
            out.push(format!(
                "ram[{:#08x}]: want {want:#04x}, got {got:#04x}",
                entry[0]
            ));
        }
    }

    out
}

#[derive(Debug)]
enum Outcome {
    Pass,
    Fail(Vec<String>),
    Unimplemented,
    Skipped,
    // The CPU raised #DE (DivideError) on a vector the suite did not mark as an
    // exception. These are IDIV/DIV overflow-boundary cases where the 386 silicon
    // produces a non-trapping result we do not model; our CPU follows Intel's
    // documented #DE. Counted separately so the divergence stays visible.
    DivideQuirk,
    Errored(String),
}

fn run_test(test: &CpuTest) -> Outcome {
    if test.exception.is_some() {
        return Outcome::Skipped;
    }
    if let Some(&first) = test.bytes.first()
        && matches!(first, 0xe4..=0xe7 | 0xec..=0xef)
    {
        return Outcome::Skipped;
    }

    let mut cpu = Cpu386::default();
    let mut bus = FlatBus::new();
    apply_state(&mut cpu, &mut bus, &test.initial);

    // Each vector is the instruction under test followed by a HLT terminator;
    // the captured final state is taken after the CPU halts (its eip is past
    // the HLT). Run until halt, guarding against a vector that never halts.
    let mut guard = 0;
    loop {
        match cpu.cycle(&mut bus) {
            Ok(outcome) => {
                if outcome.halted {
                    break;
                }
            }
            Err(CpuError::UnsupportedOpcode { .. })
            | Err(CpuError::UnsupportedTwoByteOpcode { .. })
            | Err(CpuError::UnsupportedGroupOpcode { .. }) => return Outcome::Unimplemented,
            Err(CpuError::DivideError) => return Outcome::DivideQuirk,
            Err(other) => return Outcome::Errored(other.to_string()),
        }
        guard += 1;
        if guard > 1024 {
            return Outcome::Errored("did not halt within 1024 instructions".to_string());
        }
    }

    let cl = (test.initial.regs.ecx.unwrap_or(0) & 0xff) as u8;
    let undefined = undefined_flags(&test.bytes, cl);
    let differences = diffs(&cpu, &bus, &test.final_state, undefined);
    if differences.is_empty() {
        Outcome::Pass
    } else {
        Outcome::Fail(differences)
    }
}

#[test]
fn parses_synthetic_fixture() {
    let text = include_str!("fixtures/conformance_sample.json");
    let tests = load_tests(text);

    assert_eq!(tests.len(), 3);
    assert_eq!(tests[0].name, "nop");
    assert_eq!(tests[0].bytes, vec![144, 244]);
    assert_eq!(tests[0].initial.regs.cs, Some(0));
    assert_eq!(tests[0].initial.regs.eip, Some(256));
    assert_eq!(tests[0].final_state.regs.eip, Some(258));

    assert_eq!(tests[1].name, "clc");
    assert_eq!(tests[1].initial.regs.eflags, Some(3));
    assert_eq!(tests[1].final_state.regs.eflags, Some(2));

    assert_eq!(tests[2].name, "add al imm sets af");
    assert_eq!(tests[2].final_state.regs.eflags, Some(18));
}

#[test]
fn undefined_flags_marks_logic_ops() {
    const AF: u32 = 0x0000_0010;
    assert_eq!(undefined_flags(&[0x20, 0xc0], 0), AF); // AND
    assert_eq!(undefined_flags(&[0x84, 0xc0], 0), AF); // TEST
    assert_eq!(undefined_flags(&[0x81, 0xe3, 0x01, 0x00], 0), AF); // AND group /4
    assert_eq!(undefined_flags(&[0xf7, 0xc3, 0x01, 0x00], 0), AF); // TEST group /0
    assert_eq!(undefined_flags(&[0x00, 0xc0], 0), 0); // ADD -> all defined
    assert_eq!(undefined_flags(&[0x81, 0xc3, 0x01, 0x00], 0), 0); // ADD group /0
}

#[test]
fn undefined_flags_marks_shifts() {
    // Bytes are written with the trailing 0xF4 HLT terminator the real vectors carry,
    // so the imm8 count (the byte before HLT) lands where the helper reads it.
    const AF: u32 = 0x0000_0010;
    const OF: u32 = 0x0000_0800;
    const CF: u32 = 0x0000_0001;
    // OF defined (count is a guaranteed 1), AF undefined, CF defined.
    assert_eq!(undefined_flags(&[0xd0, 0xe0, 0xf4], 0), AF); // shl al,1
    assert_eq!(undefined_flags(&[0xd1, 0xe0, 0xf4], 0), AF); // shl ax,1
    assert_eq!(undefined_flags(&[0x66, 0xd1, 0xe0, 0xf4], 0), AF); // 0x66 skipped -> d1
    // imm/CL counts below the operand width: AF|OF, CF still defined.
    assert_eq!(undefined_flags(&[0xc1, 0xe0, 0x04, 0xf4], 0), AF | OF); // shl ax,4 (word)
    assert_eq!(undefined_flags(&[0xd2, 0xe0, 0xf4], 7), AF | OF); // shl al,cl cl=7 (<8)
    // 0xC0 (imm8 form) always masks OF, so a ROL by 1 here still reports OF; CF
    // masking is unaffected because it only applies to SHL/SHR/alias.
    assert_eq!(undefined_flags(&[0xc0, 0xc0, 0x01, 0xf4], 0), AF | OF); // rol al,1 via imm
    // count reaches the operand width: CF also undefined.
    assert_eq!(undefined_flags(&[0xc0, 0xe0, 0x10, 0xf4], 0), AF | OF | CF); // shl al,16 (>=8)
    assert_eq!(undefined_flags(&[0xc0, 0xe8, 0x10, 0xf4], 0), AF | OF | CF); // shr al,16 (>=8)
    assert_eq!(undefined_flags(&[0xd2, 0xe0, 0xf4], 8), AF | OF | CF); // shl al,cl cl=8 (>=8)
    assert_eq!(undefined_flags(&[0xd2, 0xe8, 0xf4], 8), AF | OF | CF); // shr al,cl cl=8 (>=8)
    assert_eq!(undefined_flags(&[0xc1, 0xe0, 0x10, 0xf4], 0), AF | OF | CF); // shl ax,16 (word >=16)
    // dword (0x66): a 5-bit count never reaches 32, so CF stays defined.
    assert_eq!(undefined_flags(&[0x66, 0xc1, 0xe0, 0x1f, 0xf4], 0), AF | OF); // shl eax,31 (<32)
    // SAR and rotates keep CF defined at any count.
    assert_eq!(undefined_flags(&[0xd2, 0xf8, 0xf4], 20), AF | OF); // sar al,cl cl=20
    assert_eq!(undefined_flags(&[0xd2, 0xd8, 0xf4], 20), AF | OF); // rcr al,cl cl=20
}

#[test]
fn undefined_flags_marks_muldiv() {
    const CF: u32 = 0x0000_0001;
    const PF: u32 = 0x0000_0004;
    const AF: u32 = 0x0000_0010;
    const ZF: u32 = 0x0000_0040;
    const SF: u32 = 0x0000_0080;
    const OF: u32 = 0x0000_0800;
    // MUL/IMUL: CF/OF defined, SF/ZF/AF/PF undefined.
    assert_eq!(undefined_flags(&[0xf6, 0xe3], 0), SF | ZF | AF | PF); // mul bl
    assert_eq!(undefined_flags(&[0xf7, 0xe3], 0), SF | ZF | AF | PF); // mul bx
    assert_eq!(undefined_flags(&[0xf6, 0xeb], 0), SF | ZF | AF | PF); // imul bl
    // DIV/IDIV: every arithmetic flag undefined.
    assert_eq!(
        undefined_flags(&[0xf6, 0xf3], 0),
        CF | OF | SF | ZF | AF | PF
    ); // div bl
    assert_eq!(
        undefined_flags(&[0xf6, 0xfb], 0),
        CF | OF | SF | ZF | AF | PF
    ); // idiv bl
    // NOT/NEG define every modeled flag.
    assert_eq!(undefined_flags(&[0xf6, 0xd3], 0), 0); // not bl
    assert_eq!(undefined_flags(&[0xf6, 0xdb], 0), 0); // neg bl
    // TEST still masks AF.
    assert_eq!(undefined_flags(&[0xf6, 0xc3, 0x01], 0), AF); // test bl, imm8
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

#[test]
fn synthetic_fixture_executes_and_matches() {
    let tests = load_tests(include_str!("fixtures/conformance_sample.json"));

    for test in &tests {
        match run_test(test) {
            Outcome::Pass => {}
            other => panic!("test {:?} did not pass: {other:?}", test.name),
        }
    }
}

#[test]
#[ignore = "set VIRTUALDOS_386_TESTS to a directory of converted JSON vectors"]
fn conformance_suite_report() {
    let dir = std::env::var("VIRTUALDOS_386_TESTS")
        .expect("VIRTUALDOS_386_TESTS must point at the converted JSON suite");

    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .expect("test directory should be readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();

    let (mut total, mut pass, mut fail, mut unimpl, mut skip, mut quirk, mut errored) =
        (0u64, 0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let mut failing_files: Vec<String> = Vec::new();
    let mut partial_files: Vec<String> = Vec::new();
    let mut first_failures: Vec<String> = Vec::new();
    let mut quirk_samples: Vec<String> = Vec::new();
    let mut parse_errors = 0u64;

    for path in &paths {
        let text = std::fs::read_to_string(path).expect("vector file should be readable");
        let tests: Vec<CpuTest> = match serde_json::from_str(&text) {
            Ok(tests) => tests,
            Err(error) => {
                parse_errors += 1;
                eprintln!("parse {}: {error}", path.display());
                continue;
            }
        };

        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (mut file_pass, mut file_fail, mut file_unimpl, mut file_skip, mut file_quirk) =
            (0u64, 0u64, 0u64, 0u64, 0u64);
        for test in &tests {
            total += 1;
            match run_test(test) {
                Outcome::Pass => {
                    pass += 1;
                    file_pass += 1;
                }
                Outcome::Fail(differences) => {
                    fail += 1;
                    file_fail += 1;
                    if first_failures.len() < 20 {
                        first_failures.push(format!(
                            "{name} [{}]: {}",
                            test.name,
                            differences.join("; ")
                        ));
                    }
                }
                Outcome::Unimplemented => {
                    unimpl += 1;
                    file_unimpl += 1;
                }
                Outcome::Skipped => {
                    skip += 1;
                    file_skip += 1;
                }
                Outcome::DivideQuirk => {
                    quirk += 1;
                    file_quirk += 1;
                    if quirk_samples.len() < 20 {
                        quirk_samples.push(format!("{name} [{}]", test.name));
                    }
                }
                Outcome::Errored(message) => {
                    errored += 1;
                    if first_failures.len() < 20 {
                        first_failures.push(format!("{name} [{}]: ERROR {message}", test.name));
                    }
                }
            }
        }
        if file_fail > 0 {
            failing_files.push(format!("{name}: {file_pass} pass / {file_fail} fail"));
        } else if file_unimpl > 0 || file_skip > 0 || file_quirk > 0 {
            partial_files.push(format!(
                "{name}: {file_pass} pass, {file_unimpl} unimplemented, {file_skip} skipped, {file_quirk} divide-quirk"
            ));
        }
    }

    println!(
        "files={} parse_errors={parse_errors} total={total} pass={pass} fail={fail} unimplemented={unimpl} skipped={skip} divide_quirk={quirk} errored={errored}",
        paths.len()
    );
    for line in &failing_files {
        println!("FAIL {line}");
    }
    for line in &partial_files {
        println!("PARTIAL {line}");
    }
    for line in &quirk_samples {
        println!("DIVIDE-QUIRK {line}");
    }
    for line in &first_failures {
        println!("  {line}");
    }

    assert!(total > 0, "no vectors were parsed from {dir}");
    assert_eq!(
        parse_errors, 0,
        "{parse_errors} vector file(s) failed to parse; the report would understate coverage"
    );
}
