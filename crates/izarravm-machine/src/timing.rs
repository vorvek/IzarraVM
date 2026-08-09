// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Shared machine and device timing.

use super::*;

pub const OPL_NATIVE_HZ: u32 = 49_716;
pub const DAC_HZ: u32 = 44_100;
/// Ceiling on DAC-rate frames a source carries between render windows.
///
/// A source's input count comes from elapsed guest master ticks, which arrive
/// in bursts as the emulation thread runs, while the window size comes from the
/// OPL resampler under smooth host pacing. The two never agree frame-for-frame,
/// so each source queues its surplus rather than discarding it and repeating a
/// frame to cover the shortfall. 4410 frames is 100 ms at 44100 Hz -- far above
/// ordinary jitter, so reaching it means the guest is genuinely outrunning the
/// host drain, which is worth reporting rather than absorbing silently.
pub const DAC_PENDING_FRAME_CAP: usize = 4410;

/// Digital headroom reserved below full scale on the ReSonique 2's summing
/// node, as a linear gain: 0.5 is -6.02 dB, exactly one bit.
///
/// The CT1745 powers on with master, voice, FM and CD all at level 31 (0 dB),
/// matching DOSBox-X's `CTMIXER_Reset` and 86Box. Every one of those legs is
/// therefore unity, and they SUM: a title playing digital effects over FM music
/// -- Duke Nukem 3D, the common case -- drives the summing node past full scale
/// with both legs at their defaults and nothing misprogrammed. Saturating that
/// at `clamp_i16` is not just loud, it is destructive in a way a level control
/// cannot undo: symmetric clipping squashes a panned image toward the centre
/// (both channels rail at the same value) and turns every waveform into a
/// square. That is exactly the "no stereo separation" plus "peaked and muffled"
/// pair reported against the volume-decode fix.
///
/// Reserving the headroom here, once, after the legs are summed, is the only
/// placement that leaves the hardware register semantics untouched -- guest
/// reads, writes and read-modify-write round-trips of `0x30`-`0x37`/`0x41`/`0x42`
/// are unchanged -- and preserves the RELATIVE balance the decode fix
/// established (FM against voice against CD) exactly, since a single scalar
/// divides out of every ratio.
///
/// Making the mix quieter is the point: SNDMIXER.COM owns amplification (see
/// `dev_docs/sndmixer-spec.md`), and its faders have nothing to raise if the
/// defaults already sit on the clamp.
///
/// The shape is not novel: 86Box reserves headroom for the same reason. Its
/// placement is different, and worth being exact about, because the difference
/// is the whole argument for one post-sum scalar.
///
/// 86Box attenuates PER LEG, ahead of the sum, and by DIFFERENT amounts
/// (`src/sound/snd_sb.c`, read as study only under `dev_docs/reference/86box`;
/// the concept is cited, no code taken):
///
/// - digital voice, `sb_get_buffer_sb16_awe32`: `dsp * voice_l / 3.0` (-9.54 dB)
/// - CD-in, `sb16_awe32_filter_cd_audio`: `buffer * cd / 3.0 * master` (-9.54 dB)
/// - PC speaker, `sb16_awe32_filter_pc_speaker`: `/ 3.0` likewise
/// - external MIDI in through the FM registers, `sb16_awe32_filter_midi`: `/ 3.0`
/// - but the INTERNAL OPL leg, `sb_get_music_buffer_sb16_awe32:501-503`:
///   `opl_buf * fm_l * 0.7171630859375`, which is -2.89 dB and no `/ 3.0` at all
///
/// So there is no single 86Box number to match: its FM sits 6.65 dB ABOVE its
/// voice by construction, and copying that would undo exactly what the CT1745
/// volume-decode fix corrected here (FM running over voice). One scalar after
/// the sum keeps the FM/voice/CD ratios the decode fix established and leaves
/// only the absolute level to choose.
///
/// 6.02 dB is that choice. It leaves the DIGITAL VOICE leg within 0.4 dB of the
/// absolute level it had before the decode fix, so a title's effects land where
/// they used to while the FM bus comes down to meet them.
pub const MIX_HEADROOM: f32 = 0.5;

pub const PIT_INPUT_HZ: u32 = 1_193_182;
pub const WSS_AUTOCAL_FALLBACK_HZ: u32 = 8000;

/// How long a PIT counter access keeps the Accurate class on its fine batch
/// grain. See `MachineBus::note_pit_observer`.
///
/// 5 ms is chosen to cover the "compute" leg of a latch-compute-latch
/// calibration -- BIOS and game delay-loop calibrations time windows well under
/// a millisecond -- without pinning a machine to the fine grain because
/// something read a counter once. It is guest time, not host time, so it does
/// not vary with the CPU mode.
pub(crate) const PIT_OBSERVER_FINE_WINDOW_TICKS: u64 = izarravm_core::MASTER_CLOCK_HZ / 200;

impl Machine {
    pub(super) fn stall_for_micros(&mut self, micros: u64) {
        let master_ticks = (u128::from(micros) * u128::from(izarravm_core::MASTER_CLOCK_HZ)
            / 1_000_000)
            .min(u128::from(u64::MAX)) as u64;
        self.stall_for_master_ticks(master_ticks);
    }

    pub(super) fn stall_until_margo_frame(&mut self) {
        if let Some(master_ticks) = self.timeline.master_ticks_until(
            timeline::DeviceClock::MargoFrame,
            1,
            timeline::MARGO_FRAME_HZ,
        ) {
            self.stall_for_master_ticks(master_ticks);
        }
    }

    pub(super) fn stall_for_master_ticks(&mut self, master_ticks: u64) {
        let cpu_clocks = self.timeline.cpu_clocks_for_master_ticks_ceil(master_ticks);
        self.advance_master_time(master_ticks, true, 0);
        self.elapsed_clocks = self.elapsed_clocks.saturating_add(cpu_clocks);
        self.io_stall_clocks = self.io_stall_clocks.saturating_add(cpu_clocks);
    }

    /// CPU-domain work and budget clocks. This compatibility metric changes unit
    /// with the active mode and is not global guest time. Use `master_ticks` for
    /// device deadlines and cross-mode timestamps.
    pub fn elapsed_clocks(&self) -> u64 {
        self.elapsed_clocks
    }

    /// CPU-domain budget clocks spent blocked on device I/O. The fixed-time
    /// equivalent is available from `io_stall_ticks`.
    pub fn io_stall_clocks(&self) -> u64 {
        self.io_stall_clocks
    }

    /// Monotonic global guest time in fixed 6.6 GHz master ticks. Unlike
    /// `elapsed_clocks`, this value keeps one unit across live CPU-mode changes.
    pub fn master_ticks(&self) -> u64 {
        self.timeline.now_ticks()
    }

    pub fn io_stall_ticks(&self) -> u64 {
        self.timeline.io_stall_ticks()
    }

    /// Monotonic master ticks advanced while the CPU was parked by HLT.
    pub fn halted_ticks(&self) -> u64 {
        self.halted_ticks
    }

    /// Scale a step's raw bus clocks by the active level's `bus_timing` factor,
    /// carrying the fractional remainder so a cheap access in a fast mode is not
    /// rounded to zero. This is the THIRD timing lever (B-T10): it scales the whole
    /// bus portion (instruction fetch + every tiered data access already summed into
    /// `raw`) per mode, supplying the absolute per-mode magnitude that lets a fast
    /// part pull away from the flat per-access floor. The relative L1<L2<RAM tier
    /// structure stays in the `tier_cost` wait-states; this only sets the overall
    /// scale. Cheap by construction: one multiply + a modulo per call, not per
    /// access. Mirrors the CPU's `scale_clocks` for instruction clocks.
    pub(super) fn scale_bus(&mut self, raw: u64) -> u64 {
        let (num, den) = bus_timing(self.cpu.level());
        let scaled = raw * u64::from(num) + self.bus_rem;
        self.bus_rem = scaled % u64::from(den);
        scaled / u64::from(den)
    }

