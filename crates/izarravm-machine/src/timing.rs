// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Shared machine and device timing.

use super::*;

pub const OPL_NATIVE_HZ: u32 = 49_716;
pub const DAC_HZ: u32 = 44_100;
pub const PIT_INPUT_HZ: u32 = 1_193_182;
pub const WSS_AUTOCAL_FALLBACK_HZ: u32 = 8000;

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

        // The DSP reset-settle countdown advances with emulated time so a
        // detection routine's delay loop sees 0xAA become available. No lazy
        // twin yet; routed through the shared formula anyway so the last
        // hand-synchronized copy of its arithmetic is gone.
        self.dsp.advance_micros(advance.microseconds);

        // DMA playback is clock-driven: accrue DSP sample phases per CPU clock
        // and, for each whole sample, advance the block and buffer the rendered
        // stereo frame onto the DSP ring. The block-completion IRQ that
        // render_frame edges is forwarded to the PIC here, so playback timing and
        // IRQ5 no longer depend on the host frontend pulling audio. The host path
        // (render_dsp_audio) only drains what the clock already produced.
        //
        // The run loop caps normal CPU batches at the next programmed block edge,
        // then forwards the DSP latch below so the guest can acknowledge each IRQ.
        // An explicit device-only advance can span several edges while the CPU is
        // unable to acknowledge; those requests coalesce in the device latch.
        // The mixer's SB Pro stereo bit (0x0E bit1) selects 8-bit byte
        // interleaving, which halves the per-channel frame rate; sample it before
        // computing the rate the DSP frames at.
        self.dsp.set_sbpro_stereo(self.mixer.sbpro_stereo());
        let rate = self.dsp.output_frame_rate();
        // The mixer selects the IRQ line and DMA channels (registers 0x80/0x81);
        // read them before the borrow-splitting loop below so the loop's
        // `let Machine { dsp, dma, memory, .. } = self;` shape is untouched.
        let irq_line = self.mixer.selected_irq();
        let dma8 = self.mixer.selected_dma_8();
        let dma16 = self.mixer.selected_dma_16();
        if self.dsp.needs_output_tick() && rate > 0 {
            let n = advance.dsp_frames as usize;
            let Machine {
                dsp, dma, memory, ..
            } = self;
            let is16 = dsp.is_16bit();
            let ch = if is16 { dma16 } else { dma8 };
            // HLE batches the elapsed output frames but keeps DMA reads at the
            // frame that consumes them. This avoids copying stale data ahead of
            // the guest's block IRQ and lets a dry DMA source stop the batch.
            if is16 {
                dsp.tick_n_samples(n, || None, || dma.read_word(ch, memory));
            } else {
                dsp.tick_n_samples(n, || dma.read_byte(ch, memory), || None);
            }
            if dsp.take_irq() {
                let is_16bit = dsp.is_16bit();
                self.mixer.set_irq_status(is_16bit);
                self.pic.request(irq_line);
            }
        }
        // Forward a pending DSP interrupt with playback idle too: the 0xF2
        // IRQ-request command raises it without a transfer running (drivers
        // probe their IRQ wiring that way). The real chip asserts the line
        // regardless. take_irq is a test-and-clear latch, so this never
        // double-delivers an edge the per-tick forward above already took.
        if self.dsp.take_irq() {
            let is_16bit = self.dsp.is_16bit();
            self.mixer.set_irq_status(is_16bit);
            self.pic.request(irq_line);
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
            let wss_dma = self.wss_dma;
            let wss_irq = self.wss_irq;
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
        self.dsp.set_sbpro_stereo(self.mixer.sbpro_stereo());
        let programmed_wss = self.wss.output_frame_rate();
        DeviceRates {
            dsp_hz: if self.dsp.needs_output_tick() {
                u64::from(self.dsp.output_frame_rate())
            } else {
                0
            },
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

    /// Advance the DSP reset-settle clock by `micros` microseconds. The run loop
    /// drives this from CPU clocks in advance_devices; this exposes it directly
    /// so a reset-detection golden can settle the DSP without running the CPU.
    pub fn advance_dsp_micros(&mut self, micros: u64) {
        self.dsp.advance_micros(micros);
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
        let dsp_wake = if self.pic.deliverable(self.mixer.selected_irq()) {
            self.dsp.frames_until_next_irq().and_then(|frames| {
                self.timeline.cpu_clocks_until(
                    timeline::DeviceClock::Dsp,
                    frames,
                    u64::from(self.dsp.output_frame_rate()),
                )
            })
        } else {
            None
        };
        // The AD1848 / WSS terminal-count wake, on the codec's own (config) IRQ
        // line. The codec drains one Current Count per output frame, so its IRQ
        // estimator is fed the frame rate directly (no byte/word-counter scaling
        // like the SB16's). Considered only when that line can actually deliver
        // (`deliverable` also requires the master IR2 cascade pin for a slave line
        // 9/10/11) and the codec is enabled; frames_until_next_irq also returns
        // None when IEN is clear (the underflow then sets only the sticky Status
        // bit, no pin edge).
        let wss_wake = if self.wss_enabled && self.pic.deliverable(self.wss_irq) {
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

    /// CPU clocks until the next due device event.
    ///
    /// Interrupts are serviced at batch entry and devices advance at batch end,
    /// so known timer, audio, MIDI, storage, RTC, keyboard, serial, and printer edges
    /// shorten the batch to the first causal CPU clock in every CPU mode. Fast
    /// modes have a 1 ms fallback; the 386 modes keep a finer DAC-period fallback.
    /// A known edge may be earlier than either fallback.
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
        let clock_hz = self.active_mode.clock_hz();
        let mut cap = if self.active_mode.uses_approximate_timing() {
            clock_hz / 1000
        } else {
            clock_hz / u64::from(DAC_HZ)
        }
        .max(1);
        // Next PIT OUT rising edge: channel 0 feeds IRQ0, channel 2 the
        // speaker/GATE timing games poll. (Channel 1: see above.)
        for channel in [0usize, 2] {
            if let Some(ticks) = self.pit.clocks_until_out_rise(channel) {
                if let Some(clocks) = self.timeline.cpu_clocks_until(
                    timeline::DeviceClock::Pit,
                    ticks,
                    u64::from(PIT_INPUT_HZ),
                ) {
                    cap = cap.min(clocks);
                }
            }
        }
        // Next audio block IRQ edge. Both counters are expressed in output
        // frames, including stereo DMA-unit accounting inside the DSP.
        if let Some(frames) = self.dsp.frames_until_next_irq()
            && let Some(clocks) = self.timeline.cpu_clocks_until(
                timeline::DeviceClock::Dsp,
                frames,
                u64::from(self.dsp.output_frame_rate()),
            )
        {
            cap = cap.min(clocks);
        }
        if self.wss_enabled
            && let Some(frames) = self.wss.frames_until_next_irq()
            && let Some(clocks) = self.timeline.cpu_clocks_until(
                timeline::DeviceClock::Wss,
                frames,
                u64::from(self.wss.output_frame_rate()),
            )
        {
            cap = cap.min(clocks);
        }
        if let Some(ticks) = self.next_ata_deadline() {
            cap = cap.min(self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        }
        if self.vega.display_start_pending()
            && let Some(clocks) = self.timeline.cpu_clocks_until(
                timeline::DeviceClock::MargoFrame,
                1,
                timeline::MARGO_FRAME_HZ,
            )
        {
            cap = cap.min(clocks);
        }
        if let Some(ticks) = self.next_rtc_irq_deadline() {
            cap = cap.min(self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        }
        if let Some(ticks) = self.next_timed_io_deadline() {
            cap = cap.min(self.timeline.cpu_clocks_for_master_ticks_ceil(ticks).max(1));
        }
        cap.max(1).min(remaining)
    }
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
