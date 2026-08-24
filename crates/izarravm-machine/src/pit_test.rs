// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use izarravm_core::{
    CanonicalSectionId, CanonicalSectionRequirement, CanonicalSectionVersion, CanonicalStateView,
    CanonicalStateWriter,
};

// Control words: counter 0, LSB-then-MSB, binary, mode in bits 3..1.
const CW_MODE0: u8 = 0x30;
const CW_MODE1: u8 = 0x32;
const CW_MODE2: u8 = 0x34;
const CW_MODE3: u8 = 0x36;
const CW_MODE4: u8 = 0x38;
const CW_MODE5: u8 = 0x3a;

fn program_ch0(pit: &mut Pit, control: u8, count: u16) {
    pit.write_port(0x43, control);
    pit.write_port(0x40, (count & 0xff) as u8);
    pit.write_port(0x40, (count >> 8) as u8);
}

fn canonical_payload(pit: &Pit) -> Vec<u8> {
    let mut state = CanonicalStateWriter::new().unwrap();
    state
        .section(
            CanonicalSectionId::new(0x0002_0005).unwrap(),
            CanonicalSectionVersion::new(1).unwrap(),
            CanonicalSectionRequirement::Required,
            |out| pit.canonical_projection().write_payload(out),
        )
        .unwrap();
    let bytes = state.finish().unwrap();
    let view = CanonicalStateView::parse(&bytes).unwrap();
    view.sections()[0].payload().to_vec()
}

fn pit_with_counter(channel: usize, counter: Counter) -> Pit {
    let mut counters = std::array::from_fn(|_| Counter::default());
    counters[channel] = counter;
    Pit { counters }
}

fn assert_counter_offsets(
    channel: usize,
    before: Counter,
    after: Counter,
    local_offsets: &[usize],
) {
    let before = canonical_payload(&pit_with_counter(channel, before));
    let after = canonical_payload(&pit_with_counter(channel, after));
    let changed = before
        .iter()
        .zip(&after)
        .enumerate()
        .filter_map(|(index, (left, right))| (left != right).then_some(index))
        .collect::<Vec<_>>();
    let expected = local_offsets
        .iter()
        .map(|offset| channel * 20 + offset)
        .collect::<Vec<_>>();
    assert_eq!(changed, expected, "channel {channel}");
}

fn layout_counter() -> Counter {
    Counter {
        mode: 2,
        rw: RwMode::LsbThenMsb,
        bcd: false,
        count: 0x0000_1234,
        reload: 0x5678,
        out: false,
        gate: true,
        state: CounterState::Counting,
        null_count: false,
        latch: Some(0x9abc),
        status_latch: Some(0xde),
        write_msb_next: false,
        read_msb_next: false,
    }
}

#[test]
fn canonical_pit_payload_layout_is_exact() {
    let pit = Pit {
        counters: [
            Counter {
                mode: 2,
                rw: RwMode::LsbThenMsb,
                bcd: true,
                count: 0x0403_0201,
                reload: 0x0605,
                out: false,
                gate: true,
                state: CounterState::Counting,
                null_count: true,
                latch: Some(0x0807),
                status_latch: Some(0x09),
                write_msb_next: true,
                read_msb_next: false,
            },
            Counter {
                mode: 4,
                rw: RwMode::Lsb,
                bcd: false,
                count: 0x1413_1211,
                reload: 0x1615,
                out: true,
                gate: false,
                state: CounterState::WaitGate,
                null_count: false,
                latch: Some(0x1817),
                status_latch: None,
                write_msb_next: true,
                read_msb_next: true,
            },
            Counter {
                mode: 5,
                rw: RwMode::Msb,
                bcd: true,
                count: 0x2423_2221,
                reload: 0x2625,
                out: false,
                gate: true,
                state: CounterState::LoadDelay,
                null_count: true,
                latch: Some(0x2827),
                status_latch: Some(0x29),
                write_msb_next: true,
                read_msb_next: true,
            },
        ],
    };

    assert_eq!(
        canonical_payload(&pit),
        [
            0x02, 0x02, 0x01, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x00, 0x01, 0x02, 0x01, 0x01,
            0x07, 0x08, 0x01, 0x09, 0x01, 0x00, 0x04, 0x00, 0x00, 0x11, 0x12, 0x00, 0x00, 0x15,
            0x16, 0x01, 0x00, 0x00, 0x00, 0x01, 0x17, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x01,
            0x01, 0x21, 0x22, 0x00, 0x00, 0x25, 0x26, 0x00, 0x01, 0x01, 0x01, 0x01, 0x00, 0x28,
            0x01, 0x29, 0x00, 0x00,
        ]
    );
}

#[test]
fn canonical_pit_tags_are_explicit() {
    assert_eq!(rw_mode_tag(RwMode::Lsb), 0);
    assert_eq!(rw_mode_tag(RwMode::Msb), 1);
    assert_eq!(rw_mode_tag(RwMode::LsbThenMsb), 2);
    assert_eq!(counter_state_tag(CounterState::Inactive), 0);
    assert_eq!(counter_state_tag(CounterState::WaitGate), 0);
    assert_eq!(counter_state_tag(CounterState::LoadDelay), 1);
    assert_eq!(counter_state_tag(CounterState::Counting), 2);

    for mode in 0..=5 {
        let mut counter = layout_counter();
        counter.mode = mode;
        assert_eq!(canonical_payload(&pit_with_counter(0, counter))[0], mode);
    }
}

#[test]
fn canonical_pit_field_offsets_are_exact_for_every_channel() {
    for channel in 0..3 {
        let base = layout_counter();

        let mut changed = base.clone();
        changed.mode = 3;
        assert_counter_offsets(channel, base.clone(), changed, &[0]);

        let mut before = base.clone();
        before.latch = None;
        let mut changed = before.clone();
        changed.rw = RwMode::Lsb;
        assert_counter_offsets(channel, before.clone(), changed, &[1]);
        let mut changed = before.clone();
        changed.rw = RwMode::Msb;
        assert_counter_offsets(channel, before, changed, &[1]);

        let mut changed = base.clone();
        changed.bcd = true;
        assert_counter_offsets(channel, base.clone(), changed, &[2]);

        let mut changed = base.clone();
        changed.count = 0x4433_2211;
        assert_counter_offsets(channel, base.clone(), changed, &[3, 4, 5, 6]);

        let mut changed = base.clone();
        changed.reload = 0x3412;
        assert_counter_offsets(channel, base.clone(), changed, &[7, 8]);

        let mut changed = base.clone();
        changed.out = true;
        assert_counter_offsets(channel, base.clone(), changed, &[9]);

        let mut changed = base.clone();
        changed.gate = false;
        assert_counter_offsets(channel, base.clone(), changed, &[10]);

        let mut state_base = base.clone();
        state_base.count &= 0xffff;
        let mut changed = state_base.clone();
        changed.state = CounterState::LoadDelay;
        assert_counter_offsets(channel, state_base.clone(), changed, &[11]);
        let mut changed = state_base.clone();
        changed.state = CounterState::Inactive;
        assert_counter_offsets(channel, state_base, changed, &[11]);

        let mut changed = base.clone();
        changed.null_count = true;
        assert_counter_offsets(channel, base.clone(), changed, &[12]);

        let mut before = base.clone();
        before.latch = None;
        let mut changed = before.clone();
        changed.latch = Some(0x3412);
        assert_counter_offsets(channel, before, changed, &[13, 14, 15]);
        let mut changed = base.clone();
        changed.latch = Some(0x3412);
        assert_counter_offsets(channel, base.clone(), changed, &[14, 15]);

        let mut before = base.clone();
        before.status_latch = None;
        let mut changed = before.clone();
        changed.status_latch = Some(0x5a);
        assert_counter_offsets(channel, before, changed, &[16, 17]);
        let mut changed = base.clone();
        changed.status_latch = Some(0x5a);
        assert_counter_offsets(channel, base.clone(), changed, &[17]);

        let mut changed = base.clone();
        changed.write_msb_next = true;
        assert_counter_offsets(channel, base.clone(), changed, &[18]);

        let mut changed = base.clone();
        changed.read_msb_next = true;
        assert_counter_offsets(channel, base, changed, &[14, 19]);
    }
}

#[test]
fn canonical_pit_normalizes_unobservable_count_history() {
    let scenarios = [
        (0, false, CounterState::Inactive),
        (1, true, CounterState::WaitGate),
        (2, true, CounterState::LoadDelay),
        (0, true, CounterState::Counting),
        (1, true, CounterState::Counting),
        (4, false, CounterState::Counting),
        (5, false, CounterState::Counting),
    ];
    for (mode, out, state) in scenarios {
        let counter = Counter {
            mode,
            rw: RwMode::Lsb,
            count: 0x0000_3456,
            reload: 0x1234,
            out,
            gate: true,
            state,
            ..Counter::default()
        };
        let mut low = pit_with_counter(0, counter.clone());
        let mut high_counter = counter;
        high_counter.count = 0xabcd_3456;
        let mut high = pit_with_counter(0, high_counter);

        assert_eq!(canonical_payload(&low), canonical_payload(&high));
        assert_eq!(low.read_port(0x40), high.read_port(0x40));
        low.write_port(0x43, 0x00);
        high.write_port(0x43, 0x00);
        assert_eq!(low.read_port(0x40), high.read_port(0x40));
        assert_eq!(low.tick(3), high.tick(3));
        assert_eq!(canonical_payload(&low), canonical_payload(&high));
        low.set_gate(0, false);
        high.set_gate(0, false);
        low.set_gate(0, true);
        high.set_gate(0, true);
        assert_eq!(canonical_payload(&low), canonical_payload(&high));
        program_ch0(&mut low, CW_MODE2, 7);
        program_ch0(&mut high, CW_MODE2, 7);
        assert_eq!(low.tick(20), high.tick(20));
        assert_eq!(canonical_payload(&low), canonical_payload(&high));
    }
}