    fn apply_device_advance(&mut self, advance: DeviceAdvance) {
        self.opl.advance_micros(advance.microseconds);
        let sb16_irq = {
            let Machine {
                sb16, dma, memory, ..
            } = self;
            sb16.advance(advance.microseconds, advance.dsp_frames, dma, memory)
        };
        if let Some(irq) = sb16_irq {
            self.pic.request(irq.line());
        }

        // AD1848 / Windows Sound System playback, clock-driven exactly like the
        // SB16 DSP above but on the codec's own base/IRQ/DMA -- no cross-talk with
        // the SB16's mixer-selected IRQ/DMA. The codec pulls 1/2/4 byte-wide DMA
        // reads per output frame internally (8/16-bit, mono/stereo), so a single
        // byte fetcher feeds tick_sample. advance_autocal retires the post-MCE ACI
        // window one output period per frame, and the terminal-count IRQ forwards
        // to the configured PIC line. Gated entirely on wss_enabled.
        if self.wss_enabled {
            let programmed_rate = self.wss.output_frame_rate();
            let autocal_active = self.wss.autocal_active();
            // The output sample clock paces both the DMA render and the autocal
            // (ACI) countdown. On real hardware the autocal converter clock retires
            // its ~128-sample window regardless of the *programmed* sample rate, so
            // when ACI is draining while I8 selects one of the two unsupported XTAL1
            // selects (rate_hz()==0) we fall back to the lowest documented WSS rate
            // (8000 Hz) just to clock the ACI countdown -- otherwise a guest that
            // clears MCE under an invalid rate would leave ACI asserted forever.
            // DMA render is still gated on a *valid* programmed rate below, so no
            // audio is produced at the fallback cadence.
            let wss_rate = if programmed_rate > 0 {
                programmed_rate
            } else if autocal_active {
                WSS_AUTOCAL_FALLBACK_HZ
            } else {
                0
            };
            // Read live, not from a cached copy: the config register moves the
            // codec's resources at runtime (SNDCTRL.COM does exactly that).
            let wss_dma = self.wss.dma();
            let wss_irq = self.wss.irq();
            // Run the sample clock whenever there is actual per-frame work pending:
            // either playback is armed (and the rate is valid), or the post-MCE ACI
            // window is still retiring (a driver clears MCE and polls ACI before
            // setting PEN). Gating on work mirrors the DSP path's `is_playing()`
            // check so an idle codec -- the default state on every machine at
            // power-on (rate 8000 Hz, not playing, no autocal) -- skips the
            // accumulation entirely instead of spinning ~8000 times/sec.
            let playing_at_valid_rate = programmed_rate > 0 && self.wss.is_playing();
            if wss_rate > 0 && (playing_at_valid_rate || autocal_active) {
                let n = advance.wss_frames as usize;
                if n > 0 {
                    // Pre-fetch WSS data into the HLE block buffer. Large batches
                    // can span multiple blocks, so refill after auto-reload.
                    let mut remaining = n;
                    while remaining > 0 && playing_at_valid_rate {
                        let bytes_per_frame = self.wss.bytes_per_frame();
                        if self.wss.block_buffer().is_none() {
                            let frames = self.wss.current_dma_count() as usize + 1;
                            let count = frames * bytes_per_frame;
                            let mut buf = Vec::with_capacity(count);
                            {
                                let Machine { dma, memory, .. } = self;
                                for _ in 0..count {
                                    let Some(byte) = dma.read_byte(wss_dma, memory) else {
                                        break;
                                    };
                                    buf.push(byte);
                                }
                            }
                            let complete_bytes = buf.len() / bytes_per_frame * bytes_per_frame;
                            buf.truncate(complete_bytes);
                            if !buf.is_empty() {
                                self.wss.set_block_buffer(buf);
                            }
                        }
                        let mut consumed_from_buf: usize = 0;
                        let processed_frames = if let Some(buf) = self.wss.block_buffer().cloned() {
                            let start_pos = self.wss.block_buffer_pos();
                            let bytes_avail = buf.len().saturating_sub(start_pos);
                            let frames_this = (bytes_avail / bytes_per_frame).min(remaining);
                            self.wss.tick_n_samples(frames_this, || {
                                let p = start_pos + consumed_from_buf;
                                if p < buf.len() {
                                    let b = buf[p];
                                    consumed_from_buf += 1;
                                    Some(b)
                                } else {
                                    None
                                }
                            })
                        } else {
                            let Machine {
                                wss, dma, memory, ..
                            } = self;
                            wss.tick_n_samples(remaining, || dma.read_byte(wss_dma, memory))
                        };
                        if consumed_from_buf > 0 {
                            self.wss.advance_block_buffer(consumed_from_buf);
                        }
                        if self.wss.block_buffer_pos() >= self.wss.block_buffer_len() {
                            self.wss.take_block_buffer();
                        }
                        if processed_frames == 0 {
                            break;
                        }
                        remaining -= processed_frames;
                    }
                    for _ in 0..n {
                        self.wss.advance_autocal();
                    }
                    // Forward any terminal-count edge produced in the batch (one
                    // request after N frames follows the multi-edge coalescing
                    // contract; see DSP path).
                    if self.wss.take_irq() {
                        self.pic.request(wss_irq);
                    }
                }
            }
        }

        // Advance Red Book CD audio at 44.1 kHz from guest elapsed time.
        // Drive the playback LBA from guest elapsed time so position is accurate
        // independent of when the mixer drains samples. Pull in render_audio
        // consumes from the advanced position (frac for sub-frame continuity).
        // Fixed rate, no "programmed" variation.
        if self.ide.device().playback().playing && advance.cd_frames > 0 {
            self.ide
                .device_mut()
                .advance_play(advance.cd_frames.min(u64::from(u32::MAX)) as u32);
        }

        let ch2_before = self.pit.channel_out(2);
        self.speaker_transitions.clear();
        let edges = self.pit.tick_recording_out_transitions(
            advance.pit_clocks,
            2,
            &mut self.speaker_transitions,
        );
        // Per-edge forwarding, same multi-edge contract as the DSP loop above:
        // N channel-0 edges in one step issue N requests and the PIC's IRR
        // coalesces them into the one interrupt the guest can actually take.
        for _ in 0..edges {
            self.pic.request(0); // channel 0 OUT rising edge is IRQ0
        }

        // PC speaker: integrate channel-2 OUT transitions at PIT-clock precision,
        // then let the speaker model produce DAC-rate samples.
        let pit_phase = RatePhase::with_remainder(advance.pit_remainder_before);
        let transitions = self.speaker_transitions.iter().map(|event| {
            (
                pit_phase
                    .ticks_until(event.tick, u64::from(PIT_INPUT_HZ))
                    .unwrap_or(0),
                event.level,
            )
        });
        self.speaker
            .accumulate(advance.master_ticks, ch2_before, transitions);

        self.keyboard.advance_master_ticks(advance.master_ticks);

        // Both MPU ports share the period-correct IRQ9 line. Their intelligent
        // sequencers keep absolute master time, so an in-batch command write and
        // the batch-end advance agree on the first clock pulse after that write.
        let now_tick = self.timeline.now_ticks();
        self.wavetable_mpu.advance_to(now_tick);
        self.midi_mpu.advance_to(now_tick);
        self.pic.set_irq_level(
            9,
            self.wavetable_mpu.irq_level() || self.midi_mpu.irq_level(),
        );

        self.serial.advance_master_ticks(advance.master_ticks);
        self.serial2.advance_master_ticks(advance.master_ticks);
        self.lpt.advance_master_ticks(advance.master_ticks);
        self.lpt2.advance_master_ticks(advance.master_ticks);
        if self
            .rtc
            .advance_master_ticks(advance.master_ticks, advance.rtc_seconds)
        {
            self.pic.request(8);
        }

        // The FDC owns mechanical and byte deadlines in the fixed timeline.
        // Each due byte performs exactly one channel-2 cycle before the command
        // engine schedules the following byte or enters its result phase.
        self.advance_fdc_to(self.timeline.now_ticks());

        // ATA PIO and PIIX4 bus-master transfers share the authoritative master timeline. A
        // device-to-memory completion bypasses the CPU bus, so report its PRD spans directly to
        // the CPU. This also covers host-driven advances outside the instruction run loop.
        if let Some(disk) = self.ata.as_mut() {
            disk.advance_master_ticks(advance.master_ticks);
            if let Some(spans) = self.bmide.advance_master_ticks_with_writes(
                advance.master_ticks,
                &mut self.memory,
                disk,
            ) {
                for span in spans {
                    self.cpu
                        .note_device_memory_write_range(span.address, span.len);
                }
            }
        }
        self.ide.advance_master_ticks(advance.master_ticks);

        // Timed keyboard and auxiliary lines hold while OBF is set. UART and
        // LPT edge latches coalesce at their PIC inputs. Their deadlines cap
        // normal CPU batches, while a larger host-driven advance may cross
        // several transitions before the pending state is forwarded here.
        self.pic.set_irq_level(1, self.keyboard.irq1_level());
        if self.serial.take_irq() {
            self.pic.request(4); // IRQ4: COM1 (0x3F8) has a pending UART interrupt
        }
        if self.serial2.take_irq() {
            self.pic.request(3); // IRQ3: COM2 (0x2F8) has a pending UART interrupt
        }
        self.pic.set_irq_level(12, self.keyboard.irq12_level());
        if self.lpt.take_irq() {
            // IRQ7: LPT1 -ACK after a strobed byte. The Sound Blaster DSP can also
            // route to IRQ7, so this line is shared; the LPT only requests it on a
            // real strobed byte with control bit 4 set.
            self.pic.request(7);
        }
        if self.lpt2.take_irq() {
            self.pic.request(5); // IRQ5: LPT2 (0x278) -ACK after a strobed byte
        }

        // The floppy disk controller raises IRQ6 on command completion and seek
        // end. The DOR DMA/IRQ gate is honored inside take_irq, so a guest that
        // polls the FDC with the gate off does not get a spurious line.
        if self.fdc.take_irq() {
            self.pic.request(6);
        }

        // ATAPI command completion forwards IRQ15 (the secondary channel) to the
        // PIC, the way a real drive interrupts the host when a packet finishes.
        if self.ide.take_irq() {
            self.bmide.note_ide_irq(true);
            self.pic.request(ide::SECONDARY_IRQ);
        }
        // ATA hard-disk completion forwards IRQ14 (the primary channel) the same
        // way. The access-byte count flashes the C: LED through c_accesses.
        if let Some(disk) = self.ata.as_mut() {
            if disk.take_irq() {
                self.bmide.note_ide_irq(false);
                self.pic.request(ata::PRIMARY_IRQ);
            }
            if disk.take_access_bytes() > 0 {
                self.c_accesses += 1;
            }
        }
        // Flash the GUI CD LED for any data the drive just served.
        let cd_pio_bytes = self.ide.take_access_bytes();
        if cd_pio_bytes > 0 {
            self.cd_accesses += 1;
            self.cd_pio_bytes = self.cd_pio_bytes.saturating_add(cd_pio_bytes as u64);
        }

        self.vega.advance(
            advance.margo_nanoseconds,
            advance.margo_frames,
            advance.distira_lines,
            advance.vga_dots,
        );

        self.vega.pump_pusher(&self.memory);
    }

