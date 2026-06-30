//! A native .COM/.EXE loader with no DOS-kernel backing — builds a
//! DOS-compatible PSP/memory image so a guest program can run, without any
//! service-dispatch state attached. Ported from `izarravm-dos`'s loader
//! (which is identical in spirit but carries a full kernel struct this
//! crate doesn't want). See `dev_docs/2026-06-30-katea-sp3-program-runtime-design.md`.

use izarravm_bus::Memory;

/// Conventional memory's top paragraph for a directly-loaded program: 0xA000
/// (640 KiB), matching real DOS and the ported loader's prior behavior.
const ARENA_TOP: u16 = 0xa000;

/// The largest .COM image: a 64 KiB segment minus the 256-byte PSP.
const COM_MAX_LEN: usize = 0x10000 - 0x100;

/// The default Job File Table length DOS reports in PSP:0x32 (20 handles).
const JFT_LEN: usize = 20;

/// The argv0 path placed in the environment trailer. No caller in this crate
/// reads it back; it exists so the env block matches real DOS's format.
const DEFAULT_ARGV0: &str = "C:\\PROGRAM.EXE";

#[derive(Debug, thiserror::Error)]
pub enum ProgramLoadError {
    #[error(".COM image is {0} bytes, larger than the 64 KiB segment minus PSP")]
    ComTooLarge(usize),
    #[error("not a valid MZ .EXE: bad signature")]
    BadExeSignature,
    #[error("EXE image truncated: {0}")]
    ExeImageTruncated(&'static str),
    #[error("EXE relocation target is out of range")]
    ExeRelocationOutOfRange,
    #[error("not enough conventional memory: needed {needed} paragraphs, {available} available")]
    ExeNotEnoughMemory { needed: u32, available: u32 },
    #[error(transparent)]
    Bus(#[from] izarravm_bus::BusError),
}

/// Where to start executing a loaded program. A .COM sets all six to its
/// single load segment; an .EXE sets a distinct CS:IP and SS:SP with
/// DS=ES=PSP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramEntry {
    pub cs: u16,
    pub ip: u16,
    pub ss: u16,
    pub sp: u16,
    pub ds: u16,
    pub es: u16,
}

/// Build the 256-byte PSP at psp_seg:0. INT 20h (CD 20) at offset 0 so a near
/// RET to PSP:0 terminates; the top-of-memory paragraph at 0x02; the CP/M
/// CALL 5 far call at 0x05 (unused by this crate's minimal interrupt
/// surface, kept only so the PSP bytes match real DOS layout for tests that
/// read it directly); the documented vectors at 0x0A/0x0E/0x12 snapshot the
/// current INT 22h/23h/24h IVT entries; the 20-byte JFT at 0x18 wires
/// stdin/stdout/stderr to CON. The environment segment (0x2C) is filled in
/// by `place_environment` below.
fn build_psp(
    mem: &mut Memory,
    psp_seg: u16,
    top_of_mem_paragraph: u16,
) -> Result<(), ProgramLoadError> {
    let base = usize::from(psp_seg) * 16;
    mem.write_u8(base, 0xcd)?;
    mem.write_u8(base + 1, 0x20)?;
    mem.write_u16(base + 2, top_of_mem_paragraph)?;
    mem.write_u8(base + 0x05, 0x9a)?; // call far 0000:00C0
    mem.write_u16(base + 0x06, 0x00c0)?;
    mem.write_u16(base + 0x08, 0x0000)?;
    for (psp_off, int_no) in [(0x0au16, 0x22u8), (0x0e, 0x23), (0x12, 0x24)] {
        let ivt = usize::from(int_no) * 4;
        mem.write_u16(base + usize::from(psp_off), mem.read_u16(ivt)?)?;
        mem.write_u16(base + usize::from(psp_off) + 2, mem.read_u16(ivt + 2)?)?;
    }
    mem.write_u16(base + 0x16, 0)?; // parent PSP: none
    for off in 0..JFT_LEN {
        mem.write_u8(base + 0x18 + off, 0xff)?;
    }
    mem.write_u8(base + 0x18, 0x01)?; // stdin -> CON
    mem.write_u8(base + 0x19, 0x01)?; // stdout -> CON
    mem.write_u8(base + 0x1a, 0x01)?; // stderr -> CON
    mem.write_u8(base + 0x1b, 0x03)?; // stdaux -> AUX
    mem.write_u8(base + 0x1c, 0x04)?; // stdprn -> PRN
    mem.write_u16(base + 0x32, JFT_LEN as u16)?;
    mem.write_u16(base + 0x34, 0x0018)?;
    mem.write_u16(base + 0x36, psp_seg)?;
    mem.write_u8(base + 0x50, 0xcd)?; // INT 21h then RETF helper
    mem.write_u8(base + 0x51, 0x21)?;
    mem.write_u8(base + 0x52, 0xcb)?;
    mem.write_u8(base + 0x53, 0)?;
    mem.write_u8(base + 0x54, 0)?;
    mem.write_u8(base + 0x80, 0x00)?; // empty command tail
    mem.write_u8(base + 0x81, 0x0d)?;
    Ok(())
}

/// Format a DOS environment block plus the DOS 3.0+ argv0 trailer: ASCIIZ
/// `KEY=VALUE` strings, an empty-string terminator, a WORD count of 0x0001,
/// then `argv0`.
fn build_env_block_with_argv0(entries: &[(&str, &str)], argv0: &str) -> Vec<u8> {
    let mut block = Vec::new();
    for (key, value) in entries {
        block.extend_from_slice(key.as_bytes());
        block.push(b'=');
        block.extend_from_slice(value.as_bytes());
        block.push(0);
    }
    block.push(0); // the terminating empty string
    block.extend_from_slice(&1u16.to_le_bytes());
    block.extend_from_slice(argv0.as_bytes());
    block.push(0);
    block
}

/// Place an environment block one paragraph above `prog_top` and record its
/// segment in PSP:0x2C. No MCB/arena bookkeeping — this is a fixed,
/// deterministic placement (matches `prog_top + 1`, what every caller in
/// this crate expects), not a general allocator. Safe because nothing in
/// this crate's surviving callers walks the MCB chain or allocates more
/// conventional memory at runtime.
pub fn place_environment(
    mem: &mut Memory,
    psp_seg: u16,
    prog_top: u16,
    entries: &[(&str, &str)],
) -> Result<u16, ProgramLoadError> {
    let block = build_env_block_with_argv0(entries, DEFAULT_ARGV0);
    let env_seg = prog_top.wrapping_add(1);
    let env_base = usize::from(env_seg) * 16;
    for (offset, &byte) in block.iter().enumerate() {
        mem.write_u8(env_base + offset, byte)?;
    }
    mem.write_u16(usize::from(psp_seg) * 16 + 0x2c, env_seg)?;
    Ok(env_seg)
}

/// Load a .COM image into `mem` at `segment` and build its PSP. Returns the
/// entry state for the caller to apply to the CPU.
pub fn load_com(
    image: &[u8],
    mem: &mut Memory,
    segment: u16,
) -> Result<ProgramEntry, ProgramLoadError> {
    if image.len() > COM_MAX_LEN {
        return Err(ProgramLoadError::ComTooLarge(image.len()));
    }
    build_psp(mem, segment, segment.wrapping_add(0x1000))?;
    let base = usize::from(segment) * 16;
    for (index, &byte) in image.iter().enumerate() {
        mem.write_u8(base + 0x100 + index, byte)?;
    }
    // .COM stack: SP=0xFFFE with a 0x0000 return word, so a bare RET lands
    // at PSP:0 and hits the INT 20h.
    mem.write_u16(base + 0xfffe, 0x0000)?;
    Ok(ProgramEntry {
        cs: segment,
        ip: 0x0100,
        ss: segment,
        sp: 0xfffe,
        ds: segment,
        es: segment,
    })
}

struct ParsedExe<'a> {
    e_ss: u16,
    e_sp: u16,
    e_ip: u16,
    e_cs: u16,
    e_minalloc: u16,
    e_maxalloc: u16,
    module: &'a [u8],
    relocs: &'a [u8],
}