#[test]
fn canonical_pit_collapses_inactive_and_wait_gate_continuation() {
    for control in [CW_MODE1, CW_MODE5] {
        let mut inactive = Pit::default();
        inactive.write_port(0x43, control);
        let mut waiting = inactive.clone();
        waiting.write_port(0x40, 0);
        waiting.write_port(0x40, 0);

        assert_eq!(inactive.counters[0].state, CounterState::Inactive);
        assert_eq!(waiting.counters[0].state, CounterState::WaitGate);
        assert_eq!(canonical_payload(&inactive), canonical_payload(&waiting));
        assert_eq!(inactive.tick(10), waiting.tick(10));
        inactive.write_port(0x43, 0x00);
        waiting.write_port(0x43, 0x00);
        assert_eq!(inactive.read_port(0x40), waiting.read_port(0x40));
        inactive.write_port(0x40, 0x34);
        waiting.write_port(0x40, 0x34);
        assert_eq!(canonical_payload(&inactive), canonical_payload(&waiting));
        inactive.write_port(0x40, 0x12);
        waiting.write_port(0x40, 0x12);
        inactive.set_gate(0, false);
        waiting.set_gate(0, false);
        inactive.set_gate(0, true);
        waiting.set_gate(0, true);
        assert_eq!(inactive.tick(0x1235), waiting.tick(0x1235));
        assert_eq!(canonical_payload(&inactive), canonical_payload(&waiting));
    }
}

#[test]
fn canonical_pit_normalizes_unread_latch_bytes_and_inactive_phases() {
    let cases = [
        (RwMode::Lsb, false, 0x12aa, 0x34aa),
        (RwMode::Msb, false, 0xbb12, 0xbb34),
        (RwMode::LsbThenMsb, true, 0xcc12, 0xcc34),
    ];
    for (rw, read_msb_next, left_latch, right_latch) in cases {
        let left_counter = Counter {
            rw,
            latch: Some(left_latch),
            read_msb_next,
            ..Counter::default()
        };
        let mut right_counter = left_counter.clone();
        right_counter.latch = Some(right_latch);
        let mut left = pit_with_counter(0, left_counter);
        let mut right = pit_with_counter(0, right_counter);
        assert_eq!(canonical_payload(&left), canonical_payload(&right));
        assert_eq!(left.read_port(0x40), right.read_port(0x40));
        assert_eq!(canonical_payload(&left), canonical_payload(&right));
    }

    for rw in [RwMode::Lsb, RwMode::Msb] {
        let left_counter = Counter {
            rw,
            write_msb_next: false,
            read_msb_next: false,
            ..Counter::default()
        };
        let mut right_counter = left_counter.clone();
        right_counter.write_msb_next = true;
        right_counter.read_msb_next = true;
        let mut left = pit_with_counter(0, left_counter);
        let mut right = pit_with_counter(0, right_counter);
        assert_eq!(canonical_payload(&left), canonical_payload(&right));
        assert_eq!(left.read_port(0x40), right.read_port(0x40));
        left.write_port(0x40, 0x5a);
        right.write_port(0x40, 0x5a);
        assert_eq!(canonical_payload(&left), canonical_payload(&right));
    }
}

#[test]
fn canonical_pit_preserves_full_range_counting_element() {
    for mode in 0..=5 {
        for bcd in [false, true] {
            let mut pit = Pit::default();
            program_ch0(&mut pit, 0x30 | (mode << 1) | u8::from(bcd), 0);
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, false);
                pit.set_gate(0, true);
            } else {
                pit.tick(1);
            }
            assert_eq!(pit.counters[0].state, CounterState::Counting);
            assert_eq!(pit.counters[0].count, 0x1_0000);
            assert_eq!(
                &canonical_payload(&pit)[3..7],
                &[0x00, 0x00, 0x01, 0x00],
                "mode {mode}, bcd {bcd}"
            );

            let deadline = pit.clocks_until_channel0_irq();
            let mut captured = pit.clone();
            let before = canonical_payload(&captured);
            assert_eq!(before, canonical_payload(&captured));
            assert_eq!(pit.tick(1), captured.tick(1));
            assert_eq!(
                pit.clocks_until_channel0_irq(),
                captured.clocks_until_channel0_irq()
            );
            assert_eq!(canonical_payload(&pit), canonical_payload(&captured));
            assert!(deadline.is_some(), "mode {mode}, bcd {bcd}");
        }
    }
}

#[test]
fn canonical_pit_capture_preserves_half_writes_in_one_shot_and_periodic_modes() {
    let mut mode0 = Pit::default();
    let mut mode0_twin = Pit::default();
    for pit in [&mut mode0, &mut mode0_twin] {
        pit.write_port(0x43, CW_MODE0);
        pit.write_port(0x40, 0x34);
    }
    let mode0_mid = canonical_payload(&mode0);
    assert_eq!(mode0_mid, canonical_payload(&mode0));
    assert_eq!(mode0_mid[18], 1);
    assert_eq!(&mode0_mid[7..9], &[0x34, 0x00]);
    assert_eq!(mode0_mid[9], 0);
    assert_eq!(mode0_mid[11], 0);
    mode0.write_port(0x40, 0x12);
    mode0_twin.write_port(0x40, 0x12);
    assert_eq!(mode0.tick(0x1235), mode0_twin.tick(0x1235));
    assert_eq!(canonical_payload(&mode0), canonical_payload(&mode0_twin));

    let mut periodic = Pit::default();
    program_ch0(&mut periodic, CW_MODE2, 7);
    periodic.tick(1);
    let mut periodic_twin = periodic.clone();
    periodic.write_port(0x40, 0x0b);
    periodic_twin.write_port(0x40, 0x0b);
    let periodic_mid = canonical_payload(&periodic);
    assert_eq!(periodic_mid, canonical_payload(&periodic));
    assert_eq!(periodic_mid[18], 1);
    assert_eq!(periodic_mid[11], 2);
    periodic.write_port(0x40, 0x00);
    periodic_twin.write_port(0x40, 0x00);
    assert_eq!(periodic.tick(50), periodic_twin.tick(50));
    assert_eq!(
        canonical_payload(&periodic),
        canonical_payload(&periodic_twin)
    );
}

#[test]
fn canonical_pit_capture_preserves_an_unlatched_live_half_read() {
    let mut captured = Pit::default();
    program_ch0(&mut captured, CW_MODE0, 0x0101);
    captured.tick(1);
    let mut twin = captured.clone();

    assert_eq!(captured.read_port(0x40), Some(0x01));
    assert_eq!(twin.read_port(0x40), Some(0x01));
    let half_read = canonical_payload(&captured);
    assert_eq!(half_read, canonical_payload(&captured));
    assert_eq!(half_read[13], 0);
    assert_eq!(half_read[19], 1);

    captured.tick(2);
    twin.tick(2);
    assert_eq!(captured.read_port(0x40), Some(0x00));
    assert_eq!(twin.read_port(0x40), Some(0x00));
    assert_eq!(canonical_payload(&captured), canonical_payload(&twin));
}

#[test]
fn canonical_pit_matches_split_continuation_across_modes_radices_and_gates() {
    for mode in 0..=5 {
        for bcd in [false, true] {
            let control = 0x30 | (mode << 1) | u8::from(bcd);
            let count = if bcd { 0x0050 } else { 50 };
            let mut whole = Pit::default();
            program_ch0(&mut whole, control, count);
            if matches!(mode, 1 | 5) {
                whole.set_gate(0, false);
                whole.set_gate(0, true);
            }
            let mut split = whole.clone();

            assert_eq!(canonical_payload(&whole), canonical_payload(&split));
            whole.tick(1);
            split.tick(1);
            whole.set_gate(0, false);
            split.set_gate(0, false);
            let whole_paused_edges = whole.tick(7);
            let split_paused_edges = split.tick(2) + split.tick(5);
            assert_eq!(whole_paused_edges, split_paused_edges);
            whole.set_gate(0, true);
            split.set_gate(0, true);
            let whole_edges = whole.tick(130);
            let split_edges = split.tick(1) + split.tick(17) + split.tick(41) + split.tick(71);

            assert_eq!(whole_edges, split_edges, "mode {mode}, bcd {bcd}");
            assert_eq!(
                canonical_payload(&whole),
                canonical_payload(&split),
                "mode {mode}, bcd {bcd}"
            );
        }
    }
}

#[test]
fn mode3_default_count_is_18_2_hz() {
    // Count 0 means 65536. After the load clock, channel 0 raises IRQ0 every
    // 65536 input clocks: 1_193_182 / 65536 = 18.2065 Hz, the PC timer rate.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE3, 0);
    pit.tick(1); // consume the load delay
    assert_eq!(pit.clocks_until_channel0_irq(), Some(65536));
}

#[test]
fn mode3_square_wave_period_and_one_edge() {
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE3, 4);
    pit.tick(1);
    assert_eq!(pit.clocks_until_channel0_irq(), Some(4));
    assert_eq!(pit.tick(4), 1); // exactly one rising edge per period
    assert_eq!(pit.tick(4), 1); // periodic
}

#[test]
fn mode2_rate_generator_period() {
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE2, 4);
    pit.tick(1);
    assert_eq!(pit.clocks_until_channel0_irq(), Some(4));
    assert_eq!(pit.tick(4), 1);
    assert_eq!(pit.tick(4), 1);
}

#[test]
fn mode0_one_shot_fires_once() {
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE0, 4);
    pit.tick(1);
    assert_eq!(pit.clocks_until_channel0_irq(), Some(4)); // OUT rises at terminal
    assert_eq!(pit.tick(4), 1);
    assert_eq!(pit.tick(1000), 0); // no repeat
    assert_eq!(pit.clocks_until_channel0_irq(), None);
}

#[test]
fn mode4_software_strobe_fires_once() {
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE4, 4);
    pit.tick(1);
    // OUT high, strobes low at terminal then high one clock later (N+1).
    assert_eq!(pit.clocks_until_channel0_irq(), Some(5));
    assert_eq!(pit.tick(5), 1);
    assert_eq!(pit.tick(1000), 0);
}