    fn advance_fdc_to(&mut self, target_ticks: u64) {
        const FDC_DMA_CHANNEL: usize = 2;
        let mut write_ranges: Vec<(u32, u32)> = Vec::new();
        while let Some(request) = self.fdc.advance_to(target_ticks) {
            if request.offset == 0 && request.sector == request.transfer.sector {
                self.floppy_accesses = self.floppy_accesses.saturating_add(1);
            }

            let transferred = if request.transfer.read {
                let byte = self
                    .floppy
                    .as_ref()
                    .and_then(|floppy| {
                        floppy.read_sector(
                            u16::from(request.transfer.cylinder),
                            request.transfer.head,
                            request.sector,
                        )
                    })
                    .and_then(|sector| sector.get(usize::from(request.offset)))
                    .copied();
                byte.is_some_and(|byte| {
                    let Some(address) =
                        self.dma.write_byte(FDC_DMA_CHANNEL, &mut self.memory, byte)
                    else {
                        return false;
                    };
                    match write_ranges.last_mut() {
                        Some((start, width)) if start.checked_add(*width) == Some(address) => {
                            *width += 1;
                        }
                        Some((start, width)) if address.checked_add(1) == Some(*start) => {
                            *start = address;
                            *width += 1;
                        }
                        _ => write_ranges.push((address, 1)),
                    }
                    true
                })
            } else {
                self.dma
                    .pull_byte(FDC_DMA_CHANNEL, &mut self.memory)
                    .is_some_and(|byte| {
                        self.floppy.as_mut().is_some_and(|floppy| {
                            floppy.write_sector_byte(
                                u16::from(request.transfer.cylinder),
                                request.transfer.head,
                                request.sector,
                                usize::from(request.offset),
                                byte,
                            )
                        })
                    })
            };
            let terminal_count = transferred && self.dma.at_terminal_count(FDC_DMA_CHANNEL);
            self.fdc.complete_dma_byte(fdc::DmaByteOutcome {
                transferred,
                terminal_count,
            });
        }
        for (address, width) in write_ranges {
            self.cpu.note_device_memory_write_range(address, width);
        }
    }

    fn device_rates(&mut self) -> DeviceRates {
        let programmed_wss = self.wss.output_frame_rate();
        DeviceRates {
            dsp_hz: self.sb16.timeline_rate_hz(),
            wss_hz: if self.wss_enabled && (self.wss.is_playing() || self.wss.autocal_active()) {
                u64::from(if programmed_wss > 0 {
                    programmed_wss
                } else {
                    WSS_AUTOCAL_FALLBACK_HZ
                })
            } else {
                0
            },
            cd_playing: self.ide.device().playback().playing,
            vga_dot_hz: self.vega.dot_clock_hz(),
        }
    }

    fn finish_tsc_advance(&mut self, before: u64, executed_clocks: u64) {
        let timeline_clocks = self.timeline.tsc_clocks().wrapping_sub(before);
        debug_assert!(
            timeline_clocks >= executed_clocks || self.timeline.now_ticks() == u64::MAX,
            "timeline TSC clocks must cover retired clocks before saturation"
        );
        // Retired clocks already reached CpuGsw::elapsed_clocks. Add only bus,
        // ISA, stall, and halted clocks. At master-time saturation the wrapping
        // difference cancels retired clocks beyond the last representable tick.
        self.cpu
            .advance_tsc(timeline_clocks.wrapping_sub(executed_clocks));
    }