fn parse_exe(image: &[u8]) -> Result<ParsedExe<'_>, ProgramLoadError> {
    if image.len() < 0x1c {
        return Err(ProgramLoadError::ExeImageTruncated(
            "header shorter than 0x1C bytes",
        ));
    }
    let word = |off: usize| u16::from_le_bytes([image[off], image[off + 1]]);
    if word(0x00) != 0x5a4d {
        return Err(ProgramLoadError::BadExeSignature);
    }
    let e_cblp = word(0x02);
    let e_cp = word(0x04);
    let e_crlc = word(0x06);
    let e_cparhdr = word(0x08);
    let e_minalloc = word(0x0a);
    let e_maxalloc = word(0x0c);
    let e_ss = word(0x0e);
    let e_sp = word(0x10);
    let e_ip = word(0x14);
    let e_cs = word(0x16);
    let e_lfarlc = word(0x18);

    if e_cp == 0 {
        return Err(ProgramLoadError::ExeImageTruncated(
            "page count e_cp is zero",
        ));
    }
    if e_cblp > 512 {
        return Err(ProgramLoadError::ExeImageTruncated(
            "bytes-on-last-page e_cblp exceeds 512",
        ));
    }
    let header_bytes = usize::from(e_cparhdr) * 16;
    let last_page = if e_cblp != 0 {
        512 - usize::from(e_cblp)
    } else {
        0
    };
    let image_size = usize::from(e_cp) * 512 - last_page;
    if header_bytes > image_size || image_size > image.len() {
        return Err(ProgramLoadError::ExeImageTruncated(
            "load module extends past the file",
        ));
    }
    let module = &image[header_bytes..image_size];
    let reloc_end = usize::from(e_lfarlc) + usize::from(e_crlc) * 4;
    if reloc_end > image.len() {
        return Err(ProgramLoadError::ExeImageTruncated(
            "relocation table extends past the file",
        ));
    }
    let relocs = &image[usize::from(e_lfarlc)..reloc_end];
    Ok(ParsedExe {
        e_ss,
        e_sp,
        e_ip,
        e_cs,
        e_minalloc,
        e_maxalloc,
        module,
        relocs,
    })
}