#[test]
fn modes_1_and_5_need_a_gate_trigger() {
    for cw in [CW_MODE1, CW_MODE5] {
        let mut pit = Pit::default();
        program_ch0(&mut pit, cw, 4);
        pit.tick(1);
        // GATE is high but never had a rising edge: no count, no IRQ.
        assert_eq!(pit.clocks_until_channel0_irq(), None);
        // A falling then rising GATE edge triggers the one-shot.
        pit.set_gate(0, false);
        pit.set_gate(0, true);
        assert!(pit.clocks_until_channel0_irq().is_some());
        assert_eq!(pit.tick(6), 1); // one strobe/edge then done
        assert_eq!(pit.tick(1000), 0);
    }
}

/// Brute-force oracle for the analytic clocks_until_out_rise: clone and
/// step until the next OUT rising edge. Scans slightly past the longest
/// real distance (mode 4 in LoadDelay with the full-range reload: the load
/// CLK + 65536 counts + the strobe-return CLK = 65538), so unlike the
/// production clocks_until_channel0_irq (whose 65537 cap conservatively
/// declines that one corner) the oracle sees every edge the counter fires.
fn simulated_rise(counter: &Counter) -> Option<u64> {
    let mut probe = counter.clone();
    (1..=65539u64).find(|&_clocks| probe.step())
}

#[test]
fn analytic_out_rise_matches_the_step_simulation_across_modes_and_phases() {
    // Every mode x a spread of reloads x every phase across two-plus
    // periods (capped for the full-range reloads to keep the oracle
    // affordable), walked through the real tick path so both OUT phases
    // and the post-reload states are visited.
    for mode in 0..=5u8 {
        for reload in [2u16, 3, 4, 5, 7, 18, 100, 101, 255, 0] {
            let mut pit = Pit::default();
            pit.write_port(0x43, 0x30 | (mode << 1));
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, false); // arm the trigger edge below
            }
            pit.write_port(0x40, (reload & 0xff) as u8);
            pit.write_port(0x40, (reload >> 8) as u8);
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, true); // rising edge starts the one-shot
            }
            let phases = if reload == 0 {
                8
            } else {
                (2 * u64::from(reload) + 6).min(300)
            };
            for phase in 0..phases {
                assert_eq!(
                    pit.counters[0].clocks_until_out_rise(),
                    simulated_rise(&pit.counters[0]),
                    "mode {mode} reload {reload} phase {phase}"
                );
                pit.tick(1);
            }
        }
    }
}

#[test]
fn analytic_out_rise_is_none_while_gate_pauses_or_arms() {
    // GATE low pauses counting (all modes in this model): no rise without
    // guest input, so the analytic reports None, agreeing with the oracle.
    for mode in [0u8, 2, 3, 4] {
        let mut pit = Pit::default();
        pit.write_port(0x43, 0x30 | (mode << 1));
        pit.write_port(0x40, 50);
        pit.write_port(0x40, 0);
        pit.tick(5);
        pit.set_gate(0, false);
        assert_eq!(
            pit.counters[0].clocks_until_out_rise(),
            None,
            "mode {mode} paused"
        );
        assert_eq!(simulated_rise(&pit.counters[0]), None, "mode {mode} oracle");
    }
    // Modes 1/5 armed but never triggered: None until the GATE rising edge.
    for mode in [1u8, 5] {
        let mut pit = Pit::default();
        pit.write_port(0x43, 0x30 | (mode << 1));
        pit.write_port(0x40, 50);
        pit.write_port(0x40, 0);
        pit.tick(1);
        assert_eq!(
            pit.counters[0].clocks_until_out_rise(),
            None,
            "mode {mode} awaiting trigger"
        );
    }
}

#[test]
fn analytic_out_rise_declines_bcd_counters() {
    // BCD is declined by design (a conservative None relaxes the batch cap;
    // the edge itself still fires through the tick path).
    let mut pit = Pit::default();
    pit.write_port(0x43, CW_MODE2 | 1); // ch0 mode 2, BCD
    pit.write_port(0x40, 0x50);
    pit.write_port(0x40, 0x00);
    pit.tick(1);
    assert_eq!(pit.counters[0].clocks_until_out_rise(), None);
    assert!(simulated_rise(&pit.counters[0]).is_some());
}

#[test]
fn analytic_out_rise_agrees_with_the_channel0_hlt_oracle() {
    // Channel-0 states must agree with clocks_until_channel0_irq (the HLT
    // fast-forward's clone-and-step estimator), pinning the two together.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE2, 100);
    for _ in 0..250 {
        assert_eq!(
            pit.counters[0].clocks_until_out_rise(),
            pit.clocks_until_channel0_irq()
        );
        pit.tick(1);
    }
}

/// Brute-force oracle for the analytic out_after: clone and step `clocks`
/// times, returning the OUT level afterward. Unbounded in `clocks` (unlike
/// `simulated_rise`, which only needs to find the first edge) since the
/// differential test below queries multi-period distances directly.
fn simulated_out_after(counter: &Counter, clocks: u64) -> bool {
    let mut probe = counter.clone();
    for _ in 0..clocks {
        probe.step();
    }
    probe.out
}

#[test]
fn analytic_out_after_matches_the_step_simulation_across_modes_and_phases() {
    // Cover every mode across a spread of reloads (even, odd, minimum
    // legal, illegal, full-range) x every phase across two-plus periods x a
    // sweep of queried distances (0, 1, boundary-adjacent, a large
    // multi-period jump), matching out_after against clone-and-step.
    for mode in 0..=5u8 {
        for reload in [2u16, 3, 4, 5, 6, 7, 18, 100, 101, 255, 1, 0] {
            let mut pit = Pit::default();
            pit.write_port(0x43, 0x30 | (mode << 1));
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, false); // arm the trigger edge below
            }
            pit.write_port(0x40, (reload & 0xff) as u8);
            pit.write_port(0x40, (reload >> 8) as u8);
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, true); // rising edge starts the one-shot
            }
            let phases = if reload == 0 {
                6
            } else {
                (2 * u64::from(reload) + 6).min(120)
            };
            let period = if reload == 0 {
                65536
            } else {
                u64::from(reload)
            };
            let sweeps: Vec<u64> = [
                0,
                1,
                2,
                period.saturating_sub(1),
                period,
                period + 1,
                2 * period,
                2 * period + 1,
                5 * period + 3,
            ]
            .into_iter()
            .collect();
            for phase in 0..phases {
                for &clocks in &sweeps {
                    assert_eq!(
                        pit.counters[0].out_after(clocks),
                        Some(simulated_out_after(&pit.counters[0], clocks)),
                        "mode {mode} reload {reload} phase {phase} clocks {clocks}"
                    );
                }
                pit.tick(1);
            }
        }
    }
}

#[test]
fn analytic_out_after_zero_clocks_is_the_current_level() {
    // clocks == 0 must be a pure readback, in every state: Inactive,
    // WaitGate, LoadDelay, and Counting.
    let mut pit = Pit::default(); // fresh counter 0 is Inactive
    assert_eq!(pit.counters[0].out_after(0), Some(pit.counters[0].out));

    pit.write_port(0x43, CW_MODE1); // arms WaitGate
    assert_eq!(pit.counters[0].out_after(0), Some(pit.counters[0].out));

    program_ch0(&mut pit, CW_MODE3, 4); // LoadDelay immediately after the count write
    assert_eq!(pit.counters[0].out_after(0), Some(pit.counters[0].out));

    pit.tick(1); // now Counting
    assert_eq!(pit.counters[0].out_after(0), Some(pit.counters[0].out));
}

#[test]
fn analytic_out_after_holds_while_gate_is_low() {
    // GATE low pauses counting in every mode this model runs (0, 2, 3, 4);
    // out_after must hold the current level for any queried distance, since
    // only a port write (which already ends the batch) can raise GATE again.
    for mode in [0u8, 2, 3, 4] {
        let mut pit = Pit::default();
        pit.write_port(0x43, 0x30 | (mode << 1));
        pit.write_port(0x40, 50);
        pit.write_port(0x40, 0);
        pit.tick(5);
        pit.set_gate(0, false);
        let level = pit.counters[0].out;
        for clocks in [0u64, 1, 100, 100_000] {
            assert_eq!(
                pit.counters[0].out_after(clocks),
                Some(level),
                "mode {mode} clocks {clocks}"
            );
        }
    }
}

#[test]
fn analytic_out_after_matches_after_a_gate_retrigger_in_modes_1_and_5() {
    // Modes 1/5 are retriggerable one-shots: a GATE rising edge mid-batch
    // starts a fresh pulse from a Counting (not LoadDelay) state. out_after
    // must track the post-retrigger phase, not the pre-trigger one.
    for (cw, mode) in [(CW_MODE1, 1u8), (CW_MODE5, 5u8)] {
        let mut pit = Pit::default();
        program_ch0(&mut pit, cw, 6);
        pit.set_gate(0, false);
        pit.set_gate(0, true); // first trigger
        pit.tick(3); // partway through the first pulse
        pit.set_gate(0, false);
        pit.set_gate(0, true); // retrigger: count reloads to 6, Counting again
        for clocks in [0u64, 1, 2, 5, 6, 7, 12, 13] {
            assert_eq!(
                pit.counters[0].out_after(clocks),
                Some(simulated_out_after(&pit.counters[0], clocks)),
                "mode {mode} clocks {clocks}"
            );
        }
    }
}

#[test]
fn analytic_out_after_holds_in_wait_gate() {
    // Modes 1/5 armed but never triggered: WaitGate holds OUT at its current
    // level for any queried distance, agreeing with the oracle (no CLK moves
    // an untriggered one-shot).
    for cw in [CW_MODE1, CW_MODE5] {
        let mut pit = Pit::default();
        program_ch0(&mut pit, cw, 6);
        pit.tick(1); // WaitGate: GATE never rose
        let level = pit.counters[0].out;
        for clocks in [0u64, 1, 100, 100_000] {
            assert_eq!(pit.counters[0].out_after(clocks), Some(level));
            assert_eq!(simulated_out_after(&pit.counters[0], clocks), level);
        }
    }
}

