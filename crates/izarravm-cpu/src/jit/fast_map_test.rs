// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

const MAPPING_EPOCH: u64 = 7;

fn page(bytes: &mut [u8; PAGE_SIZE], physical_page: u32, writable: bool) -> DirectPage {
    DirectPage {
        physical_page,
        ptr: bytes.as_mut_ptr(),
        len: bytes.len(),
        writable,
        mapping_epoch: MAPPING_EPOCH,
    }
}

#[test]
fn ram_read_and_write_fill_independent_biases() {
    let mut bytes = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();
    let linear = 0xc123_4567;
    let physical = 0x0012_3567;

    assert!(map.populate_read(
        linear,
        physical,
        page(&mut bytes, 0x0012_3000, false),
        PagePermissions {
            writable: false,
            user: true,
        },
        // Bare-map tests have no code watches; the PAGE_WATCHED bit's own coverage lives with the
        // watched-page-bit fixtures.
        false,
    ));
    let read = map.entry(linear);
    assert_eq!(read.kind(), PageKind::Ram);
    assert_eq!(read.physical_page, 0x0012_3000);
    assert_eq!(read.mapping_epoch, MAPPING_EPOCH);
    assert_eq!(
        read.read_ptr(linear),
        Some(bytes.as_mut_ptr().wrapping_add(0x567))
    );
    assert_eq!(read.write_ptr(linear), None);
    assert!(!read.writable());
    assert!(read.user());

    assert!(map.populate_write(
        linear,
        physical,
        page(&mut bytes, 0x0012_3000, true),
        PagePermissions {
            writable: true,
            user: false,
        },
        false,
    ));
    let write = map.entry(linear);
    assert_eq!(
        write.read_ptr(linear),
        Some(bytes.as_mut_ptr().wrapping_add(0x567))
    );
    assert_eq!(
        write.write_ptr(linear),
        Some(bytes.as_mut_ptr().wrapping_add(0x567))
    );
    assert!(write.writable());
    assert!(!write.user());
}

#[test]
fn interpreter_lookup_requires_a_live_bias_and_current_permissions() {
    let mut bytes = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();
    let linear = 0x8123_4564;
    let physical = 0x0012_3564;
    bytes[0x564..0x568].copy_from_slice(&0x4433_2211u32.to_le_bytes());

    assert!(map.populate_read(
        linear,
        physical,
        page(&mut bytes, 0x0012_3000, false),
        PagePermissions {
            writable: false,
            user: false,
        },
        false,
    ));
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, false, false, false),
        Some(physical)
    );
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, false, true, false),
        None
    );
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, true, false, false),
        None
    );
    assert_eq!(
        map.lookup_access(linear, MAPPING_EPOCH, BusWidth::Dword, false, false, false,)
            .unwrap()
            .read(BusWidth::Dword),
        0x4433_2211
    );

    assert!(map.populate_write(
        linear,
        physical,
        page(&mut bytes, 0x0012_3000, true),
        PagePermissions {
            writable: false,
            user: false,
        },
        false,
    ));
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, true, false, false),
        Some(physical)
    );
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, true, false, true),
        None
    );
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, true, true, false),
        None
    );
    map.lookup_access(linear, MAPPING_EPOCH, BusWidth::Dword, true, false, false)
        .unwrap()
        .write(BusWidth::Dword, 0xaabb_ccdd);
    assert_eq!(&bytes[0x564..0x568], &0xaabb_ccddu32.to_le_bytes());
    assert!(
        map.lookup_access(
            (linear & !PAGE_MASK) | 0xffe,
            MAPPING_EPOCH,
            BusWidth::Dword,
            false,
            false,
            false,
        )
        .is_none(),
        "cross-page accesses must retain the precise slow path"
    );

    map.invalidate_page(linear);
    assert_eq!(
        map.lookup_physical(linear, MAPPING_EPOCH, false, false, false),
        None
    );
}