    fn advance_master_time(&mut self, master_ticks: u64, io_stall: bool, executed_clocks: u64) {
        let tsc_before = self.timeline.tsc_clocks();
        let mut remaining = master_ticks;
        if remaining == 0 {
            let rates = self.device_rates();
            let advance = if io_stall {
                self.timeline.advance_io_stall_ticks(0, rates)
            } else {
                self.timeline.advance_master_ticks(0, rates)
            };
            self.apply_device_advance(advance);
            self.finish_tsc_advance(tsc_before, executed_clocks);
            return;
        }
        while remaining != 0 {
            let step = self
                .fdc
                .ticks_until_event(self.timeline.now_ticks())
                .map_or(remaining, |deadline| remaining.min(deadline));
            let rates = self.device_rates();
            let advance = if io_stall {
                self.timeline.advance_io_stall_ticks(step, rates)
            } else {
                self.timeline.advance_master_ticks(step, rates)
            };
            self.apply_device_advance(advance);
            remaining -= step;
        }
        self.finish_tsc_advance(tsc_before, executed_clocks);
    }

    pub(super) fn advance_cpu_work(&mut self, clocks: u64, executed_clocks: u64) {
        let master_ticks = self.timeline.master_ticks_for_cpu_clocks(clocks);
        let crosses_fdc_deadline = self
            .fdc
            .ticks_until_event(self.timeline.now_ticks())
            .is_some_and(|deadline| deadline <= master_ticks);
        if crosses_fdc_deadline {
            self.advance_master_time(master_ticks, false, executed_clocks);
        } else {
            let tsc_before = self.timeline.tsc_clocks();
            let rates = self.device_rates();
            let advance = self.timeline.advance_cpu_clocks(clocks, rates);
            self.apply_device_advance(advance);
            self.finish_tsc_advance(tsc_before, executed_clocks);
        }
        self.elapsed_clocks = self.elapsed_clocks.saturating_add(clocks);
    }

    pub(super) fn advance_halted_cpu_clocks(&mut self, clocks: u64) {
        let before = self.timeline.now_ticks();
        self.advance_cpu_work(clocks, 0);
        self.halted_ticks = self
            .halted_ticks
            .saturating_add(self.timeline.now_ticks().saturating_sub(before));
    }

    /// Advance global guest time by active-mode CPU clocks without executing CPU
    /// work. Used for wall shortfall, halted scanout, and focused device tests.
    pub fn advance_devices_clocks(&mut self, clocks: u64) {
        let master_ticks = self.timeline.master_ticks_for_cpu_clocks(clocks);
        self.advance_devices_ticks(master_ticks);
    }

    /// Advance devices and global guest time by a fixed master-tick duration
    /// without executing CPU work.
    pub fn advance_devices_ticks(&mut self, master_ticks: u64) {
        self.advance_master_time(master_ticks, false, 0);
    }

    pub(super) fn advance_halted_ticks(&mut self, master_ticks: u64) {
        let before = self.timeline.now_ticks();
        let cpu_clocks = self.timeline.cpu_clocks_for_master_ticks_ceil(master_ticks);
        self.advance_master_time(master_ticks, false, 0);
        self.elapsed_clocks = self.elapsed_clocks.saturating_add(cpu_clocks);
        self.halted_ticks = self
            .halted_ticks
            .saturating_add(self.timeline.now_ticks().saturating_sub(before));
    }

    #[cfg(test)]
    pub(super) fn advance_devices(&mut self, clocks: u64) {
        self.advance_devices_clocks(clocks);
    }

    /// Advance device time and the timeline by at most `clocks` without
    /// running the CPU, and return the clocks actually consumed.
    ///
    /// Contract: if the next VGA vertical-retrace START edge falls strictly
    /// inside the span, the advance stops AT that edge (the beam lands on the
    /// first dot of the retrace window, so a port 0x3DA read already returns
    /// bit 3 set) and the consumed count is returned; the caller tops up the
    /// remainder in further calls, typically granting the CPU a small execution
    /// quantum in between so a guest polling 0x3DA observes the window. With no
    /// intervening edge the full `clocks` is consumed.
    ///
    /// Why: a 16 ms wall-pacing top-up sweeps the beam across more than a whole
    /// mode-13h frame (14.3 ms) with zero instructions executing, so a guest
    /// double-polling 0x3DA for the 2-scanline vretrace window deterministically
    /// missed every window that opened and closed inside a top-up (measured
    /// catch rate 12.8 percent at a 1/8 execution share). Stopping at each start
    /// edge makes every window observable.
    ///
    /// Termination guarantee: the returned count is >= 1 whenever `clocks` >= 1.
    /// When the beam already sits on the edge or inside the retrace window, the
    /// next start edge is a full frame ahead (see
    /// `Vga::dots_until_vretrace_start`), so back-to-back calls always make
    /// progress and a caller looping `remaining -= consumed` terminates. The
    /// stop honors the fractional `vga_dots` accumulator, overshooting the edge
    /// by at most a few dots (well inside the ~1600-dot window). One caveat: a
    /// 1-ulp rounding mismatch in the dots-to-clocks conversion could in
    /// principle land the beam a dot short of the edge; the caller's peek
    /// executes instructions whose own device advance carries the beam into the
    /// window, so the contract holds for observers either way.
    pub fn advance_wall_shortfall(&mut self, clocks: u64) -> u64 {
        let consume = match self.clocks_to_vretrace_start() {
            Some(edge_clocks) => edge_clocks.min(clocks),
            None => clocks,
        };
        self.advance_devices_clocks(consume);
        consume
    }

    /// Master-tick form of `advance_wall_shortfall`. This is the pacing seam:
    /// a live CPU-mode switch cannot change the unit of its budget or result.
    pub fn advance_wall_shortfall_ticks(&mut self, master_ticks: u64) -> u64 {
        let consume = match self.master_ticks_to_vretrace_start() {
            Some(edge_ticks) => edge_ticks.max(1).min(master_ticks),
            None => master_ticks,
        };
        self.advance_devices_ticks(consume);
        consume
    }

    /// Clocks of device time until the VGA beam reaches the next vertical-
    /// retrace start edge, converted from beam dots through the timeline phase.
    /// Delivering the returned count to `advance_devices_clocks` moves the beam
    /// onto (or a dot or two past) the edge. `None` means the CRTC has no usable
    /// frame geometry.
    fn clocks_to_vretrace_start(&self) -> Option<u64> {
        let edge_dots = self.vega.dots_until_vretrace_start()?;
        self.timeline.cpu_clocks_until(
            timeline::DeviceClock::Vga,
            edge_dots,
            self.vega.dot_clock_hz(),
        )
    }

    fn master_ticks_to_vretrace_start(&self) -> Option<u64> {
        let edge_dots = self.vega.dots_until_vretrace_start()?;
        self.timeline.master_ticks_until(
            timeline::DeviceClock::Vga,
            edge_dots,
            self.vega.dot_clock_hz(),
        )
    }

    /// Drive a PIT counter's GATE line. The PC ties GATE0/GATE1 high; the sound
    /// path wires GATE2 from port 0x61. Exposed so the GATE-triggered modes
    /// have a caller outside tests.
    pub fn set_timer_gate(&mut self, channel: usize, level: bool) {
        self.pit.set_gate(channel, level);
    }

    /// Input CLK pulses until channel 0 produces its next OUT rising edge, or None
    /// if the counter cannot fire from its current state. Used by HLT fast-forward.
    pub fn clocks_until_timer0_irq(&self) -> Option<u64> {
        self.pit.clocks_until_channel0_irq()
    }