#[test]
fn analytic_out_after_declines_bcd_counters() {
    // BCD is declined by design, same precedent as clocks_until_out_rise: a
    // conservative None sends the caller back to the non-lazy path.
    let mut pit = Pit::default();
    pit.write_port(0x43, CW_MODE2 | 1); // ch0 mode 2, BCD
    pit.write_port(0x40, 0x50);
    pit.write_port(0x40, 0x00);
    pit.tick(1);
    assert_eq!(pit.counters[0].out_after(10), None);
}

#[test]
fn analytic_out_after_matches_on_channel1_and_channel2_defaults() {
    // Cover the production call sites: channel 1 (DRAM refresh, mode 2) and
    // channel 2 (speaker, mode 3), the two channels port 0x61 bits 4/5 read.
    let mut pit = Pit::default(); // channel 1 pre-seeded mode 2 count 18
    for clocks in [0u64, 1, 5, 17, 18, 19, 36, 37, 90, 91] {
        assert_eq!(
            pit.counters[1].out_after(clocks),
            Some(simulated_out_after(&pit.counters[1], clocks)),
            "channel 1 clocks {clocks}"
        );
    }

    pit.write_port(0x43, 0xB6); // counter 2, LSB+MSB, mode 3, binary
    pit.set_gate(2, true);
    pit.write_port(0x42, 0x18); // 0x1518, an odd reload like PoP's speaker driver
    pit.write_port(0x42, 0x15);
    pit.tick(1);
    for clocks in [0u64, 1, 2, 100, 0x1518 / 2, 0x1518, 0x1518 * 2 + 7] {
        assert_eq!(
            pit.counters[2].out_after(clocks),
            Some(simulated_out_after(&pit.counters[2], clocks)),
            "channel 2 clocks {clocks}"
        );
    }
}

/// Brute-force oracle for the analytic count_after: clone and step `clocks` times,
/// returning the counting element as a read would expose it.
fn simulated_count_after(counter: &Counter, clocks: u64) -> u16 {
    let mut probe = counter.clone();
    for _ in 0..clocks {
        probe.step();
    }
    probe.masked_count()
}

#[test]
fn analytic_count_after_matches_the_step_simulation_across_modes_and_phases() {
    // Same coverage shape as the out_after differential above -- every mode, a
    // spread of reloads (even, odd, minimum legal, illegal, full-range), every
    // phase across two-plus periods, and distances from 0 through several periods
    // -- but pinning the COUNTER VALUE a mid-batch read reports, which is what the
    // 0x40/0x42 peek returns.
    for mode in 0..=5u8 {
        for reload in [2u16, 3, 4, 5, 6, 7, 18, 100, 101, 255, 1, 0] {
            let mut pit = Pit::default();
            pit.write_port(0x43, 0x30 | (mode << 1));
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, false); // arm the trigger edge below
            }
            pit.write_port(0x40, (reload & 0xff) as u8);
            pit.write_port(0x40, (reload >> 8) as u8);
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, true); // rising edge starts the one-shot
            }
            let phases = if reload == 0 {
                6
            } else {
                (2 * u64::from(reload) + 6).min(120)
            };
            let period = if reload == 0 {
                65536
            } else {
                u64::from(reload)
            };
            let sweeps: Vec<u64> = [
                0,
                1,
                2,
                period.saturating_sub(1),
                period,
                period + 1,
                2 * period,
                2 * period + 1,
                5 * period + 3,
            ]
            .into_iter()
            .collect();
            for phase in 0..phases {
                for &clocks in &sweeps {
                    assert_eq!(
                        pit.counters[0].count_after(clocks),
                        Some(simulated_count_after(&pit.counters[0], clocks)),
                        "mode {mode} reload {reload} phase {phase} clocks {clocks}"
                    );
                }
                pit.tick(1);
            }
        }
    }
}

#[test]
fn analytic_count_after_matches_the_step_simulation_with_a_low_gate() {
    // The sweep above always runs with GATE high (channel 0's gate is wired
    // high), so it never enters LoadDelay-with-GATE-low -- the one state where
    // `step` still LOADS the counting element (the load is unconditional; only
    // `step_counting` is gated) while the counter cannot count. Guest-reachable
    // on channel 2: clear 0x61 bit 0, program the counter, then latch/read 0x42.
    // Both peeks are pinned here, `count_after` because it must report the
    // RELOAD and `out_after` because it must report the stored level (OUT does
    // not move on the load).
    for mode in 0..=5u8 {
        for reload in [2u16, 3, 18, 100, 101, 255, 1, 0] {
            let mut pit = Pit::default();
            pit.set_gate(0, false);
            pit.write_port(0x43, 0x30 | (mode << 1));
            pit.write_port(0x40, (reload & 0xff) as u8);
            pit.write_port(0x40, (reload >> 8) as u8);
            for phase in 0..6u64 {
                for clocks in [0u64, 1, 2, 3, 17, 1000, 70000] {
                    assert_eq!(
                        pit.counters[0].count_after(clocks),
                        Some(simulated_count_after(&pit.counters[0], clocks)),
                        "count mode {mode} reload {reload} phase {phase} clocks {clocks}"
                    );
                    assert_eq!(
                        pit.counters[0].out_after(clocks),
                        Some(simulated_out_after(&pit.counters[0], clocks)),
                        "out mode {mode} reload {reload} phase {phase} clocks {clocks}"
                    );
                }
                pit.tick(1);
            }
        }
    }
}

#[test]
fn analytic_count_after_zero_clocks_is_the_current_counting_element() {
    // clocks == 0 must be a pure readback in every state, so a peek at the very
    // start of a batch is byte-identical to the pre-peek behavior.
    let mut pit = Pit::default(); // fresh counter 0 is Inactive
    assert_eq!(
        pit.counters[0].count_after(0),
        Some(pit.counters[0].masked_count())
    );
    pit.write_port(0x43, CW_MODE1); // arms WaitGate
    assert_eq!(
        pit.counters[0].count_after(0),
        Some(pit.counters[0].masked_count())
    );
    program_ch0(&mut pit, CW_MODE2, 100); // LoadDelay
    assert_eq!(
        pit.counters[0].count_after(0),
        Some(pit.counters[0].masked_count())
    );
    pit.tick(3); // Counting
    assert_eq!(
        pit.counters[0].count_after(0),
        Some(pit.counters[0].masked_count())
    );
}

#[test]
fn analytic_count_after_declines_for_a_bcd_counter() {
    // BCD declines exactly like out_after, so the caller keeps today's live-field
    // behavior rather than a wrong decimal peek.
    let mut pit = Pit::default();
    pit.write_port(0x43, CW_MODE2 | 1); // ch0 mode 2, BCD
    pit.write_port(0x40, 0x50);
    pit.write_port(0x40, 0x00);
    pit.tick(5);
    assert_eq!(pit.counters[0].count_after(7), None);
}

#[test]
fn analytic_count_after_holds_while_gate_is_low() {
    // A low GATE pauses counting in every mode that honors it, so the peek must
    // report the CE unchanged for any distance.
    for mode in [0u8, 2, 3, 4] {
        let mut pit = Pit::default();
        pit.write_port(0x43, 0x30 | (mode << 1));
        pit.write_port(0x40, 50);
        pit.write_port(0x40, 0);
        pit.tick(4);
        pit.set_gate(0, false);
        let held = pit.counters[0].masked_count();
        for clocks in [0u64, 1, 7, 1000] {
            assert_eq!(
                pit.counters[0].count_after(clocks),
                Some(held),
                "mode {mode} clocks {clocks}"
            );
        }
    }
}

/// Brute-force oracle for the mid-batch status byte: clone, step `clocks` times,
/// then latch at zero offset. The same shape as `simulated_count_after`.
fn simulated_status_after(counter: &Counter, clocks: u64) -> u8 {
    let mut probe = counter.clone();
    for _ in 0..clocks {
        probe.step();
    }
    probe.latch_status(0);
    probe.status_latch.unwrap()
}

/// The analytic answer: latch at an in-batch offset of `clocks`.
fn analytic_status_after(counter: &Counter, clocks: u64) -> u8 {
    let mut probe = counter.clone();
    probe.latch_status(clocks);
    probe.status_latch.unwrap()
}

#[test]
fn mid_batch_status_byte_matches_the_step_simulation_across_load_delay_phases() {
    // The status-byte counterpart of the count_after and out_after
    // differentials, and the test that retires the "status reports the
    // batch-start null count" residual. The WHOLE byte is compared, so it pins
    // bit 7 (OUT, already peeked) and bit 6 (NULL COUNT, the residual) together
    // and would also catch a peek leaking into the three register-state fields
    // that no CLK may move.
    //
    // The sweep deliberately STARTS in LoadDelay -- the state entered by the
    // count write and left on the very first CLK -- because that is the only
    // phase where bit 6 moves at all. `clocks == 0` must still report the bit
    // SET (nothing has clocked yet) and `clocks >= 1` must report it CLEAR;
    // both edges are inside the sweep.
    for mode in 0..=5u8 {
        for reload in [2u16, 3, 18, 100, 255, 1, 0] {
            for gate in [true, false] {
                let mut pit = Pit::default();
                pit.write_port(0x43, 0x30 | (mode << 1));
                pit.set_gate(0, gate);
                pit.write_port(0x40, (reload & 0xff) as u8);
                pit.write_port(0x40, (reload >> 8) as u8);
                // Phase 0 is LoadDelay (or WaitGate for the gate-low modes 1/5);
                // later phases walk into Counting so the test also pins that the
                // peek leaves an already-loaded counter's bit 6 alone.
                for phase in 0..8u64 {
                    for clocks in [0u64, 1, 2, 3, 17, 260, 5_000] {
                        assert_eq!(
                            analytic_status_after(&pit.counters[0], clocks),
                            simulated_status_after(&pit.counters[0], clocks),
                            "mode {mode} reload {reload} gate {gate} phase {phase} clocks {clocks}"
                        );
                    }
                    pit.tick(1);
                }
            }
        }
    }
}