#[test]
fn access_rejects_a_mapping_from_an_old_bus_epoch() {
    let mut bytes = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();
    let linear = 0x7123_4000;
    let physical = 0x0023_4000;

    assert!(map.populate_read(
        linear,
        physical,
        page(&mut bytes, physical, false),
        PagePermissions::UNPAGED,
        false,
    ));
    assert!(map.has_read_mapping_at_epoch(linear, physical, MAPPING_EPOCH));
    assert!(!map.has_read_mapping_at_epoch(linear, physical, MAPPING_EPOCH + 1));
    assert!(
        map.lookup_access(
            linear,
            MAPPING_EPOCH + 1,
            BusWidth::Byte,
            false,
            false,
            false,
        )
        .is_none()
    );
}

#[test]
fn mode13_is_distinct_and_exposes_native_store_bias() {
    let mut bytes = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();
    let linear = 0x00aa_0123;
    let physical = MODE13_BASE + 0x123;

    assert!(map.populate_write(
        linear,
        physical,
        page(&mut bytes, MODE13_BASE, true),
        PagePermissions::UNPAGED,
        false,
    ));
    let entry = map.entry(linear);
    assert_eq!(entry.kind(), PageKind::Mode13);
    assert_eq!(
        entry.write_ptr(linear),
        Some(bytes.as_mut_ptr().wrapping_add(0x123))
    );

    assert!(map.populate_read(
        linear,
        physical,
        page(&mut bytes, MODE13_BASE, false),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(
        map.entry(linear).read_ptr(linear),
        Some(bytes.as_mut_ptr().wrapping_add(0x123))
    );
}

#[test]
fn vga_invalidation_handles_aliases_and_refills_without_list_growth() {
    const PAGED_ALIAS: u32 = 0x8123_4000;
    let mut ram = Box::new([0u8; PAGE_SIZE]);
    let mut vga = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();
    assert!(map.populate_write(
        0x2000,
        0x2000,
        page(&mut ram, 0x2000, true),
        PagePermissions::UNPAGED,
        false,
    ));

    for _ in 0..3 {
        assert!(map.populate_write(
            MODE13_BASE,
            MODE13_BASE,
            page(&mut vga, MODE13_BASE, true),
            PagePermissions::UNPAGED,
            false,
        ));
        assert!(map.populate_read(
            PAGED_ALIAS,
            MODE13_BASE,
            page(&mut vga, MODE13_BASE, false),
            PagePermissions {
                writable: true,
                user: false,
            },
            false,
        ));
        assert_eq!(map.vga_pages.len(), 2);
        assert_eq!(map.populated_pages.len(), 3);
        assert!(map.has_write_mapping(MODE13_BASE, MODE13_BASE));
        assert!(map.has_read_mapping(PAGED_ALIAS, MODE13_BASE));

        map.invalidate_vga_pages();

        assert!(map.vga_pages.is_empty());
        assert!(map.has_write_mapping(0x2000, 0x2000));
        assert!(!map.has_write_mapping(MODE13_BASE, MODE13_BASE));
        assert!(!map.has_read_mapping(PAGED_ALIAS, MODE13_BASE));
    }
}

#[test]
fn invlpg_is_exact_and_refill_does_not_duplicate_population_list() {
    let mut first = Box::new([0u8; PAGE_SIZE]);
    let mut second = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();

    assert!(map.populate_read(
        0x1000,
        0x3000,
        page(&mut first, 0x3000, false),
        PagePermissions::UNPAGED,
        false,
    ));
    assert!(map.populate_read(
        0x2000,
        0x4000,
        page(&mut second, 0x4000, false),
        PagePermissions::UNPAGED,
        false,
    ));
    map.invalidate_page(0x1fff);
    assert_eq!(map.entry(0x1000).kind(), PageKind::Unavailable);
    assert_eq!(map.entry(0x2000).kind(), PageKind::Ram);

    assert!(map.populate_read(
        0x1000,
        0x3000,
        page(&mut first, 0x3000, false),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(map.populated_pages.len(), 2);
}

#[test]
fn global_invalidation_and_clone_leave_no_live_entries() {
    let mut bytes = Box::new([0u8; PAGE_SIZE]);
    let mut vga = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();
    assert!(map.populate_read(
        0xffff_f000,
        0x7000,
        page(&mut bytes, 0x7000, false),
        PagePermissions::UNPAGED,
        false,
    ));
    assert!(map.populate_write(
        MODE13_BASE,
        MODE13_BASE,
        page(&mut vga, MODE13_BASE, true),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(map.vga_pages.len(), 1);

    let clone = map.clone();
    assert_eq!(clone.entry(0xffff_f000).kind(), PageKind::Unavailable);
    assert!(clone.storage.is_none());

    map.invalidate_all();
    assert_eq!(map.entry(0xffff_f000).kind(), PageKind::Unavailable);
    assert_eq!(map.entry(MODE13_BASE).kind(), PageKind::Unavailable);
    assert!(map.populated_pages.is_empty());
    assert!(map.vga_pages.is_empty());

    assert!(map.populate_write(
        MODE13_BASE,
        MODE13_BASE,
        page(&mut vga, MODE13_BASE, true),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(map.populated_pages.len(), 1);
    assert_eq!(map.vga_pages.len(), 1);
}

// ---------------------------------------------------------------------------------------------
// The one-lookup store-bias derivation battery (design T1-T3,
// `dev_docs/2026-08-07-one-lookup-store-design.md`). The CPU-level halves — watch-edge sweeps
// poisoning through `clear_entry`, and the emitted differential — live in
// `cpu_jit_store_bias_test.rs`.

/// A 4096-aligned page backing. The plain `Box<[u8; PAGE_SIZE]>` helper above is align-1, which
/// is exactly the misaligned-backing case `derive_store_bias` must DEGRADE on — so the fast
/// encodings need this aligned twin, and the degradation test uses the unaligned one on purpose.
#[repr(align(4096))]
struct AlignedPage([u8; PAGE_SIZE]);

fn aligned_page() -> Box<AlignedPage> {
    Box::new(AlignedPage([0u8; PAGE_SIZE]))
}

fn direct(bytes: &mut AlignedPage, physical_page: u32, writable: bool) -> DirectPage {
    DirectPage {
        physical_page,
        ptr: bytes.0.as_mut_ptr(),
        len: bytes.0.len(),
        writable,
        mapping_epoch: MAPPING_EPOCH,
    }
}

#[test]
fn store_bias_fast_encodings_for_ram_mode13_and_supervisor() {
    let mut ram = aligned_page();
    let mut vga = aligned_page();
    let mut map = FastMap::default();

    // Plain user+writable RAM: the bias itself, both tag bits clear.
    assert!(map.populate_write(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        false,
    ));
    let bias = map.store_bias_for_test(0x0040_0000);
    assert_eq!(bias & STORE_BIAS_TAG_MASK, 0, "plain RAM is untagged");
    assert_eq!(
        bias,
        (ram.0.as_mut_ptr() as usize).wrapping_sub(0x0040_0000),
        "the fast entry IS the write bias"
    );

    // The Mode 13h aperture: bias | bit 0.
    assert!(map.populate_write(
        MODE13_BASE,
        MODE13_BASE,
        direct(&mut vga, MODE13_BASE, true),
        PagePermissions::UNPAGED,
        false,
    ));
    let vga_bias = map.store_bias_for_test(MODE13_BASE);
    assert_eq!(vga_bias & STORE_BIAS_TAG_MASK, STORE_BIAS_MODE13);
    assert_ne!(vga_bias, STORE_BIAS_POISON);

    // Supervisor-only RAM (fails ring 3's user+writable test): bias | bit 1.
    assert!(map.populate_write(
        0x0041_0000,
        0x0009_1000,
        direct(&mut ram, 0x0009_1000, true),
        PagePermissions {
            writable: true,
            user: false,
        },
        false,
    ));
    assert_eq!(
        map.store_bias_for_test(0x0041_0000) & STORE_BIAS_TAG_MASK,
        STORE_BIAS_SUPERVISOR
    );
}

#[test]
fn store_bias_poison_causes_each_in_isolation() {
    // No write bias: a read-only fill leaves the poison in place.
    let mut ram = aligned_page();
    let mut map = FastMap::default();
    assert!(map.populate_read(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, false),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(map.store_bias_for_test(0x0040_0000), STORE_BIAS_POISON);

    // Watched page: the derivation reads the same byte the PAGE_WATCHED bit lives in, so
    // "watched implies poisoned" is by construction, not by sweep.
    assert!(map.populate_write(
        0x0041_0000,
        0x0009_1000,
        direct(&mut ram, 0x0009_1000, true),
        PagePermissions::UNPAGED,
        true,
    ));
    assert_eq!(map.store_bias_for_test(0x0041_0000), STORE_BIAS_POISON);

    // Misaligned host backing: degrade to poison, never to a wrong tag (design D7). A
    // deterministically misaligned pointer: 8 bytes into an aligned two-page block.
    #[repr(align(4096))]
    struct TwoPages([u8; 2 * PAGE_SIZE]);
    let mut two = Box::new(TwoPages([0u8; 2 * PAGE_SIZE]));
    assert!(map.populate_write(
        0x0042_0000,
        0x0009_2000,
        DirectPage {
            physical_page: 0x0009_2000,
            ptr: unsafe { two.0.as_mut_ptr().add(8) },
            len: PAGE_SIZE,
            writable: true,
            mapping_epoch: MAPPING_EPOCH,
        },
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(map.store_bias_for_test(0x0042_0000), STORE_BIAS_POISON);
    // The write bias itself must still serve the interpreter regardless of alignment.
    assert!(map.has_write_mapping(0x0042_0000, 0x0009_2000));
}

#[test]
fn store_bias_rides_clear_entry_and_rederives_on_refill() {
    let mut ram = aligned_page();
    let mut map = FastMap::default();
    assert!(map.populate_write(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_ne!(map.store_bias_for_test(0x0040_0000), STORE_BIAS_POISON);

    // Every entry-death path funnels through `clear_entry`; INVLPG is the cheapest to drive.
    map.invalidate_page(0x0040_0000);
    assert_eq!(map.store_bias_for_test(0x0040_0000), STORE_BIAS_POISON);

    // The refill after a watch mark carries the watched bit AND the poison together (H4/INV-P).
    assert!(map.populate_write(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        true,
    ));
    assert!(map.page_watched_bit_for_test(0x0040_0000));
    assert_eq!(map.store_bias_for_test(0x0040_0000), STORE_BIAS_POISON);
}

/// Design T3 (review-closed hazard H2): a same-mapping refill that changes the permission bits
/// must re-derive the store bias, because flags rebuild from fresh permissions on EVERY fill.
#[test]
fn a_same_mapping_refill_with_dropped_permissions_rederives_the_bias() {
    let mut ram = aligned_page();
    let mut map = FastMap::default();
    assert!(map.populate_write(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(
        map.store_bias_for_test(0x0040_0000) & STORE_BIAS_TAG_MASK,
        0
    );

    // Same mapping (same physical, same epoch, same kind), but the fresh walk lost the user
    // bit: the entry must flip to the supervisor tag in the SAME populate, not keep the
    // ring-3-fast encoding.
    assert!(map.populate_read(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions {
            writable: true,
            user: false,
        },
        false,
    ));
    assert_eq!(
        map.store_bias_for_test(0x0040_0000) & STORE_BIAS_TAG_MASK,
        STORE_BIAS_SUPERVISOR,
        "the read refill's fresh permissions must reach the store bias (H2)"
    );
}

// ---------------------------------------------------------------------------------------------
// The one-lookup LOAD-bias derivation battery (load design L1-L3,
// `dev_docs/2026-08-07-one-lookup-load-design.md`). The CPU-level halves — the emitted
// differential, the counter-identity cells, the trio ordering — live in
// `cpu_jit_load_bias_test.rs`.

/// L1's twin-difference pin, the load design's core claim held in ONE test so the two
/// derivations cannot drift apart silently: a WATCHED user page derives a POISONED store bias
/// and a FAST load bias from the identical flags byte — reads have no code-watch dimension.
#[test]
fn a_watched_page_derives_a_poisoned_store_bias_and_a_fast_load_bias() {
    let mut ram = aligned_page();
    let mut map = FastMap::default();
    assert!(map.populate_read(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        true,
    ));
    assert!(map.populate_write(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        true,
    ));
    assert!(map.page_watched_bit_for_test(0x0040_0000));
    assert_eq!(map.store_bias_for_test(0x0040_0000), STORE_BIAS_POISON);
    let load = map.load_bias_for_test(0x0040_0000);
    assert_ne!(
        load, LOAD_BIAS_POISON,
        "watched loads stay FAST (design D1)"
    );
    assert_eq!(load & LOAD_BIAS_TAG_MASK, 0);
    assert_eq!(
        load,
        (ram.0.as_mut_ptr() as usize).wrapping_sub(0x0040_0000),
        "the fast entry IS the read bias"
    );
}

#[test]
fn load_bias_fast_encodings_for_ram_mode13_supervisor_and_read_only() {
    let mut ram = aligned_page();
    let mut vga = aligned_page();
    let mut map = FastMap::default();

    // The Mode 13h aperture: bias | bit 0.
    assert!(map.populate_read(
        MODE13_BASE,
        MODE13_BASE,
        direct(&mut vga, MODE13_BASE, true),
        PagePermissions::UNPAGED,
        false,
    ));
    let vga_bias = map.load_bias_for_test(MODE13_BASE);
    assert_eq!(vga_bias & LOAD_BIAS_TAG_MASK, LOAD_BIAS_MODE13);
    assert_ne!(vga_bias, LOAD_BIAS_POISON);

    // Supervisor RAM (PAGE_USER clear): bias | bit 1. PAGE_WRITABLE is irrelevant to loads —
    // a writable-but-not-user page still tags bit 1, and a user read-only page does NOT.
    assert!(map.populate_read(
        0x0041_0000,
        0x0009_1000,
        direct(&mut ram, 0x0009_1000, true),
        PagePermissions {
            writable: true,
            user: false,
        },
        false,
    ));
    assert_eq!(
        map.load_bias_for_test(0x0041_0000) & LOAD_BIAS_TAG_MASK,
        LOAD_BIAS_SUPERVISOR
    );

    // Review F3's non-vacuity cell: a READ-ONLY-populated aligned page (write bias UNAVAILABLE,
    // the class no store battery ever exercised) derives a FAST load bias while its store bias
    // stays poisoned.
    assert!(map.populate_read(
        0x0042_0000,
        0x0009_2000,
        direct(&mut ram, 0x0009_2000, false),
        PagePermissions {
            writable: false,
            user: true,
        },
        false,
    ));
    assert_eq!(map.load_bias_for_test(0x0042_0000) & LOAD_BIAS_TAG_MASK, 0);
    assert_ne!(map.load_bias_for_test(0x0042_0000), LOAD_BIAS_POISON);
    assert_eq!(map.store_bias_for_test(0x0042_0000), STORE_BIAS_POISON);
}

#[test]
fn load_bias_poison_causes_each_in_isolation() {
    // No read bias: a write-only fill leaves the poison in place (the fill resets read_biases
    // AND drops HAS_READ_BIAS on a fresh mapping, so the derivation poisons twice over).
    let mut ram = aligned_page();
    let mut map = FastMap::default();
    assert!(map.populate_write(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(map.load_bias_for_test(0x0040_0000), LOAD_BIAS_POISON);
    assert_ne!(map.store_bias_for_test(0x0040_0000), STORE_BIAS_POISON);

    // Misaligned host backing: degrade to poison, never to a wrong tag (design D1's alignment
    // term). Same deterministic construction as the store twin's cell.
    #[repr(align(4096))]
    struct TwoPages([u8; 2 * PAGE_SIZE]);
    let mut two = Box::new(TwoPages([0u8; 2 * PAGE_SIZE]));
    assert!(map.populate_read(
        0x0042_0000,
        0x0009_2000,
        DirectPage {
            physical_page: 0x0009_2000,
            ptr: unsafe { two.0.as_mut_ptr().add(8) },
            len: PAGE_SIZE,
            writable: true,
            mapping_epoch: MAPPING_EPOCH,
        },
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(map.load_bias_for_test(0x0042_0000), LOAD_BIAS_POISON);
    // The read bias itself must still serve the interpreter regardless of alignment.
    assert!(map.has_read_mapping(0x0042_0000, 0x0009_2000));
}

#[test]
fn load_bias_rides_clear_entry_and_rederives_on_refill() {
    let mut ram = aligned_page();
    let mut map = FastMap::default();
    assert!(map.populate_read(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_ne!(map.load_bias_for_test(0x0040_0000), LOAD_BIAS_POISON);

    // Every entry-death path funnels through `clear_entry`; INVLPG is the cheapest to drive.
    // For loads a sweep-driven clear is PERF-ONLY over-invalidation (a watched page's loads
    // were legal), healed here by the natural refill — which must re-derive FAST even when the
    // refill carries the watched bit.
    map.invalidate_page(0x0040_0000);
    assert_eq!(map.load_bias_for_test(0x0040_0000), LOAD_BIAS_POISON);

    assert!(map.populate_read(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        true,
    ));
    assert!(map.page_watched_bit_for_test(0x0040_0000));
    assert_ne!(map.load_bias_for_test(0x0040_0000), LOAD_BIAS_POISON);
}

/// L3's derivation half (hazard R1): a same-mapping refill that drops PAGE_USER must flip the
/// load bias to the supervisor tag in the SAME populate — including a WRITE refill, whose
/// derivation reads the KEPT read bias against the fresh flags byte.
#[test]
fn a_same_mapping_refill_that_drops_user_flips_the_load_bias_tag() {
    let mut ram = aligned_page();
    let mut map = FastMap::default();
    assert!(map.populate_read(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions::UNPAGED,
        false,
    ));
    assert_eq!(map.load_bias_for_test(0x0040_0000) & LOAD_BIAS_TAG_MASK, 0);

    // The refill arrives on the WRITE side (same physical, epoch, kind): read_biases is kept
    // by the same_mapping arm, flags rebuild fresh without PAGE_USER, and the load bias must
    // flip rather than keep the ring-3-fast encoding.
    assert!(map.populate_write(
        0x0040_0000,
        0x0009_0000,
        direct(&mut ram, 0x0009_0000, true),
        PagePermissions {
            writable: true,
            user: false,
        },
        false,
    ));
    assert_eq!(
        map.load_bias_for_test(0x0040_0000) & LOAD_BIAS_TAG_MASK,
        LOAD_BIAS_SUPERVISOR,
        "the write refill's fresh permissions must reach the load bias (R1)"
    );
}

/// `classify_reject` must attribute a refusal to the clause `lookup_access` reaches FIRST.
///
/// This is the coupling between the two ladders. They are written adjacently in `fast_map.rs`
/// and cannot share code (`lookup_access` must not widen its hot return value to carry a reason
/// -- that taxes the DISARMED path through the return ABI), so the parity is pinned here
/// instead. Every case below is DOUBLY rejecting on purpose: the single-cause cases would pass
/// under any ordering and prove nothing.
#[test]
fn classify_reject_follows_lookup_access_ladder_order() {
    let mut bytes = Box::new([0u8; PAGE_SIZE]);
    let mut map = FastMap::default();
    // A page whose read bias is live at MAPPING_EPOCH, supervisor-only and non-writable, so
    // permission and epoch clauses can both be provoked at will.
    let base = 0x8123_4000u32;
    assert!(map.populate_read(
        base,
        0x0012_3000,
        page(&mut bytes, 0x0012_3000, false),
        PagePermissions {
            writable: false,
            user: false,
        },
        false,
    ));

    // Misaligned AND epoch-stale. `lookup_access` tests alignment before it ever indexes the
    // epoch array, so the answer must be Misaligned.
    assert_eq!(
        map.classify_reject(
            base + 0x101,
            MAPPING_EPOCH + 1,
            BusWidth::Word,
            false,
            false,
            false
        ),
        SlotReject::Misaligned,
    );
    assert!(
        map.lookup_access(
            base + 0x101,
            MAPPING_EPOCH + 1,
            BusWidth::Word,
            false,
            false,
            false
        )
        .is_none()
    );

    // Page-crossing AND misaligned. The crossing clause is the left operand of `lookup_access`'s
    // `||`, and `||` evaluates left to right, so crossing wins.
    assert_eq!(
        map.classify_reject(
            base + 0xfff,
            MAPPING_EPOCH,
            BusWidth::Word,
            false,
            false,
            false
        ),
        SlotReject::PageCross,
    );

    // Epoch-stale AND permission-refused (user access to a supervisor page). The epoch array is
    // consulted before the flags word.
    assert_eq!(
        map.classify_reject(
            base + 0x100,
            MAPPING_EPOCH + 1,
            BusWidth::Word,
            false,
            true,
            false
        ),
        SlotReject::Epoch,
    );

    // Live and current, but a CPL-3 accessor on a supervisor page: Permission, not Absent.
    assert_eq!(
        map.classify_reject(
            base + 0x100,
            MAPPING_EPOCH,
            BusWidth::Word,
            false,
            true,
            false
        ),
        SlotReject::Permission,
    );

    // No write bias was ever published for this page, so a write is Absent -- and Absent is
    // reached only AFTER the epoch test, which this case also has to pass.
    assert_eq!(
        map.classify_reject(
            base + 0x100,
            MAPPING_EPOCH,
            BusWidth::Word,
            true,
            false,
            false
        ),
        SlotReject::Absent,
    );

    // An entirely unpopulated page is Absent too, from the liveness clause.
    assert_eq!(
        map.classify_reject(
            0x9000_0100,
            MAPPING_EPOCH,
            BusWidth::Word,
            false,
            false,
            false
        ),
        SlotReject::Absent,
    );

    // And the classifier never contradicts the ladder: everything it classifies was refused.
    for (linear, epoch, width, write, user) in [
        (
            base + 0x101,
            MAPPING_EPOCH + 1,
            BusWidth::Word,
            false,
            false,
        ),
        (base + 0xfff, MAPPING_EPOCH, BusWidth::Word, false, false),
        (base + 0x100, MAPPING_EPOCH + 1, BusWidth::Word, false, true),
        (base + 0x100, MAPPING_EPOCH, BusWidth::Word, false, true),
        (base + 0x100, MAPPING_EPOCH, BusWidth::Word, true, false),
        (0x9000_0100, MAPPING_EPOCH, BusWidth::Word, false, false),
    ] {
        assert!(
            map.lookup_access(linear, epoch, width, write, user, false)
                .is_none(),
            "classify_reject was asked about an access lookup_access ADMITS ({linear:#x})"
        );
    }
}