    /// CPU clocks to advance while halted so the next wake-capable IRQ lands, or
    /// None if nothing can wake the CPU (so HLT is a genuine halt). A halted guest
    /// is woken by timer, audio, storage, RTC, keyboard, serial, or printer
    /// completion.
    /// Each is considered only when unmasked and deliverable. The result is the
    /// soonest applicable wake, clamped to the deadline and to at least one clock
    /// so the run loop always makes progress.
    pub(super) fn next_timer_wake(&self, deadline_ticks: u64) -> Option<u64> {
        if !self.cpu.interrupts_enabled() {
            return None;
        }
        let remaining_ticks = deadline_ticks.saturating_sub(self.timeline.now_ticks());
        if remaining_ticks == 0 {
            return None;
        }
        let remaining = self
            .timeline
            .cpu_clocks_for_master_ticks_ceil(remaining_ticks)
            .max(1);
        let pit_wake = if self.pic.irq0_unmasked() {
            self.clocks_until_timer0_irq().and_then(|pit_delta| {
                self.timeline.cpu_clocks_until(
                    timeline::DeviceClock::Pit,
                    pit_delta,
                    u64::from(PIT_INPUT_HZ),
                )
            })
        } else {
            None
        };
        let dsp_wake = self.sb16.irq_deadline().and_then(|deadline| {
            if self.pic.deliverable(deadline.line()) {
                self.timeline.cpu_clocks_until(
                    timeline::DeviceClock::Dsp,
                    deadline.frames(),
                    deadline.rate_hz(),
                )
            } else {
                None
            }
        });
        // The AD1848 / WSS terminal-count wake, on the codec's own (config) IRQ
        // line. The codec drains one Current Count per output frame, so its IRQ
        // estimator is fed the frame rate directly (no byte/word-counter scaling
        // like the SB16's). Considered only when that line can actually deliver
        // (`deliverable` also requires the master IR2 cascade pin for a slave line
        // 9/10/11) and the codec is enabled; frames_until_next_irq also returns
        // None when IEN is clear (the underflow then sets only the sticky Status
        // bit, no pin edge).
        let wss_wake = if self.wss_enabled && self.pic.deliverable(self.wss.irq()) {
            self.wss.frames_until_next_irq().and_then(|frames| {
                self.timeline.cpu_clocks_until(
                    timeline::DeviceClock::Wss,
                    frames,
                    u64::from(self.wss.output_frame_rate()),
                )
            })
        } else {
            None
        };
        let ata_wake = if self.pic.deliverable(ata::PRIMARY_IRQ)
            && self.ata.as_ref().is_some_and(ata::AtaDisk::irq_enabled)
        {
            self.next_primary_ata_irq_deadline()
                .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1))
        } else {
            None
        };
        let atapi_wake = if self.pic.deliverable(ide::SECONDARY_IRQ) && self.ide.irq_enabled() {
            self.ide
                .ticks_until_irq()
                .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1))
        } else {
            None
        };
        let keyboard_wake = if (self.pic.deliverable(1) && self.keyboard.irq1_enabled())
            || (self.pic.deliverable(12) && self.keyboard.irq12_enabled())
        {
            self.keyboard
                .ticks_until_irq()
                .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1))
        } else {
            None
        };
        let mpu_wake = self
            .pic
            .deliverable(9)
            .then(|| {
                self.wavetable_mpu
                    .ticks_until_event()
                    .into_iter()
                    .chain(self.midi_mpu.ticks_until_event())
                    .min()
            })
            .flatten()
            .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        let rtc_wake = self
            .pic
            .deliverable(8)
            .then(|| self.next_rtc_irq_deadline())
            .flatten()
            .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        let serial_wake = self
            .pic
            .deliverable(4)
            .then(|| self.serial.ticks_until_irq())
            .flatten()
            .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        let serial2_wake = self
            .pic
            .deliverable(3)
            .then(|| self.serial2.ticks_until_irq())
            .flatten()
            .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        let lpt_wake = self
            .pic
            .deliverable(7)
            .then(|| self.lpt.ticks_until_irq())
            .flatten()
            .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        let lpt2_wake = self
            .pic
            .deliverable(5)
            .then(|| self.lpt2.ticks_until_irq())
            .flatten()
            .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        let fdc_wake = self
            .pic
            .deliverable(6)
            .then(|| self.fdc.ticks_until_event(self.timeline.now_ticks()))
            .flatten()
            .map(|ticks| self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        // The sooner of whichever wakes apply; None only when none can fire.
        let wake = [
            pit_wake,
            dsp_wake,
            wss_wake,
            ata_wake,
            atapi_wake,
            keyboard_wake,
            mpu_wake,
            rtc_wake,
            serial_wake,
            serial2_wake,
            lpt_wake,
            lpt2_wake,
            fdc_wake,
        ]
        .into_iter()
        .flatten()
        .min()?;
        Some(wake.max(1).min(remaining))
    }

    fn next_ata_deadline(&self) -> Option<u64> {
        self.ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_completion)
            .into_iter()
            .chain(self.bmide.ticks_until_completion())
            .chain(self.ide.ticks_until_completion())
            .min()
    }

    fn next_primary_ata_irq_deadline(&self) -> Option<u64> {
        self.ata
            .as_ref()
            .and_then(ata::AtaDisk::ticks_until_irq)
            .into_iter()
            .chain(self.bmide.ticks_until_completion())
            .min()
    }

    fn next_rtc_irq_deadline(&self) -> Option<u64> {
        let periodic = self.rtc.ticks_until_periodic_irq();
        let update_or_alarm = self.rtc.seconds_until_irq().and_then(|seconds| {
            self.timeline
                .master_ticks_until(timeline::DeviceClock::Rtc, seconds, 1)
        });
        periodic.into_iter().chain(update_or_alarm).min()
    }

    pub(super) fn next_timed_io_deadline(&self) -> Option<u64> {
        self.serial
            .ticks_until_event()
            .into_iter()
            .chain(self.serial2.ticks_until_event())
            .chain(self.lpt.ticks_until_event())
            .chain(self.lpt2.ticks_until_event())
            .chain(self.fdc.ticks_until_event(self.timeline.now_ticks()))
            .chain(self.keyboard.ticks_until_event())
            .chain(self.wavetable_mpu.ticks_until_event())
            .chain(self.midi_mpu.ticks_until_event())
            .min()
    }

    /// Whether a consumer that can make CPU-batch GRANULARITY guest-visible is
    /// currently active. This is the admission test for the Accurate class's fine
    /// (DAC-period) batch fallback; see `event_batch_cap`.
    ///
    /// Every term is a live bool or one Option test: no rational conversion, no
    /// device query that walks a queue. It runs once per batch, on the same hot
    /// path the cap itself sits on.
    ///
    /// The terms, and what each one protects:
    /// - OPL timers running. AdLib detection starts timer 1 (one 80 us step),
    ///   runs a PURE-COMPUTE delay loop, then reads the status byte once. Nothing
    ///   in that loop ends a batch, and the Accurate class reads the LIVE status
    ///   byte (the `predicted_opl_status` peek is Approximate-only), so if the
    ///   whole loop fits inside one batch the read reports the pre-delay flags and
    ///   the guest concludes there is no card. Same failure the Approximate class
    ///   hit before the peek existed.
    /// - Speaker data enable. While the membrane is driven, port 0x61 bit 5
    ///   (channel-2 OUT) is read live on this class too, so batch length bounds
    ///   how stale a speaker-timing poll can be.
    /// - DSP output clock, WSS playback/autocal, Red Book CD playback. The
    ///   DMA-fed producers; a fine batch keeps the DMA current-count a guest can
    ///   poll for its play position moving one frame at a time.
    /// - A recent PIT counter access (`pit_observer_fine_until`). Not audio at
    ///   all, and no longer load-bearing for counter VALUES: `Counter::count_after`
    ///   now peeks the counting element at the in-batch instant of every 0x40-0x43
    ///   access, so a latch-compute-latch measurement is exact at any batch grain.
    ///   The window is kept as the one case the peek declines -- a BCD-programmed
    ///   counter, where `count_after` returns None and the read falls back to the
    ///   live (batch-start) field, exactly as the 0x61 arm falls back for a BCD
    ///   `out_after`. Removing the term is a measurable batch-length change and
    ///   belongs to its own slice, not to a fidelity fix.
    ///
    /// NOT a term, deliberately: the sample producers' own fidelity. The original
    /// cap (2026-06-21) existed so "the per-clock fine-samplers ... never alias" --
    /// at that time the speaker sampled channel-2 OUT once per advance. Five days
    /// later the speaker was rebuilt to integrate PIT transitions at their exact
    /// sub-sample instants, and the timeline gives every producer a persistent
    /// rate phase, so the DSP/WSS/CD/speaker frame counts and sample values are
    /// split-invariant. The anti-aliasing rationale did not survive; what is left
    /// is the observation-point list above.
    ///
    /// ALSO not a term, and the reason this is a gate and not a wider one: the
    /// Margo blit engine. Its BUSY poll is the sharpest batch-granularity
    /// consumer in the machine, but a gate cannot serve it -- the arming MMIO
    /// write happens INSIDE the batch whose cap was already chosen, so the gate
    /// would always be read too early. It is handled as a real deadline in
    /// `event_batch_cap` instead.
    fn fine_batch_grain_required(&self) -> bool {
        self.opl.timers_running()
            || self.speaker.data_enabled()
            || self.sb16.is_producing()
            || (self.wss_enabled && (self.wss.is_playing() || self.wss.autocal_active()))
            || self.ide.device().playback().playing
            || self.timeline.now_ticks() < self.pit_observer_fine_until
    }

    /// CPU clocks until the next due device event.
    ///
    /// Interrupts are serviced at batch entry and devices advance at batch end,
    /// so known timer, audio, MIDI, storage, RTC, keyboard, serial, and printer edges
    /// shorten the batch to the first causal CPU clock in every CPU mode. Every
    /// mode has a 1 ms fallback; the 386 (Accurate) modes drop to a finer
    /// DAC-period fallback while `fine_batch_grain_required` says a consumer can
    /// see the difference. A known edge may be earlier than either fallback.
    ///
    /// Why the fine fallback is gated rather than unconditional: at 22 MHz a
    /// DAC period is ~500 clocks, so an idle 386 guest paid a floor of ~44,000
    /// batch iterations per guest second -- each one a fresh pull-scan of the
    /// device deadline queries below -- to protect consumers that were not
    /// running. Gating it leaves the protected cases exactly as fine as before
    /// and gives the idle case the same 1 ms ceiling the Approximate class uses.
    ///
    /// CAVEAT CLOSED (was: time-derived port reads with no in-batch peek). The PIT
    /// counter latch on 0x40/0x42 used to return the counting element as of BATCH
    /// START, so the FIRST latch of a "latch, compute, latch" measurement -- the one
    /// that lands in a batch nothing had shortened yet -- could sit up to a full
    /// coarse batch (1 ms) early, and the PIT-observer window only covered the
    /// touches after it. The principled fix named here has since been taken:
    /// `Counter::count_after` peeks the counting element the way `Pit::out_after`
    /// peeks OUT, and `MachineBus` takes it on every 0x40-0x43 access in BOTH timing
    /// classes (it changes the value read, never whether the batch ends). A
    /// mid-batch latch is now exactly what a real `advance_devices` of the same
    /// clock total would produce, pinned by
    /// `a_mid_batch_counter_latch_matches_a_real_advance_devices_of_the_same_clocks`,
    /// so the batch grain no longer bounds counter-read accuracy at all. What is
    /// left is a BCD-programmed counter, where the peek declines (as `out_after`
    /// does, and for the same reason: no PC software clocks the PIT in BCD) and the
    /// observer window still holds the grain fine.
    ///
    /// PIT channel 1 is EXCLUDED deliberately: the power-on DRAM-refresh
    /// heartbeat (mode 2, reload 18, ~15 us) runs forever, so its term would
    /// bind every batch below the 386 fallback and cancel this path
    /// outright. Its OUT is only guest-visible through a port 0x61 read, which
    /// ends the batch anyway. PIC masking is likewise ignored on purpose: an
    /// edge on a masked line latches IRR at the same advance either way, so the
    /// per-batch mask query buys no alignment.
    ///
    /// Timeline phase is included in every conversion, so splitting execution
    /// into different batches does not move an event deadline.
    pub(super) fn event_batch_cap(&self, remaining: u64) -> u64 {
        let edge_ticks = self
            .vega_edge_ticks()
            .into_iter()
            .chain(self.next_cacheable_edge_ticks())
            .min();
        self.compose_batch_cap(edge_ticks, remaining)
    }

    /// The device-free half of the cap: the mode-class fallback grain.
    ///
    /// Split out of `event_batch_cap` so the cached path can keep paying it per
    /// batch. It is two integer divisions and, on the Accurate class, the
    /// `fine_batch_grain_required` gate -- all live-field tests, nothing that
    /// walks a device queue -- so there is nothing here worth caching, and the
    /// gate is LEVEL state that a cache would have to invalidate on every
    /// consumer edge anyway.
    fn batch_grain_fallback(&self) -> u64 {
        let clock_hz = self.active_mode.clock_hz();
        // Order matters for cost: the class test short-circuits, so the
        // Approximate (486/586) modes never run the gate at all.
        if self.active_mode.uses_approximate_timing() || !self.fine_batch_grain_required() {
            clock_hz / 1000
        } else {
            clock_hz / u64::from(DAC_HZ)
        }
        .max(1)
    }

    /// Fold a master-tick edge delta and the fallback grain into the CPU-clock
    /// cap. One conversion for the whole scan rather than one per term: `ceil`
    /// is monotone and `.max(1)` commutes with `min`, so
    /// `min_i(ceil(t_i).max(1)) == ceil(min_i t_i).max(1)` exactly. This is a
    /// value-preserving rewrite of the old per-term `cap.min(...)` chain, not an
    /// approximation.
    fn compose_batch_cap(&self, edge_ticks: Option<u64>, remaining: u64) -> u64 {
        let mut cap = self.batch_grain_fallback();
        if let Some(ticks) = edge_ticks {
            cap = cap.min(self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        }
        cap.max(1).min(remaining)
    }

    /// The two Margo terms, deliberately kept OUT of the deadline cache.
    ///
    /// Both are armed by a memory write, and Margo MMIO writes do not all set
    /// `io_touched`: only the blit-arming edge does (see
    /// `MachineBus::write_memory_byte_recorded`), while a DISPLAY_START register
    /// write returns `VideoWrite::Accepted` like any framebuffer store. There is
    /// therefore no cheap seam that could invalidate a cached Margo deadline, and
    /// caching one would violate the "never later than a fresh scan" contract.
    /// They cost a bool test and a `u64` field read when idle, which is what the
    /// cache was buying elsewhere anyway.
    ///
    /// The blit term is a real device deadline whose absence was load-bearing in
    /// a way nothing recorded: STATUS.BUSY lives in MMIO, so the BIOS's
    /// `margo_wait` spin (`mov eax,[fs:MARGO_MMIO+8]` / `test al,1` / `jnz`,
    /// izbios-lfb.inc) cannot break its own batch. BUSY therefore stayed set for
    /// the WHOLE remaining batch however short the operation was, and the guest
    /// paid a full batch cap per wait -- the DAC-period fallback was the only
    /// thing keeping that bill down on the 386 tier, and raising the fallback to
    /// 1 ms alone stopped POST from clearing the RAM test inside its 20M-cycle
    /// budget. With the deadline here the wait costs exactly the busy time the
    /// engine modeled (a glyph expand is ~740 ns, ~16 clocks at 22 MHz). The
    /// arming write ends its own batch, so this term is always seen from a batch
    /// that starts with the engine busy.
    fn vega_edge_ticks(&self) -> Option<u64> {
        let blit_busy_ns = self.vega.blitter_busy_ns();
        let blit = if blit_busy_ns > 0 {
            self.timeline.master_ticks_until(
                timeline::DeviceClock::MargoNs,
                blit_busy_ns,
                timeline::NANOSECOND_HZ,
            )
        } else {
            None
        };
        let display_start = if self.vega.display_start_pending() {
            self.timeline.master_ticks_until(
                timeline::DeviceClock::MargoFrame,
                1,
                timeline::MARGO_FRAME_HZ,
            )
        } else {
            None
        };
        blit.into_iter().chain(display_start).min()
    }

    /// Master ticks until the earliest armed PIT / audio / storage / RTC /
    /// timed-I/O edge. This is the ~15-query pull-scan the deadline cache exists
    /// to hoist off the per-batch path; every term here is an ABSOLUTE instant in
    /// disguise (see `event_batch_cap_cached`), which is what makes caching it
    /// sound.
    fn next_cacheable_edge_ticks(&self) -> Option<u64> {
        // Next PIT OUT rising edge: channel 0 feeds IRQ0, channel 2 the
        // speaker/GATE timing games poll. (Channel 1: see above.)
        let pit = [0usize, 2].into_iter().filter_map(|channel| {
            self.pit.clocks_until_out_rise(channel).and_then(|ticks| {
                self.timeline.master_ticks_until(
                    timeline::DeviceClock::Pit,
                    ticks,
                    u64::from(PIT_INPUT_HZ),
                )
            })
        });
        // Next audio block IRQ edge. Both counters are expressed in output
        // frames, including stereo DMA-unit accounting inside the DSP.
        let dsp = self.sb16.irq_deadline().and_then(|deadline| {
            self.timeline.master_ticks_until(
                timeline::DeviceClock::Dsp,
                deadline.frames(),
                deadline.rate_hz(),
            )
        });
        let wss = if self.wss_enabled {
            self.wss.frames_until_next_irq().and_then(|frames| {
                self.timeline.master_ticks_until(
                    timeline::DeviceClock::Wss,
                    frames,
                    u64::from(self.wss.output_frame_rate()),
                )
            })
        } else {
            None
        };
        pit.chain(dsp)
            .chain(wss)
            .chain(self.next_ata_deadline())
            .chain(self.next_rtc_irq_deadline())
            .chain(self.next_timed_io_deadline())
            .min()
    }

    /// Drop the cached device edge. Correct at any time; the only cost of an
    /// unnecessary call is the pull-scan the next batch entry then runs.
    pub(crate) fn invalidate_device_edge_cache(&mut self) {
        self.device_edge_cache = DeviceEdgeCache::Stale;
    }

    /// Test seam: the cache's raw state. A cap comparison cannot see every
    /// invalidation -- the mode-class fallback is 1 ms of guest time, so a device
    /// edge further out than that is invisible in the cap however wrong the cache
    /// is -- so the invalidation-site tests assert on this directly.
    #[cfg(test)]
    pub(super) fn device_edge_cache_state(&self) -> DeviceEdgeCache {
        self.device_edge_cache
    }

    /// How many batch entries ran, and how many of those had to re-scan. Host
    /// instrumentation for the deadline cache; never an emulation input.
    pub fn device_edge_cache_counts(&self) -> (u64, u64) {
        (self.device_edge_batches, self.device_edge_scans)
    }

    /// `event_batch_cap` with the device pull-scan served from a maintained
    /// next-deadline cache. This is the run loop's entry point;
    /// `event_batch_cap` stays the fresh-scan reference and the oracle below.
    ///
    /// PUSH, not pull. Concept borrowed from 86Box's `src/timer.c`, which keeps a
    /// sorted timer list behind one global `timer_target` and compares against it
    /// once per instruction instead of asking every device how far away its next
    /// event is (86Box is GPL-2 and study-only for this tree: the idea is
    /// attributed here, no code was copied). The adaptation to this codebase is
    /// deliberately coarser than a sorted list -- one cached MINIMUM rather than a
    /// full ordering -- because the batch loop only ever needs the earliest edge,
    /// and a single `Option<u64>` needs no per-device arm/disarm plumbing.
    ///
    /// The cache holds an ABSOLUTE master tick, which is sound because every term
    /// in `next_cacheable_edge_ticks` is an absolute instant expressed as a delta:
    /// a `RatePhase` term is `ceil((events * MASTER_HZ - remainder) / rate)`, and
    /// advancing `d` ticks lowers that numerator by exactly `d * rate`, so
    /// `now + ticks_until` does not move. The master-tick terms (ATA, RTC
    /// periodic, FDC, serial, LPT, keyboard, MPU) are stored deadlines already.
    ///
    /// INVALIDATION IS CONSERVATIVE by construction: returning an edge EARLIER
    /// than a fresh scan only shortens a batch (always safe -- a shorter batch is
    /// strictly more observable), while returning a LATER one would let a device
    /// edge land mid-batch, so every path that can move a device schedule drops
    /// the cache. Those paths are:
    ///   * batch entry, when the cached edge is already due -- the device fired
    ///     inside its own advance and rearmed or went idle;
    ///   * batch end, whenever the batch was not provably quiet: any guest port
    ///     access (`io_touched`, which the Margo blit-arming MMIO write also
    ///     sets), a bus-side DMA write to guest RAM, any serviced HLE / mode /
    ///     Toka / BIOS32 / unittester step, or a HLT fast-forward;
    ///   * `run_until_tick` entry, which covers EVERY host-side mutator in one
    ///     place -- keyboard/mouse/joystick injection, media mount and eject,
    ///     CMOS and RTC seeding, audio rendering, canonical-state restore -- since
    ///     those can only run between run calls on the machine thread. The
    ///     individually risky ones also invalidate at their own site, so the
    ///     property does not silently depend on that scheduling fact.
    ///
    /// The `debug_assert` compares every cached answer against the fresh scan, so
    /// the whole test suite is a continuous audit of the invalidation list.
    pub(super) fn event_batch_cap_cached(&mut self, remaining: u64) -> u64 {
        self.device_edge_batches = self.device_edge_batches.wrapping_add(1);
        let now = self.timeline.now_ticks();
        if let DeviceEdgeCache::Due(at) = self.device_edge_cache
            && at <= now
        {
            self.device_edge_cache = DeviceEdgeCache::Stale;
        }
        if self.device_edge_cache == DeviceEdgeCache::Stale {
            self.device_edge_scans = self.device_edge_scans.wrapping_add(1);
            self.device_edge_cache = match self.next_cacheable_edge_ticks() {
                Some(ticks) => DeviceEdgeCache::Due(now.saturating_add(ticks)),
                None => DeviceEdgeCache::Idle,
            };
        }
        // The common case from here is one compare (above) and one subtraction.
        let cached = match self.device_edge_cache {
            DeviceEdgeCache::Due(at) => Some(at - now),
            DeviceEdgeCache::Idle | DeviceEdgeCache::Stale => None,
        };
        let edge_ticks = self.vega_edge_ticks().into_iter().chain(cached).min();
        let cap = self.compose_batch_cap(edge_ticks, remaining);
        debug_assert_eq!(
            cap,
            self.event_batch_cap(remaining),
            "device-edge cache disagreed with a fresh pull-scan (cache {:?}, now {now})",
            self.device_edge_cache,
        );
        cap
    }
}