#[test]
fn the_null_count_bit_is_set_at_zero_offset_and_clear_one_clock_later() {
    // Non-vacuity for the sweep above: without the peek, bit 6 is the live
    // field and BOTH reads below return it set, so the differential could pass
    // on a counter that never leaves LoadDelay within the sweep. This pins the
    // single transition the peek exists to model, in the state a guest reaches
    // by writing a count and immediately issuing a read-back.
    let mut pit = Pit::default();
    pit.write_port(0x43, CW_MODE2);
    pit.write_port(0x40, 100);
    pit.write_port(0x40, 0);
    assert_eq!(
        pit.counters[0].state,
        CounterState::LoadDelay,
        "sanity: a completed count write must leave the counter in LoadDelay"
    );
    assert_eq!(
        analytic_status_after(&pit.counters[0], 0) & 0x40,
        0x40,
        "at zero in-batch offset nothing has clocked, so NULL COUNT must be set"
    );
    assert_eq!(
        analytic_status_after(&pit.counters[0], 1) & 0x40,
        0,
        "one CLK loads the counting element, so NULL COUNT must read clear"
    );
}

#[test]
fn the_null_count_bit_holds_through_wait_gate_however_many_clocks_pass() {
    // Modes 1 and 5 park in WaitGate until a GATE rising edge, and `step` never
    // loads there -- so unlike LoadDelay the bit must NOT clear with distance.
    // A peek that keyed on "a count was written" rather than on the state
    // machine would clear it here and this is the case that catches that.
    for mode in [CW_MODE1, CW_MODE5] {
        let mut pit = Pit::default();
        pit.write_port(0x43, mode);
        pit.set_gate(0, false);
        pit.write_port(0x40, 100);
        pit.write_port(0x40, 0);
        assert_eq!(pit.counters[0].state, CounterState::WaitGate);
        for clocks in [0u64, 1, 2, 5_000] {
            assert_eq!(
                analytic_status_after(&pit.counters[0], clocks) & 0x40,
                0x40,
                "mode {mode:#04x} clocks {clocks}: WaitGate must hold NULL COUNT set"
            );
        }
    }
}

#[test]
fn counter_latch_freezes_the_read() {
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE3, 100);
    pit.tick(1); // load: count = 100
    pit.tick(4); // count decremented (mode 3 steps by two): 100 -> 92
    pit.write_port(0x43, 0x00); // counter-latch command, counter 0
    pit.tick(10); // keeps counting, but the latch is frozen
    let lo = pit.read_port(0x40).unwrap();
    let hi = pit.read_port(0x40).unwrap();
    assert_eq!(u16::from_le_bytes([lo, hi]), 92);
}

#[test]
fn read_back_status_reports_mode_and_out() {
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE3, 4);
    pit.tick(1);
    pit.write_port(0x43, 0xe2); // read-back: latch status, counter 0
    let status = pit.read_port(0x40).unwrap();
    assert_eq!(status & 0x80, 0x80); // OUT high in mode 3 after load
    assert_eq!((status >> 1) & 0x07, 3); // mode 3
    assert_eq!((status >> 4) & 0x03, 3); // RW = LSB then MSB
}

#[test]
fn lsb_then_msb_write_and_read() {
    let mut pit = Pit::default();
    pit.write_port(0x43, CW_MODE3);
    pit.write_port(0x40, 0x34); // LSB
    pit.write_port(0x40, 0x12); // MSB -> count 0x1234
    pit.tick(1); // load
    pit.write_port(0x43, 0x00); // latch
    let lo = pit.read_port(0x40).unwrap();
    let hi = pit.read_port(0x40).unwrap();
    assert_eq!(u16::from_le_bytes([lo, hi]), 0x1234);
}

// BCD counting. Control words set bit 0 (BCD).
const CW_MODE0_BCD: u8 = CW_MODE0 | 1;
const CW_MODE2_BCD: u8 = CW_MODE2 | 1;

#[test]
fn bcd_dec_borrows_across_packed_nibbles() {
    // Values are packed BCD: 0x0100 is decimal 100, decrementing to 0x0099.
    assert_eq!(Counter::bcd_dec(0x0100, 1), 0x0099);
    assert_eq!(Counter::bcd_dec(0x0001, 1), 0x0000);
    assert_eq!(Counter::bcd_dec(0x0000, 1), 0x9999); // underflow wraps to top
    assert_eq!(Counter::bcd_dec(0x1000, 1), 0x0999);
    assert_eq!(Counter::bcd_dec(0x0000, 2), 0x9998); // two-step wrap
    assert_eq!(Counter::bcd_dec(0x0100, 2), 0x0098);
}

#[test]
fn bcd_reload_zero_is_full_decimal_range() {
    // Reload 0 in BCD loads 0x10000 so the first decrement wraps to 0x9999 and
    // the period is exactly 10000 input clocks; in binary it is 65536.
    let mut c = Counter {
        bcd: true,
        reload: 0,
        ..Default::default()
    };
    assert_eq!(c.effective_reload(), 0x10000);
    c.bcd = false;
    assert_eq!(c.effective_reload(), 65536);

    // The packed-BCD decrement takes 10000 steps from the full-range load to 0.
    let mut count = 0x10000u32;
    let mut steps = 0u32;
    loop {
        count = Counter::bcd_dec(count, 1);
        steps += 1;
        if count == 0 {
            break;
        }
    }
    assert_eq!(steps, 10000);
}

#[test]
fn bcd_mode2_counts_in_decimal() {
    // Program ch0 mode 2 BCD with count 0x0100 (= 100 decimal). The period in
    // input clocks must be 100, not 256.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE2_BCD, 0x0100);
    pit.tick(1); // load
    assert_eq!(pit.clocks_until_channel0_irq(), Some(100));
    assert_eq!(pit.tick(100), 1);
    assert_eq!(pit.tick(100), 1); // periodic
}

#[test]
fn bcd_mode0_one_shot_decimal() {
    // Mode 0 BCD one-shot: count 0x0050 (= 50 decimal) fires once at clock 50.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE0_BCD, 0x0050);
    pit.tick(1); // load
    assert_eq!(pit.clocks_until_channel0_irq(), Some(50));
    assert_eq!(pit.tick(50), 1);
    assert_eq!(pit.tick(1000), 0); // no repeat
}

#[test]
fn mode1_new_count_mid_pulse_waits_for_next_gate() {
    // A longer count written during a live mode-1 pulse must not abort
    // the pulse. The original count completes; the new count loads on the next
    // GATE rising edge.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE1, 4);
    // Trigger the one-shot with a GATE rising edge, then count partway.
    pit.set_gate(0, false);
    pit.set_gate(0, true);
    assert_eq!(pit.tick(2), 0); // two of four clocks consumed, pulse still live

    // Write a longer count (10) mid-pulse. The live pulse keeps its old count.
    pit.write_port(0x40, 10);
    pit.write_port(0x40, 0);
    assert!(!pit.channel_out(0)); // pulse still low, not aborted
    assert_eq!(pit.tick(2), 1); // original 4-clock pulse completes here
    assert!(pit.channel_out(0));

    // The new count only applies after the next GATE rising edge.
    pit.set_gate(0, false);
    pit.set_gate(0, true);
    assert_eq!(pit.tick(9), 0); // nine of the new ten clocks, still low
    assert_eq!(pit.tick(1), 1); // tenth clock completes the new pulse
}

#[test]
fn mode3_gate_falling_forces_out_high_immediately() {
    // Dropping GATE in mode 2/3 forces OUT high at once, with no tick.
    let mut pit = Pit::default();
    // Use channel 2 so the wiring is exercised on a non-IRQ counter.
    pit.write_port(0x43, 0xb6); // counter 2, LSB/MSB, mode 3, binary
    pit.write_port(0x42, 10);
    pit.write_port(0x42, 0);
    pit.tick(1); // load
    // Raise then drop GATE on channel 2.
    pit.set_gate(2, true);
    pit.set_gate(2, false);
    assert!(pit.channel_out(2)); // high immediately, with no intervening tick
}

#[test]
fn read_back_latch_nothing_is_a_no_op() {
    // A read-back with D5=D4=1 latches nothing. A following read must
    // still return the live count, not a stale latch.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE3, 100);
    pit.tick(1); // load: count = 100
    pit.tick(4); // mode 3 steps by two: 100 -> 92
    // Read-back, counter 0 selected, but neither count nor status latched.
    pit.write_port(0x43, 0xf2); // sc=11, D5=1, D4=1, counter-0 bit set
    pit.tick(4); // keeps counting: 92 -> 84
    // No latch was taken, so a normal read tracks the live count.
    pit.write_port(0x43, 0x00); // now latch for real
    let lo = pit.read_port(0x40).unwrap();
    let hi = pit.read_port(0x40).unwrap();
    assert_eq!(u16::from_le_bytes([lo, hi]), 84);
}

#[test]
fn read_back_latch_nothing_does_not_latch_status() {
    // The no-op form must not produce a status byte either: a plain read after
    // it returns the count, not a latched status.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE0, 0x1234);
    pit.tick(1); // load: count = 0x1234
    pit.tick(1); // first real decrement: 0x1234 -> 0x1233
    pit.write_port(0x43, 0xf2); // read-back latch-nothing, counter 0
    pit.write_port(0x43, 0x00); // counter-latch command -> count latched
    let lo = pit.read_port(0x40).unwrap();
    let hi = pit.read_port(0x40).unwrap();
    // The latch-nothing read-back left no status latched, so the read returns
    // the live count: 0x1234 loaded, decremented once to 0x1233.
    assert_eq!(u16::from_le_bytes([lo, hi]), 0x1233);
}