fn apply_relocs(
    mem: &mut Memory,
    base: usize,
    module_len: usize,
    relocs: &[u8],
    addend: u16,
) -> Result<(), ProgramLoadError> {
    for entry in relocs.chunks_exact(4) {
        let off = u16::from_le_bytes([entry[0], entry[1]]);
        let seg = u16::from_le_bytes([entry[2], entry[3]]);
        let module_offset = usize::from(seg) * 16 + usize::from(off);
        if module_offset + 2 > module_len {
            return Err(ProgramLoadError::ExeRelocationOutOfRange);
        }
        let target = base + module_offset;
        let value = mem.read_u16(target)?;
        mem.write_u16(target, value.wrapping_add(addend))?;
    }
    Ok(())
}

/// Load a DOS MZ/.EXE into `mem`. Conventional memory is one block ending at
/// paragraph 0xA000; `e_minalloc` is enforced and `e_maxalloc` clamps the
/// PSP top-of-memory word.
pub fn load_exe(
    image: &[u8],
    mem: &mut Memory,
    psp_segment: u16,
) -> Result<ProgramEntry, ProgramLoadError> {
    let exe = parse_exe(image)?;
    let module_len = exe.module.len();
    let module_paras = module_len.div_ceil(16) as u32;

    let start_seg = psp_segment.wrapping_add(0x10);
    let needed = u32::from(start_seg) + module_paras + u32::from(exe.e_minalloc);
    if needed > u32::from(ARENA_TOP) {
        return Err(ProgramLoadError::ExeNotEnoughMemory {
            needed,
            available: u32::from(ARENA_TOP),
        });
    }
    let top_paragraph = (u32::from(start_seg) + module_paras + u32::from(exe.e_maxalloc))
        .min(u32::from(ARENA_TOP)) as u16;
    build_psp(mem, psp_segment, top_paragraph)?;

    let base = usize::from(start_seg) * 16;
    for (index, &byte) in exe.module.iter().enumerate() {
        mem.write_u8(base + index, byte)?;
    }
    apply_relocs(mem, base, module_len, exe.relocs, start_seg)?;

    Ok(ProgramEntry {
        cs: start_seg.wrapping_add(exe.e_cs),
        ip: exe.e_ip,
        ss: start_seg.wrapping_add(exe.e_ss),
        sp: exe.e_sp,
        ds: psp_segment,
        es: psp_segment,
    })
}

