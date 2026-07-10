// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

#[allow(clippy::too_many_arguments)] // a test-only MZ header builder; one field per param is clearer than a struct here
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
    let env_seg = place_environment(
        &mut mem,
        0x0100,
        0x1100,
        &[("BLASTER", "A220 I5 D1 H5 P300 T6")],
    )
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
    assert_eq!(s, b"BLASTER=A220 I5 D1 H5 P300 T6");
}