// Measure one full mode-3 period of channel 0 as (high_clocks, low_clocks) by
// counting input CLKs between OUT edges. The pit must already be loaded and
// counting. Counts the clocks OUT spends high then the clocks it spends low over
// the next complete cycle, so it is immune to where in the period we start.
fn measure_high_low(pit: &mut Pit) -> (usize, usize) {
    // Step to a falling edge: the clock after which OUT first reads low.
    let mut prev = pit.channel_out(0);
    loop {
        pit.tick(1);
        let now = pit.channel_out(0);
        if prev && !now {
            break; // falling edge: OUT just went low
        }
        prev = now;
    }
    // OUT is low now. Count low clocks until the rising edge.
    let mut low = 1; // the clock that drove OUT low counts as the first low clock
    loop {
        pit.tick(1);
        if pit.channel_out(0) {
            break; // rising edge: OUT back high
        }
        low += 1;
    }
    // OUT is high now. Count high clocks until the next falling edge.
    let mut high = 1; // the clock that drove OUT high is the first high clock
    loop {
        pit.tick(1);
        if !pit.channel_out(0) {
            break; // next falling edge ends the high phase
        }
        high += 1;
    }
    (high, low)
}

#[test]
fn mode3_odd_count_high_phase_is_one_clock_longer() {
    // Datasheet: an odd count N holds OUT high for (N+1)/2 clocks and low for
    // (N-1)/2 clocks, for a full period of N. Count 5 must give high 3, low 2.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE3, 5);
    pit.tick(1); // load
    let (high, low) = measure_high_low(&mut pit);
    assert_eq!((high, low), (3, 2));
    assert_eq!(high + low, 5); // halves still sum to the full count
}

#[test]
fn mode3_even_count_stays_symmetric() {
    // An even count N is symmetric: high N/2, low N/2. Count 6 gives 3 and 3.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE3, 6);
    pit.tick(1); // load
    let (high, low) = measure_high_low(&mut pit);
    assert_eq!((high, low), (3, 3));
}

#[test]
fn pop_speaker_count_rewrites_reproduction() {
    // Reproduce Prince of Persia's PC-speaker driver from the captured writes:
    // channel 2 set to mode 3, LSB+MSB, binary (control word 0xB6), GATE2 high,
    // then a fresh 16-bit count (~16344) written every ~408 PIT input clocks.
    // Count 16344 is ~73 Hz, so OUT should toggle about once per 8172-clock half
    // period (~2 times over this run), NOT once per count rewrite.
    let mut pit = Pit::default();
    pit.write_port(0x43, 0xB6); // counter 2, LSB+MSB, mode 3, binary
    pit.set_gate(2, true);
    let count: u16 = 16344; // ~73 Hz: half-period 8172 input clocks
    let updates = 60usize;
    let ticks_between = 408usize;
    let mut transitions = 0usize;
    let mut prev = pit.channel_out(2);
    for _ in 0..updates {
        pit.write_port(0x42, (count & 0xff) as u8); // LSB
        pit.write_port(0x42, (count >> 8) as u8); // MSB
        for _ in 0..ticks_between {
            pit.tick(1);
            let now = pit.channel_out(2);
            if now != prev {
                transitions += 1;
                prev = now;
            }
        }
    }
    let total_ticks = updates * ticks_between;
    let expected = total_ticks / (count as usize / 2);
    println!(
        "PoP repro: {transitions} OUT transitions over {total_ticks} ticks, {updates} rewrites; \
             real-hw expects ~{expected}"
    );
    // The tone must actually sound at its programmed rate. 0 = silent (each
    // rewrite reset the live counter); ~{updates} = OUT wrongly coupled to the
    // rewrite cadence. Correct is ~{expected} toggles for the 73 Hz tone.
    assert!(
        (2..=8).contains(&transitions),
        "mode-3 count 16344 should sound at ~73 Hz (~{expected} OUT toggles over \
             {total_ticks} ticks), but got {transitions}: a mid-count rewrite must not \
             reset the running counter"
    );
}

#[test]
fn mode3_odd_count_period_is_exact() {
    // Over a full period the asymmetric halves still sum to N input clocks, so
    // exactly one rising edge lands per N clocks for an odd count.
    let mut pit = Pit::default();
    program_ch0(&mut pit, CW_MODE3, 5);
    pit.tick(1);
    assert_eq!(pit.clocks_until_channel0_irq(), Some(5));
    assert_eq!(pit.tick(5), 1);
    assert_eq!(pit.tick(5), 1); // periodic
    assert_eq!(pit.tick(15), 3); // three more whole periods
}

#[test]
fn channel1_mode2_refresh_out_toggles_at_its_period() {
    // Channel 1 is the AT DRAM-refresh timer: mode 2, a short count. Its OUT
    // pulses low for one clock at the terminal count of every period, which a
    // refresh consumer reads through channel_out(1). Drive a count of 18 (the
    // typical AT refresh divisor) and confirm the OUT pin pulses once per period.
    let mut pit = Pit::default();
    // Counter 1, LSB/MSB, mode 2, binary: SC=01, RW=11, mode=010 -> 0x74.
    pit.write_port(0x43, 0x74);
    pit.write_port(0x41, 18);
    pit.write_port(0x41, 0);
    pit.tick(1); // load: count = 18, OUT high
    assert!(pit.channel_out(1), "mode 2 holds OUT high after load");

    // Walk one whole period at single-clock granularity and count the clocks
    // where OUT samples low. Mode 2 drops OUT for exactly one clock per period.
    let mut low_clocks = 0;
    for _ in 0..18 {
        pit.tick(1);
        if !pit.channel_out(1) {
            low_clocks += 1;
        }
    }
    assert_eq!(low_clocks, 1, "OUT pulses low once per refresh period");
    assert!(
        pit.channel_out(1),
        "OUT is back high at the period boundary"
    );

    // The next period repeats the single low pulse: the refresh timer is steady.
    let mut low_clocks = 0;
    for _ in 0..18 {
        pit.tick(1);
        if !pit.channel_out(1) {
            low_clocks += 1;
        }
    }
    assert_eq!(low_clocks, 1, "refresh OUT keeps pulsing every period");
}

// -- IZARRAVM_PIT_BULK_ADVANCE: the analytic advance vs the per-CLK loop -------
//
// The bar for this slice is IDENTITY, not plausibility: the guest state after a
// bulk advance must be what `clocks` calls to `Counter::step` produce, field for
// field, and the caller's two observables -- the channel-0 rising-edge COUNT and
// the watched channel's OUT transition LIST -- must match element for element.
// `Pit` and `OutTransition` both derive `PartialEq`, so every assertion below is
// a whole-value comparison rather than a field spot-check.

/// Control word for `channel` in `mode`, LSB-then-MSB, binary or BCD.
fn control_word(channel: usize, mode: u8, bcd: bool) -> u8 {
    ((channel as u8) << 6) | 0x30 | (mode << 1) | u8::from(bcd)
}

fn program_channel(pit: &mut Pit, channel: usize, mode: u8, count: u16, bcd: bool) {
    pit.write_port(0x43, control_word(channel, mode, bcd));
    let port = 0x40 + channel as u16;
    pit.write_port(port, (count & 0xff) as u8);
    pit.write_port(port, (count >> 8) as u8);
}

fn fresh_counters() -> crate::PitBulkAdvanceCounters {
    crate::PitBulkAdvanceCounters::default()
}

/// Run ONE advance of `clocks` CLKs on both arms from the same start state and
/// assert the edge count, the watched channel's transition list and the whole
/// post-advance chip agree. Returns whether the bulk arm actually engaged, so a
/// sweep can prove it is not passing by declining everything.
fn assert_arms_agree(base: &Pit, clocks: u64, channel: usize, label: &str) -> bool {
    let mut loop_pit = base.clone();
    let mut loop_transitions = Vec::new();
    let mut loop_counters = fresh_counters();
    let loop_edges = loop_pit.tick_recording_out_transitions(
        clocks,
        channel,
        &mut loop_transitions,
        false,
        &mut loop_counters,
    );

    let mut bulk_pit = base.clone();
    let mut bulk_transitions = Vec::new();
    let mut bulk_counters = fresh_counters();
    let bulk_edges = bulk_pit.tick_recording_out_transitions(
        clocks,
        channel,
        &mut bulk_transitions,
        true,
        &mut bulk_counters,
    );

    assert_eq!(
        bulk_edges, loop_edges,
        "{label}: channel-0 rising-edge count"
    );
    assert_eq!(
        bulk_transitions, loop_transitions,
        "{label}: channel-{channel} OUT transitions"
    );
    assert_eq!(bulk_pit, loop_pit, "{label}: post-advance chip state");
    if bulk_counters.advances > 0 {
        assert_eq!(
            bulk_counters.transitions,
            bulk_transitions.len() as u64,
            "{label}: the transition counter must count what was emitted"
        );
    }
    bulk_counters.advances > 0
}

#[test]
fn pit_bulk_advance_matches_the_step_loop_across_modes_reloads_phases_and_spans() {
    // Every mode x a spread of reloads (even, odd, minimum legal, the
    // datasheet's illegal 1, the full-range 0) x every phase across two-plus
    // periods, walked through the REAL tick path so both OUT phases and the
    // post-reload states are visited x spans shorter than, equal to and longer
    // than the period, including zero.
    //
    // Channel 1 stays the default AT refresh timer (mode 2, count 18) and
    // channel 2 runs a second live counter, so a bulk form that advanced only
    // the channels it is asked about fails on whole-chip equality.
    let mut engaged = 0u64;
    let mut declined = 0u64;
    for mode in 0..=5u8 {
        for reload in [2u16, 3, 4, 5, 6, 7, 18, 100, 101, 255, 1, 0] {
            let mut pit = Pit::default();
            program_channel(&mut pit, 2, 3, 7, false);
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, false); // arm the trigger edge below
            }
            program_channel(&mut pit, 0, mode, reload, false);
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, true); // rising edge starts the one-shot
            }
            let period = if reload == 0 {
                65536u64
            } else {
                u64::from(reload)
            };
            // The full-range reload's oracle costs 65536 steps per span, so it
            // gets fewer phases and stops one period past the boundary.
            let wide = period >= 1000;
            let phases = if wide { 2 } else { (2 * period + 4).min(40) };
            let spans: Vec<u64> = if wide {
                vec![0, 1, 2, period - 1, period, period + 1]
            } else {
                vec![
                    0,
                    1,
                    2,
                    period - 1,
                    period,
                    period + 1,
                    2 * period,
                    2 * period + 1,
                    3 * period + 5,
                ]
            };
            for phase in 0..phases {
                for &clocks in &spans {
                    for channel in [0usize, 2] {
                        let label = format!(
                            "mode {mode} reload {reload} phase {phase} clocks {clocks} \
                             watch channel {channel}"
                        );
                        if assert_arms_agree(&pit, clocks, channel, &label) {
                            engaged += 1;
                        } else {
                            declined += 1;
                        }
                    }
                }
                pit.tick(1); // walk the phase on the reference arm
            }
        }
    }
    assert!(
        engaged > 0,
        "the sweep must actually exercise the analytic path"
    );
    assert!(
        declined > 0,
        "the sweep must also reach the declines (reload 1 in modes 2 and 3, \
         and the empty advance)"
    );
}