/// Load a .COM or .EXE by signature ("MZ" at the start of the image means
/// .EXE) and build its PSP.
pub fn load_program(
    image: &[u8],
    mem: &mut Memory,
    psp_segment: u16,
) -> Result<ProgramEntry, ProgramLoadError> {
    if image.len() >= 2 && image[0] == b'M' && image[1] == b'Z' {
        load_exe(image, mem, psp_segment)
    } else {
        load_com(image, mem, psp_segment)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_mz(
        module: &[u8],
        relocs: &[(u16, u16)],
        e_cs: u16,
        e_ip: u16,
        e_ss: u16,
        e_sp: u16,
        e_minalloc: u16,
        e_maxalloc: u16,
    ) -> Vec<u8> {
        let e_cparhdr: u16 = 2;
        let header_bytes = usize::from(e_cparhdr) * 16;
        assert!(
            0x1c + relocs.len() * 4 <= header_bytes,
            "relocs overflow header"
        );
        let total = header_bytes + module.len();
        let e_cp = total.div_ceil(512) as u16;
        let e_cblp = (total % 512) as u16;
        let e_lfarlc: u16 = 0x1c;
        let mut img = vec![0u8; total];
        img[0..2].copy_from_slice(b"MZ");
        img[2..4].copy_from_slice(&e_cblp.to_le_bytes());
        img[4..6].copy_from_slice(&e_cp.to_le_bytes());
        img[6..8].copy_from_slice(&(relocs.len() as u16).to_le_bytes());
        img[8..10].copy_from_slice(&e_cparhdr.to_le_bytes());
        img[10..12].copy_from_slice(&e_minalloc.to_le_bytes());
        img[12..14].copy_from_slice(&e_maxalloc.to_le_bytes());
        img[14..16].copy_from_slice(&e_ss.to_le_bytes());
        img[16..18].copy_from_slice(&e_sp.to_le_bytes());
        img[20..22].copy_from_slice(&e_ip.to_le_bytes());
        img[22..24].copy_from_slice(&e_cs.to_le_bytes());
        img[24..26].copy_from_slice(&e_lfarlc.to_le_bytes());
        for (i, (off, seg)) in relocs.iter().enumerate() {
            let p = 0x1c + i * 4;
            img[p..p + 2].copy_from_slice(&off.to_le_bytes());
            img[p + 2..p + 4].copy_from_slice(&seg.to_le_bytes());
        }
        img[header_bytes..].copy_from_slice(module);
        img
    }

    #[test]
    fn load_com_builds_psp_and_entry() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = [0xb8, 0x00, 0x4c, 0xcd, 0x21]; // mov ax,4c00; int 21
        let entry = load_com(&image, &mut mem, 0x0100).unwrap();
        assert_eq!(
            entry,
            ProgramEntry {
                cs: 0x0100,
                ip: 0x0100,
                ss: 0x0100,
                sp: 0xfffe,
                ds: 0x0100,
                es: 0x0100
            }
        );
        let base = 0x0100usize * 16;
        assert_eq!(mem.read_u8(base).unwrap(), 0xcd);
        assert_eq!(mem.read_u8(base + 1).unwrap(), 0x20);
        assert_eq!(mem.read_u16(base + 0x02).unwrap(), 0x1100);
        assert_eq!(mem.read_u8(base + 0x80).unwrap(), 0x00);
        assert_eq!(mem.read_u8(base + 0x81).unwrap(), 0x0d);
        assert_eq!(mem.read_u8(base + 0x100).unwrap(), 0xb8);
        assert_eq!(mem.read_u8(base + 0x104).unwrap(), 0x21);
        assert_eq!(mem.read_u16(base + 0xfffe).unwrap(), 0x0000);
    }

    #[test]
    fn load_com_rejects_oversize_image() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let len = 0x10000 - 0x100 + 1;
        let image = vec![0x90; len];
        match load_com(&image, &mut mem, 0x0100) {
            Err(ProgramLoadError::ComTooLarge(reported)) => assert_eq!(reported, len),
            other => panic!("expected ComTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn load_exe_parses_entry_and_places_module() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let module = [0xaa, 0xbb, 0xcc, 0xdd];
        let image = build_mz(&module, &[], 0x0002, 0x0010, 0x0001, 0x0200, 0x10, 0xffff);
        let psp = 0x0100u16;
        let start_seg = psp + 0x10;
        let entry = load_exe(&image, &mut mem, psp).unwrap();
        assert_eq!(entry.cs, start_seg + 0x0002);
        assert_eq!(entry.ip, 0x0010);
        assert_eq!(entry.ss, start_seg + 0x0001);
        assert_eq!(entry.sp, 0x0200);
        assert_eq!(entry.ds, psp);
        assert_eq!(entry.es, psp);
        let base = usize::from(start_seg) * 16;
        assert_eq!(mem.read_u8(base).unwrap(), 0xaa);
        assert_eq!(mem.read_u8(base + 3).unwrap(), 0xdd);
    }

    #[test]
    fn load_exe_applies_relocation() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let module = [0u8; 8];
        let image = build_mz(&module, &[(4u16, 0u16)], 0, 0, 0, 0x100, 0x10, 0xffff);
        let psp = 0x0100u16;
        let start_seg = psp + 0x10;
        load_exe(&image, &mut mem, psp).unwrap();
        let target = usize::from(start_seg) * 16 + 4;
        assert_eq!(mem.read_u16(target).unwrap(), start_seg);
    }

    #[test]
    fn load_exe_builds_psp() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let module = [0x90u8; 16];
        let image = build_mz(&module, &[], 0, 0, 0, 0x100, 0x10, 0x20);
        let psp = 0x0100u16;
        load_exe(&image, &mut mem, psp).unwrap();
        let base = usize::from(psp) * 16;
        assert_eq!(mem.read_u8(base).unwrap(), 0xcd);
        assert_eq!(mem.read_u8(base + 1).unwrap(), 0x20);
        assert_eq!(mem.read_u16(base + 2).unwrap(), 0x0131);
        assert_eq!(mem.read_u8(base + 0x80).unwrap(), 0x00);
        assert_eq!(mem.read_u8(base + 0x81).unwrap(), 0x0d);
    }

    #[test]
    fn load_exe_rejects_bad_signature() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut image = build_mz(&[0u8; 16], &[], 0, 0, 0, 0x100, 0x10, 0xffff);
        image[0] = b'X';
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(ProgramLoadError::BadExeSignature)
        ));
    }

    #[test]
    fn load_exe_rejects_truncated_header() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = [0x4d, 0x5a, 0x00];
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(ProgramLoadError::ExeImageTruncated(_))
        ));
    }

    #[test]
    fn load_exe_rejects_truncated_module() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut image = build_mz(&[0u8; 16], &[], 0, 0, 0, 0x100, 0x10, 0xffff);
        image[4..6].copy_from_slice(&9u16.to_le_bytes());
        image[2..4].copy_from_slice(&0u16.to_le_bytes());
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(ProgramLoadError::ExeImageTruncated(_))
        ));
    }

    #[test]
    fn load_exe_rejects_out_of_bounds_relocation() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = build_mz(&[0u8; 8], &[(100u16, 0u16)], 0, 0, 0, 0x100, 0x10, 0xffff);
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(ProgramLoadError::ExeRelocationOutOfRange)
        ));
    }

    #[test]
    fn load_exe_rejects_insufficient_memory() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = build_mz(&[0u8; 16], &[], 0, 0, 0, 0x100, 0xffff, 0xffff);
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(ProgramLoadError::ExeNotEnoughMemory { .. })
        ));
    }

    #[test]
    fn load_exe_rejects_oversized_e_cblp() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let mut image = build_mz(&[0u8; 16], &[], 0, 0, 0, 0x100, 0x10, 0xffff);
        image[2..4].copy_from_slice(&0x0201u16.to_le_bytes());
        assert!(matches!(
            load_exe(&image, &mut mem, 0x100),
            Err(ProgramLoadError::ExeImageTruncated(_))
        ));
    }

    #[test]
    fn load_program_routes_exe_by_signature() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = build_mz(
            &[0u8; 16],
            &[],
            0x0002,
            0x0010,
            0x0001,
            0x0200,
            0x10,
            0xffff,
        );
        let entry = load_program(&image, &mut mem, 0x0100).unwrap();
        assert_ne!(entry.cs, entry.ds);
        assert_eq!(entry.ds, 0x0100);
    }

    #[test]
    fn load_program_routes_com_when_no_mz() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        let image = [0xb8, 0x00, 0x4c, 0xcd, 0x21];
        let entry = load_program(&image, &mut mem, 0x0100).unwrap();
        assert_eq!(entry.cs, 0x0100);
        assert_eq!(entry.ds, 0x0100);
        assert_eq!(entry.ss, 0x0100);
        assert_eq!(entry.es, 0x0100);
        assert_eq!(entry.ip, 0x0100);
        assert_eq!(entry.sp, 0xfffe);
    }

    #[test]
    fn place_environment_sits_one_paragraph_above_prog_top() {
        let mut mem = Memory::new(1024 * 1024).unwrap();
        build_psp(&mut mem, 0x0100, 0x1100).unwrap();
        let env_seg =
            place_environment(&mut mem, 0x0100, 0x1100, &[("BLASTER", "A220 I5 D1 H5 T6")])
                .unwrap();
        assert_eq!(env_seg, 0x1101);
        assert_eq!(mem.read_u16(0x0100 * 16 + 0x2c).unwrap(), env_seg);
        let base = usize::from(env_seg) * 16;
        let mut s = Vec::new();
        let mut i = 0;
        loop {
            let b = mem.read_u8(base + i).unwrap();
            if b == 0 {
                break;
            }
            s.push(b);
            i += 1;
        }
        assert_eq!(s, b"BLASTER=A220 I5 D1 H5 T6");
    }
}