/// The maintained next-device-edge deadline behind `event_batch_cap_cached`.
///
/// `Idle` and `Due` are both VALID cached answers; only `Stale` forces a scan.
/// Caching "nothing is armed" is what makes an idle guest cheap, and it carries
/// the same invalidation obligation as a cached instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeviceEdgeCache {
    /// Nothing is cached; the next batch entry runs the full pull-scan.
    Stale,
    /// The scan found no armed device edge at all.
    Idle,
    /// Absolute master tick of the earliest armed device edge.
    Due(u64),
}

impl MachineBus<'_> {
    pub(super) fn guest_tick_now(&self) -> u64 {
        self.master_ticks_at_batch_start.saturating_add(
            self.timeline_at_batch_start
                .master_ticks_for_cpu_clocks(self.in_batch_clocks()),
        )
    }

    /// Predict the VGA beam position at the current point in a CPU batch without
    /// mutating device state. The core-clock total includes completed runs and
    /// prior instructions in the current run. Bus clocks use the batch-entry
    /// timing ratio and fractional carry, matching the later batch-end advance.
    ///
    /// Fetch and data clocks for the in-flight instruction may be only partly
    /// recorded, so the prediction is monotonic and cannot exceed the final
    /// batch total. Only beam position is predicted. Frame-boundary effects stay
    /// in `advance_devices` at batch end.
    pub(super) fn predicted_beam(&self) -> u64 {
        let in_batch_clocks = self.in_batch_clocks();
        let (_, whole_dots) = self
            .timeline_at_batch_start
            .preview_cpu_clocks(in_batch_clocks, self.vega.dot_clock_hz());
        let frame = self.vega.frame_dots();
        if frame == 0 {
            return self.beam_at_batch_start; // guard: un-programmed CRTC, mirrors Vga::advance
        }
        (self.beam_at_batch_start + whole_dots) % frame
    }

    /// Whole VGA dots between the current in-batch instant and a projected
    /// absolute in-batch clock total.
    #[cfg(feature = "jit")]
    pub(super) fn poll_project_dot_advance(&self, candidate_clocks: u64) -> Option<u64> {
        let current_clocks = self.in_batch_clocks();
        if candidate_clocks < current_clocks {
            return None;
        }
        let (_, current_dots) = self
            .timeline_at_batch_start
            .preview_cpu_clocks(current_clocks, self.vega.dot_clock_hz());
        let (_, candidate_dots) = self
            .timeline_at_batch_start
            .preview_cpu_clocks(candidate_clocks, self.vega.dot_clock_hz());
        candidate_dots.checked_sub(current_dots)
    }

    /// The OPL status byte at the current point in the batch, without stepping
    /// the chip. The lazy-path analogue of `predicted_beam`, and it exists for
    /// the same reason: in the Approximate class devices only advance at batch
    /// end, so a status read taken mid-batch would otherwise report the state
    /// the chip had when the batch STARTED.
    ///
    /// That is not a nicety. AdLib detection starts timer 1 (one 80 us step),
    /// runs a fixed delay loop, then reads status ONCE. The delay loop is pure
    /// computation, so it never ends the batch, and the read used to see the
    /// pre-delay flags and conclude no card was present -- which is why AdLib
    /// music was silent on 486 and 586 while the exact-timing 386 modes, whose
    /// devices advance per instruction, played it fine.
    ///
    /// Unlike `predicted_beam` this folds in `isa_io_clocks`. The batch-end
    /// advance adds that accrual to the batch total, and it is charged ONLY on
    /// OPL polls, so including it here is what makes the peek agree with the
    /// advance that follows -- and excluding it from the beam peek stays right
    /// for the same reason.
    ///
    /// With nothing pending this is exactly `OplChip::status()`: `expired_after`
    /// with a zero elapsed reduces to the live `expired` flag, because `advance`
    /// leaves `accumulated_us` below one step.
    /// Returns the predicted byte and the microseconds it was predicted at, so
    /// the OPL trace can record what the read actually saw.
    pub(super) fn predicted_opl_status(&self) -> (u8, u64) {
        let clocks = self.in_batch_clocks().saturating_add(*self.isa_io_clocks);
        let micros = self.timeline_at_batch_start.preview_microseconds(clocks);
        (self.opl.status_after(micros), micros)
    }

    /// Batch-scoped CPU clocks elapsed so far. Beam and PIT predictions share
    /// this conversion so they use the same core total, bus scaling, and carry.
    fn in_batch_clocks(&self) -> u64 {
        let in_batch_bus_clocks = self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start;
        let scaled = in_batch_bus_clocks * u64::from(self.bus_num_at_batch_start)
            + self.bus_rem_at_batch_start;
        let scaled_bus_clocks = scaled / u64::from(self.bus_den_at_batch_start);
        self.prior_runs_core_clocks + self.core_clocks_so_far + scaled_bus_clocks
    }

    /// Elapsed PIT input clocks at the current point in the batch without
    /// mutating `pit_clocks`. Compute this once when reading several channels.
    ///
    /// Converts the shared in-batch clock total `T` (`in_batch_clocks`, the same
    /// T `predicted_beam` peeks with) into elapsed PIT input clocks by calling
    /// the SAME `advance_fractional` function the real `advance_devices` PIT
    /// step calls, with `pit_clocks_at_batch_start` standing in for the live
    /// accumulator and `pit_per_clock_at_batch_start` for the live rate. NOT
    /// `predict_dots_core` with PIT_INPUT_HZ standing in for the dot clock:
    /// that formula's `clocks * rate_hz * inv_clock` factoring floor-diverges
    /// from the real advance's pre-divided `clocks * pit_per_clock` product at
    /// the IEEE-f64 level (see `advance_fractional`'s doc comment), which would
    /// let a lazy read report an OUT level one PIT clock ahead of or behind
    /// what batch end establishes. `advance_devices` only runs at batch end /
    /// wake step, never mid-batch, so `pit_clocks_at_batch_start` IS the live
    /// `pit_clocks` value the real call will start folding T's clocks into: no
    /// time travel, this predicts exactly what a real `advance_devices` at T
    /// followed by a read would produce.
    /// Record that the guest just touched a PIT counter, so the Accurate class
    /// keeps its fine batch grain for a while (see
    /// `Machine::fine_batch_grain_required` and `pit_observer_fine_until`).
    ///
    /// Armed by every access to 0x40-0x43, read or write. Since
    /// `Counter::count_after` peeks the counting element at the access instant, this
    /// no longer protects the latched VALUE (which is exact at any grain); it covers
    /// the BCD counters that peek declines for. The window is generous (`PIT_OBSERVER_FINE_WINDOW_MS`)
    /// because the whole point is to cover the "compute" leg between two
    /// latches; it is charged from batch start rather than the exact in-flight
    /// instruction because a few microseconds at this scale cannot matter and
    /// `master_ticks_at_batch_start` is already loaded here.
    pub(super) fn note_pit_observer(&mut self) {
        *self.pit_observer_fine_until = self
            .master_ticks_at_batch_start
            .saturating_add(PIT_OBSERVER_FINE_WINDOW_TICKS);
    }

    pub(super) fn elapsed_pit_clocks(&self) -> u64 {
        self.timeline_at_batch_start
            .preview_cpu_clocks(self.in_batch_clocks(), self.vega.dot_clock_hz())
            .0
    }

    /// Peek `channel`'s live PIT OUT level mid-batch without stepping
    /// `pit` or mutating `pit_clocks`. `None` when the channel's counter is BCD
    /// (see `Counter::out_after` via `Pit::out_after`); the caller falls back to
    /// a real read in that case. Convenience wrapper over `elapsed_pit_clocks`
    /// for a single-channel peek (tests, and any future single-channel lazy
    /// port); the production 0x61 read arm needs both channels and calls
    /// `elapsed_pit_clocks` directly instead, per the note above.
    #[cfg(test)]
    pub(super) fn predicted_pit_out(&self, channel: usize) -> Option<bool> {
        self.pit.out_after(channel, self.elapsed_pit_clocks())
    }
}