#[test]
fn pit_bulk_advance_of_zero_clocks_moves_nothing() {
    // `Counter::advance` and `Counter::out_transitions_in` are asserted DIRECTLY
    // here, not through `tick_with_observer`. The chip-level entry point already
    // returns early on an empty advance, so the zero-CLK guards inside these two
    // are unreachable from production -- and an unreachable guard is exactly
    // what rots into a landmine the day a second caller appears. The contract is
    // pinned at the functions that state it. (Both guards SURVIVED the first
    // mutation round for precisely this reason; this test is what kills them.)
    for mode in 0..=5u8 {
        for reload in [2u16, 7, 100, 0] {
            let mut pit = Pit::default();
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, false);
            }
            program_channel(&mut pit, 0, mode, reload, false);
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, true);
            }
            // Every state a counter can be in: LoadDelay / WaitGate straight off
            // the write, Counting a few CLKs later, and Inactive once a one-shot
            // has finished.
            for warmup in [0u64, 3, 300] {
                pit.tick(warmup);
                let before = pit.counters[0].clone();
                let mut probe = before.clone();
                assert_eq!(
                    probe.advance(0),
                    0,
                    "mode {mode} reload {reload} warmup {warmup}: no CLK, no edge"
                );
                assert_eq!(
                    probe, before,
                    "mode {mode} reload {reload} warmup {warmup}: no CLK, no state change"
                );
                let mut emitted = 0u32;
                before.out_transitions_in(0, &mut |_, _| emitted += 1);
                assert_eq!(
                    emitted, 0,
                    "mode {mode} reload {reload} warmup {warmup}: no CLK, no transition"
                );
            }
        }
    }
}

#[test]
fn pit_bulk_advance_declines_bcd_counters_and_still_matches_the_loop() {
    // BCD is declined on the same ground as every analytic peek in this file: no
    // PC software clocks the PIT in BCD, so decimal half-cycles are not modeled.
    // The decline is NOT the knob -- a BCD counter takes the loop on both arms --
    // and the two arms must still land in the same place.
    for mode in 0..=5u8 {
        for reload in [0x02u16, 0x07, 0x18, 0x99, 0x0100] {
            let mut pit = Pit::default();
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, false);
            }
            program_channel(&mut pit, 0, mode, reload, true);
            if matches!(mode, 1 | 5) {
                pit.set_gate(0, true);
            }
            for phase in 0..4 {
                for clocks in [0u64, 1, 2, 5, 50, 300] {
                    let label =
                        format!("bcd mode {mode} reload {reload:#x} phase {phase} clocks {clocks}");
                    assert!(
                        !assert_arms_agree(&pit, clocks, 0, &label),
                        "{label}: a BCD counter must decline the analytic advance"
                    );
                    let mut probe = pit.clone();
                    let mut counters = fresh_counters();
                    probe.tick_arm(clocks, true, &mut counters);
                    if clocks > 0 {
                        assert_eq!(counters.declines_bcd, 1, "{label}");
                        assert_eq!(counters.advances, 0, "{label}");
                    }
                }
                pit.tick(1);
            }
        }
    }
}

#[test]
fn pit_bulk_advance_declines_an_illegal_mode_2_or_3_reload() {
    // Reload 1 is the datasheet's illegal input for modes 2 and 3 (count 2 is
    // the minimum). `step_counting` handles it loosely -- it reloads on EVERY
    // CLK -- which leaves the analytic form no period to fold and, in mode 3, no
    // half-period. It declines rather than guessing, and the decline names
    // itself so a row that lost the lever to an illegal count is diagnosable.
    for mode in [2u8, 3] {
        let mut pit = Pit::default();
        program_channel(&mut pit, 0, mode, 1, false);
        for clocks in [1u64, 2, 5, 40] {
            let label = format!("mode {mode} reload 1 clocks {clocks}");
            assert!(!assert_arms_agree(&pit, clocks, 0, &label), "{label}");
            let mut probe = pit.clone();
            let mut counters = fresh_counters();
            probe.tick_arm(clocks, true, &mut counters);
            assert_eq!(counters.declines_illegal_reload, 1, "{label}");
            assert_eq!(counters.declines_bcd, 0, "{label}");
            assert_eq!(counters.advances, 0, "{label}");
        }

        // The reachable form of the same input, and the one the read-only peeks
        // get WRONG (`counting_count_after` returns the reload outright for
        // `r <= 1`): a live counter whose RELOAD is rewritten to 1 while its
        // counting element is still large. `arm` deliberately does not reset the
        // live count for modes 2 and 3, so this state really does occur.
        let mut running = Pit::default();
        program_channel(&mut running, 0, mode, 100, false);
        running.tick(10);
        running.write_port(0x40, 1);
        running.write_port(0x40, 0);
        assert!(running.counters[0].count > 1, "mode {mode}: CE still large");
        for clocks in [1u64, 50, 89, 90, 91, 200] {
            let label = format!("mode {mode} reload rewritten to 1, clocks {clocks}");
            assert!(!assert_arms_agree(&running, clocks, 0, &label), "{label}");
        }
    }
}

#[test]
fn pit_bulk_advance_matches_the_step_loop_with_the_gate_low() {
    for mode in 0..=5u8 {
        for from_load_delay in [false, true] {
            let mut pit = Pit::default();
            program_channel(&mut pit, 0, mode, 50, false);
            if !from_load_delay {
                pit.tick(3); // past the load, into Counting
            }
            pit.set_gate(0, false);
            for clocks in [0u64, 1, 2, 3, 10, 120] {
                let label =
                    format!("mode {mode} load_delay {from_load_delay} gate low clocks {clocks}");
                assert_arms_agree(&pit, clocks, 0, &label);
            }
        }
    }
}

#[test]
fn pit_bulk_advance_reproduces_the_gate_low_mode_2_and_3_out_force() {
    // `step_counting` forces OUT high on the first paused CLK in modes 2 and 3.
    // The read-only peeks do NOT: `out_after`'s Counting + !gate arm returns the
    // STORED level. Today the two agree, because the only two ways into a
    // GATE-low mode-2/3 state (`set_gate`'s falling edge and `write_control`)
    // both set OUT high on the way in -- so this state is CONSTRUCTED, not
    // programmed. A bulk advance writes the field, and writing back the peeks'
    // answer here would be a real state divergence.
    for mode in [2u8, 3] {
        let counter = Counter {
            mode,
            rw: RwMode::LsbThenMsb,
            reload: 50,
            count: 20,
            out: false,
            gate: false,
            state: CounterState::Counting,
            ..Counter::default()
        };
        let base = pit_with_counter(0, counter);
        assert!(
            !base.counters[0].out,
            "mode {mode}: the premise of this test"
        );

        // Zero CLKs is not an advance: nothing may move.
        let mut idle = base.clone();
        let mut counters = fresh_counters();
        idle.tick_arm(0, true, &mut counters);
        assert!(!idle.counters[0].out, "mode {mode}: no CLK, no force");
        assert_eq!(idle, base, "mode {mode}: no CLK, no state change");

        for clocks in [1u64, 2, 7, 400] {
            let label = format!("constructed gate-low mode {mode} clocks {clocks}");
            assert!(assert_arms_agree(&base, clocks, 0, &label), "{label}");
            let mut probe = base.clone();
            let mut counters = fresh_counters();
            probe.tick_arm(clocks, true, &mut counters);
            assert!(
                probe.counters[0].out,
                "{label}: the GATE-low force must drive OUT high"
            );
        }

        // From LoadDelay the FIRST CLK is the unconditional load, so the force
        // needs two CLKs, not one.
        let load_delay = pit_with_counter(
            0,
            Counter {
                state: CounterState::LoadDelay,
                ..base.counters[0].clone()
            },
        );
        for clocks in [1u64, 2, 3] {
            let label = format!("constructed gate-low LoadDelay mode {mode} clocks {clocks}");
            assert!(assert_arms_agree(&load_delay, clocks, 0, &label), "{label}");
            let mut probe = load_delay.clone();
            let mut counters = fresh_counters();
            probe.tick_arm(clocks, true, &mut counters);
            assert_eq!(
                probe.counters[0].out,
                clocks >= 2,
                "{label}: the load CLK cannot force OUT"
            );
        }
    }
}

#[test]
fn pit_bulk_advance_clears_null_count_only_on_the_load_clk() {
    // `null_count` is cleared in exactly one place -- `step`'s LoadDelay arm --
    // and the mode-2/3 reload inside `step_counting` does NOT clear it. Modes 1
    // and 5 park in WaitGate and load on the GATE edge instead, so they have no
    // LoadDelay CLK to test here.
    for mode in [0u8, 2, 3, 4] {
        let mut pit = Pit::default();
        program_channel(&mut pit, 0, mode, 40, false);
        assert!(
            pit.counters[0].null_count,
            "mode {mode}: armed by the write"
        );
        assert_eq!(pit.counters[0].state, CounterState::LoadDelay);
        for (clocks, still_armed) in [(0u64, true), (1, false), (2, false), (90, false)] {
            let mut bulk = pit.clone();
            let mut bulk_counters = fresh_counters();
            bulk.tick_arm(clocks, true, &mut bulk_counters);
            let mut reference = pit.clone();
            let mut loop_counters = fresh_counters();
            reference.tick_arm(clocks, false, &mut loop_counters);
            assert_eq!(
                bulk.counters[0].null_count, still_armed,
                "mode {mode} clocks {clocks}"
            );
            assert_eq!(bulk, reference, "mode {mode} clocks {clocks}");
        }
    }

    // A mode-2/3 count rewritten while the counter is already Counting re-arms
    // `null_count` but stays in Counting, so nothing ever clears it again. A
    // bulk advance must not clear it either.
    for mode in [2u8, 3] {
        let mut pit = Pit::default();
        program_channel(&mut pit, 0, mode, 40, false);
        pit.tick(5);
        assert!(!pit.counters[0].null_count);
        pit.write_port(0x40, 30);
        pit.write_port(0x40, 0);
        assert_eq!(pit.counters[0].state, CounterState::Counting);
        assert!(pit.counters[0].null_count, "mode {mode}: re-armed in place");
        for clocks in [1u64, 2, 39, 40, 100] {
            let mut probe = pit.clone();
            let mut counters = fresh_counters();
            probe.tick_arm(clocks, true, &mut counters);
            assert!(
                probe.counters[0].null_count,
                "mode {mode} clocks {clocks}: a reload is not a load"
            );
            assert_arms_agree(
                &pit,
                clocks,
                0,
                &format!("mode {mode} re-armed clocks {clocks}"),
            );
        }
    }
}

/// One scripted operation against the chip, applied to both arms in lockstep.
#[derive(Debug, Clone, Copy)]
enum PitOp {
    Advance(u64),
    Gate(usize, bool),
    Write(u16, u8),
    Read(u16),
}

/// Drive both arms through the same script, comparing after EVERY step. This is
/// where the mid-run events live: a GATE edge and a count rewrite cannot happen
/// inside one advance (both are only reachable from a port write, which already
/// ends the CPU batch), so "mid-advance" really means "between two advances",
/// and the script interleaves them at several phases of the period.
fn assert_script_agrees(script: &[PitOp], watch: usize, label: &str) {
    let mut bulk = Pit::default();
    let mut reference = Pit::default();
    let mut bulk_counters = fresh_counters();
    let mut loop_counters = fresh_counters();
    let mut engaged = false;
    for (index, op) in script.iter().enumerate() {
        match *op {
            PitOp::Advance(clocks) => {
                let mut bulk_transitions = Vec::new();
                let mut loop_transitions = Vec::new();
                let before = bulk_counters.advances;
                let bulk_edges = bulk.tick_recording_out_transitions(
                    clocks,
                    watch,
                    &mut bulk_transitions,
                    true,
                    &mut bulk_counters,
                );
                let loop_edges = reference.tick_recording_out_transitions(
                    clocks,
                    watch,
                    &mut loop_transitions,
                    false,
                    &mut loop_counters,
                );
                engaged |= bulk_counters.advances > before;
                assert_eq!(bulk_edges, loop_edges, "{label} step {index}: edges");
                assert_eq!(
                    bulk_transitions, loop_transitions,
                    "{label} step {index}: transitions"
                );
            }
            PitOp::Gate(channel, level) => {
                bulk.set_gate(channel, level);
                reference.set_gate(channel, level);
            }
            PitOp::Write(port, value) => {
                bulk.write_port(port, value);
                reference.write_port(port, value);
            }
            PitOp::Read(port) => {
                assert_eq!(
                    bulk.read_port(port),
                    reference.read_port(port),
                    "{label} step {index}: port {port:#x} read"
                );
            }
        }
        assert_eq!(
            bulk, reference,
            "{label} step {index}: chip state after {op:?}"
        );
    }
    assert!(engaged, "{label}: the analytic path never engaged");
}

#[test]
fn pit_bulk_advance_matches_the_step_loop_across_gate_edges_and_count_rewrites() {
    for mode in 0..=5u8 {
        let script = vec![
            PitOp::Write(0x43, control_word(0, mode, false)),
            PitOp::Write(0x40, 100),
            PitOp::Write(0x40, 0),
            PitOp::Advance(3),
            PitOp::Gate(0, false),
            PitOp::Advance(5),
            PitOp::Gate(0, true),
            PitOp::Advance(7),
            PitOp::Read(0x40),
            PitOp::Advance(97),
            PitOp::Write(0x40, 10),
            PitOp::Advance(1),
            PitOp::Write(0x40, 0),
            PitOp::Advance(250),
            PitOp::Write(0x43, 0x00), // counter-0 latch command
            PitOp::Advance(9),
            PitOp::Read(0x40),
            PitOp::Read(0x40),
            PitOp::Gate(0, false),
            PitOp::Advance(11),
            PitOp::Gate(0, true),
            PitOp::Advance(65540),
            // Channel 2's own gate, the one port 0x61 bit 0 drives.
            PitOp::Write(0x43, control_word(2, 3, false)),
            PitOp::Write(0x42, 9),
            PitOp::Write(0x42, 0),
            PitOp::Advance(4),
            PitOp::Gate(2, false),
            PitOp::Advance(6),
            PitOp::Gate(2, true),
            PitOp::Advance(40),
            PitOp::Advance(0),
            PitOp::Advance(1),
        ];
        assert_script_agrees(&script, 2, &format!("mode {mode} watching channel 2"));
        assert_script_agrees(&script, 0, &format!("mode {mode} watching channel 0"));
    }
}

#[test]
fn pit_bulk_advance_both_arms_are_reachable_in_one_binary() {
    // The ladder drives both arms from ONE binary, so both must be reachable
    // without a rebuild -- and the OFF arm must be provably the loop, not the
    // bulk path wearing the loop's label.
    let mut pit = Pit::default();
    program_channel(&mut pit, 0, 2, 100, false);
    let mut off_arm = pit.clone();
    let mut on_arm = pit.clone();
    let mut off = fresh_counters();
    let mut on = fresh_counters();
    let off_edges = off_arm.tick_arm(500, false, &mut off);
    let on_edges = on_arm.tick_arm(500, true, &mut on);

    assert_eq!(
        off_edges, on_edges,
        "the two arms must count the same edges"
    );
    assert_eq!(off_arm, on_arm, "the two arms must land in the same state");
    assert!(off_edges > 0, "the advance must have produced edges at all");

    // OFF: nothing analytic, and the decline names the knob rather than a
    // property of the chip.
    assert_eq!(off.advances, 0);
    assert_eq!(off.advance_clocks, 0);
    assert_eq!(off.declines_knob_off, 1);
    assert_eq!(off.loop_advances, 1);
    assert_eq!(off.loop_clocks, 500);
    assert_eq!(off.declines_bcd, 0);
    assert_eq!(off.declines_illegal_reload, 0);
    assert_eq!(off.declines_span_too_wide, 0);

    // ON: analytic, with no per-CLK loop at all.
    assert_eq!(on.advances, 1);
    assert_eq!(on.advance_clocks, 500);
    assert_eq!(on.loop_advances, 0);
    assert_eq!(on.loop_clocks, 0);
    assert_eq!(on.declines_knob_off, 0);
}

#[test]
fn pit_bulk_advance_declines_a_span_wider_than_the_counting_element() {
    // 2^32 CLKs is an hour of guest time inside one device advance and the
    // fallback would need four billion iterations to serve it, so this is a
    // guard rather than a path. It is asserted anyway: the mode-0/1/4/5 wrap
    // arithmetic below it is only exact under this cap.
    //
    // The predicate is checked directly rather than through `tick_arm`, because
    // taking the fallback here means running the four-billion-iteration loop the
    // guard exists to stay away from -- that costs two minutes of test time and
    // proves nothing the predicate does not.
    let mut pit = Pit::default();
    program_channel(&mut pit, 0, 2, 100, false);
    assert_eq!(pit.bulk_decline((1u64 << 32) - 1), None);
    assert_eq!(pit.bulk_decline(1u64 << 32), Some(BulkDecline::SpanTooWide));
    assert_eq!(pit.bulk_decline(u64::MAX), Some(BulkDecline::SpanTooWide));

    // And the counter really is the one that moves on that path.
    let mut counters = fresh_counters();
    let mut probe = pit.clone();
    probe.tick_arm(1_000, true, &mut counters);
    assert_eq!(counters.declines_span_too_wide, 0);
    assert_eq!(counters.advances, 1);
}

#[test]
fn pit_bulk_advance_knob_reads_unset_and_empty_as_the_default() {
    use std::env::VarError;
    // DEFAULT OFF, and empty means DEFAULT rather than "the OFF arm on purpose".
    // Those coincide only while the default is off; the rule is written as
    // "empty == unset" so it survives the flip.
    assert!(!parse_bulk_advance_arm(Err(VarError::NotPresent)));
    assert!(!parse_bulk_advance_arm(Ok(String::new())));
    assert!(!parse_bulk_advance_arm(Ok("   ".to_string())));
    assert!(!parse_bulk_advance_arm(Ok("0".to_string())));
    assert!(!parse_bulk_advance_arm(Ok("off".to_string())));
    assert!(!parse_bulk_advance_arm(Ok(" OFF ".to_string())));
    assert!(parse_bulk_advance_arm(Ok("1".to_string())));
    assert!(parse_bulk_advance_arm(Ok("on".to_string())));
    assert!(parse_bulk_advance_arm(Ok(" On ".to_string())));
}

#[test]
#[should_panic(expected = "names no arm")]
fn pit_bulk_advance_knob_refuses_a_typo() {
    // A mistyped ladder leg that fell through to the default would be read as
    // "the arm I named changed nothing".
    let _ = parse_bulk_advance_arm(Ok("yes".to_string()));
}

#[test]
#[should_panic(expected = "not valid UTF-8")]
fn pit_bulk_advance_knob_refuses_a_non_utf8_value() {
    let _ = parse_bulk_advance_arm(Err(std::env::VarError::NotUnicode(
        std::ffi::OsString::from("on"),
    )));
}
