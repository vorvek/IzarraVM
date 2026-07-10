// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Timing constants for audio, PIT, etc.
//! Extracted as part of Phase 3.

use super::*;

pub const OPL_NATIVE_HZ: u32 = 49_716;
pub const DAC_HZ: u32 = 44_100;
pub const PIT_INPUT_HZ: u32 = 1_193_182;
pub const WSS_AUTOCAL_FALLBACK_HZ: u32 = 8000;

impl Machine {
    /// Consume `secs` of emulated time for a device operation that blocks the
    /// guest (a floppy seek/read). Advancing both the timeline and the devices
    /// by the same amount keeps timekeeping coupled, the way an instruction's own
    /// clocks do. Guest time jumps forward; the GUI's realtime pacing then
    /// turns that jump into a visible wall-clock wait. Mechanical duration is
    /// independent of the active GSW speed.
    pub(super) fn stall_for(&mut self, secs: f64) {
        if secs <= 0.0 {
            return;
        }
        // Floppy mechanics still expose seconds as f64. Convert once at this
        // seam, rounding up to the first causal master tick. The floppy model can
        // move to integer durations without changing device advance.
        let master_ticks = (secs * izarravm_core::MASTER_CLOCK_HZ as f64).ceil() as u64;
        self.stall_for_master_ticks(master_ticks);
    }

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
        self.advance_master_time(master_ticks, true);
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
        // stereo frame onto the DSP ring. The half/end-buffer IRQ that
        // render_frame edges is forwarded to the PIC here, so playback timing and
        // IRQ5 no longer depend on the host frontend pulling audio. The host path
        // (render_dsp_audio) only drains what the clock already produced.
        //
        // MULTI-EDGE CONTRACT (holds for this DSP loop and the WSS/ADPCM loops
        // below, which mirror it): take_irq is drained INSIDE the producer loop,
        // at the sample tick that edged it, so every block edge reaches the PIC
        // within the advance in which it occurred and none is ever parked in the
        // device-side latch across a step (where a later gate, e.g. is_playing
        // going false at a single-cycle block end, could strand it). When one
        // advance spans N edges the PIC receives N requests, but the CPU does not
        // execute during advance_devices, so the guest cannot acknowledge between
        // them: the 8259 latches each request into IRR and a request on a
        // still-set IRR bit is absorbed, exactly as real hardware absorbs a new
        // pulse on a line whose interrupt is still pending. N intra-step edges
        // therefore deliver ONE guest interrupt by construction; that is the
        // architecturally correct coalescing, not a loss. What the run loop must
        // (and does, see the Approximate batch cap) arrange is that batches end
        // at block-edge instants when the guest needs one interrupt PER edge.
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
        if self.dsp.is_playing() && rate > 0 {
            let n = advance.dsp_frames as usize;
            let Machine {
                dsp, dma, memory, ..
            } = self;
            let is16 = dsp.is_16bit();
            let ch = if is16 { dma16 } else { dma8 };
            // HLE: on new block (command level), pre-fetch the entire block data
            // in bulk (advances DMA in one operation), store in buffer, then
            // render from buffer (no per-sample fetch micro steps).
            if dsp.block_remaining() == dsp.block_size() && dsp.block_buffer().is_none() {
                let bpp = if is16 { 2 } else { 1 };
                let nbytes = dsp.block_size() as usize * bpp;
                let mut buf = Vec::with_capacity(nbytes);
                for _ in 0..nbytes {
                    if is16 {
                        let w = dma.read_word(ch, memory).unwrap_or(0);
                        buf.extend_from_slice(&w.to_le_bytes());
                    } else {
                        buf.push(dma.read_byte(ch, memory).unwrap_or(0));
                    }
                }
                dsp.set_block_buffer(buf);
            }
            // HLE buffer feeding with refill support for auto-init blocks that may
            // be crossed inside a single large-n advance (e.g. tests and long steps).
            // We chunk the requested n at buffer/block boundaries so we can
            // re-prefetch the next DMA block when auto-init reloads remaining.
            let mut remaining = n;
            while remaining > 0 {
                if dsp.block_remaining() == dsp.block_size() && dsp.block_buffer().is_none() {
                    let bpp = if is16 { 2 } else { 1 };
                    let nbytes = dsp.block_size() as usize * bpp;
                    let mut buf = Vec::with_capacity(nbytes);
                    for _ in 0..nbytes {
                        if is16 {
                            let w = dma.read_word(ch, memory).unwrap_or(0);
                            buf.extend_from_slice(&w.to_le_bytes());
                        } else {
                            buf.push(dma.read_byte(ch, memory).unwrap_or(0));
                        }
                    }
                    dsp.set_block_buffer(buf);
                }
                let start_pos = dsp.block_buffer_pos();
                let mut consumed_from_buf: usize = 0;
                if let Some(buf) = dsp.block_buffer().cloned() {
                    let bytes_per_frame = if is16 { 2 } else { 1 };
                    let bytes_avail = buf.len().saturating_sub(start_pos);
                    if bytes_avail >= bytes_per_frame {
                        let frames_this = (bytes_avail / bytes_per_frame).min(remaining);
                        if is16 {
                            dsp.tick_n_samples(
                                frames_this,
                                || None,
                                || {
                                    let p = start_pos + consumed_from_buf;
                                    if p + 1 < buf.len() {
                                        let w = u16::from_le_bytes([buf[p], buf[p + 1]]);
                                        consumed_from_buf += 2;
                                        Some(w)
                                    } else {
                                        None
                                    }
                                },
                            );
                        } else {
                            dsp.tick_n_samples(
                                frames_this,
                                || {
                                    let p = start_pos + consumed_from_buf;
                                    if p < buf.len() {
                                        let b = buf[p];
                                        consumed_from_buf += 1;
                                        Some(b)
                                    } else {
                                        None
                                    }
                                },
                                || None,
                            );
                        }
                    }
                } else {
                    // Fallback direct per-frame (old path) for this chunk.
                    let frames_this = remaining; // will be limited by dry inside
                    if is16 {
                        dsp.tick_n_samples(frames_this, || None, || dma.read_word(ch, memory));
                    } else {
                        dsp.tick_n_samples(frames_this, || dma.read_byte(ch, memory), || None);
                    }
                }
                if consumed_from_buf > 0 {
                    dsp.advance_block_buffer(consumed_from_buf);
                }
                // If we hit end of this buf during the chunk, clear so next while
                // iteration (or future) can pre-fetch if auto-init reset the block.
                if dsp.block_buffer_pos() >= dsp.block_buffer_len() {
                    dsp.take_block_buffer();
                }
                // Reduce by how many we asked this chunk (actual produced may be
                // slightly less if dry, but advance will have stopped feeding).
                // Conservative: always reduce by the chunk size we targeted; if
                // underfed the phase carry will handle tail next advance.
                let did = if consumed_from_buf > 0 {
                    consumed_from_buf / (if is16 { 2 } else { 1 })
                } else {
                    remaining
                };
                if did == 0 {
                    break;
                }
                remaining = remaining.saturating_sub(did);
            }
            if dsp.take_irq() {
                let is_16bit = dsp.is_16bit();
                self.mixer.set_irq_status(is_16bit);
                self.pic.request(irq_line);
            }
        }
        // Forward a pending DSP interrupt with playback idle too: the 0xF2
        // IRQ-request command raises it without a transfer running (drivers
        // probe their IRQ wiring that way) — the real chip asserts the line
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
                    // HLE block buffer pre-fetch + feeding for WSS (Phase 4), with
                    // support for spanning multiple blocks within large n (refill
                    // when auto-reload happens inside tick).
                    let mut remaining = n;
                    while remaining > 0 {
                        if playing_at_valid_rate && self.wss.block_buffer().is_none() {
                            let frames = self.wss.current_dma_count() as usize;
                            let count = frames * self.wss.bytes_per_frame();
                            if count > 0 {
                                let mut buf = Vec::with_capacity(count);
                                {
                                    let Machine { dma, memory, .. } = self;
                                    for _ in 0..count {
                                        buf.push(dma.read_byte(wss_dma, memory).unwrap_or(0));
                                    }
                                }
                                self.wss.set_block_buffer(buf);
                            }
                        }
                        let mut consumed_from_buf: usize = 0;
                        if playing_at_valid_rate {
                            if let Some(buf) = self.wss.block_buffer().cloned() {
                                let start_pos = self.wss.block_buffer_pos();
                                let bytes_avail = buf.len().saturating_sub(start_pos);
                                if bytes_avail > 0 {
                                    let frames_this = bytes_avail.min(remaining);
                                    self.wss.tick_n_samples(frames_this, || {
                                        let p = start_pos + consumed_from_buf;
                                        if p < buf.len() {
                                            let b = buf[p];
                                            consumed_from_buf += 1;
                                            Some(b)
                                        } else {
                                            None
                                        }
                                    });
                                }
                            } else {
                                let frames_this = remaining;
                                let Machine {
                                    wss, dma, memory, ..
                                } = self;
                                wss.tick_n_samples(frames_this, || dma.read_byte(wss_dma, memory));
                            }
                        }
                        if consumed_from_buf > 0 {
                            self.wss.advance_block_buffer(consumed_from_buf);
                        }
                        if self.wss.block_buffer_pos() >= self.wss.block_buffer_len() {
                            self.wss.take_block_buffer();
                        }
                        let did = if consumed_from_buf > 0 {
                            consumed_from_buf
                        } else {
                            remaining
                        };
                        if did == 0 {
                            break;
                        }
                        remaining = remaining.saturating_sub(did);
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

        // CD audio (Red Book 44.1 kHz) HLE time-driven advance (Phase 4).
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

        // Decay the keyboard-to-aux settle window (see AUX_BYTE_SETTLE_TICKS in
        // keyboard.rs) so a mouse byte held back by a just-read keyboard
        // scancode releases once real PS/2 wire time has actually elapsed.
        self.keyboard.advance_mouse_pacing(advance.master_ticks);

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

        // ATA PIO and PIIX4 bus-master transfers share the authoritative master
        // timeline. A device-to-memory completion bypasses the CPU bus, so drop
        // cached code immediately. This also covers host-driven timeline advances
        // that occur outside the instruction run loop.
        if let Some(disk) = self.ata.as_mut() {
            disk.advance_master_ticks(advance.master_ticks);
            if self
                .bmide
                .advance_master_ticks(advance.master_ticks, &mut self.memory, disk)
            {
                self.cpu.note_device_memory_write();
            }
        }
        self.ide.advance_master_ticks(advance.master_ticks);

        // These edge latches coalesce like the PIC input pins. Timed UART and
        // LPT deadlines cap normal CPU batches, while a larger host-driven
        // advance may cross several transitions before the single pending edge
        // is forwarded here.
        if self.keyboard.take_irq() {
            self.pic.request(1); // IRQ1: keyboard output buffer has a scancode
        }
        if self.serial.take_irq() {
            self.pic.request(4); // IRQ4: COM1 (0x3F8) has a pending UART interrupt
        }
        if self.serial2.take_irq() {
            self.pic.request(3); // IRQ3: COM2 (0x2F8) has a pending UART interrupt
        }
        if self.keyboard.take_irq12() {
            self.pic.request(12); // IRQ12: mouse output buffer has an aux byte
        }
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
        if self.ide.take_access_bytes() > 0 {
            self.cd_accesses += 1;
        }

        self.margo.advance_busy(advance.margo_nanoseconds);
        self.margo.advance_frames(advance.margo_frames);

        // Distira's 525-line scanout runs at 60 Hz in fixed guest time,
        // independent of the active CPU mode.
        self.distira.advance_frame_phase(advance.distira_lines);

        self.video.advance(advance.vga_dots);

        self.pump_pusher();
    }

    fn device_rates(&mut self) -> DeviceRates {
        self.dsp.set_sbpro_stereo(self.mixer.sbpro_stereo());
        let programmed_wss = self.wss.output_frame_rate();
        DeviceRates {
            dsp_hz: if self.dsp.is_playing() {
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
            vga_dot_hz: self.video.dot_clock_hz(),
        }
    }

    fn advance_master_time(&mut self, master_ticks: u64, io_stall: bool) {
        let rates = self.device_rates();
        let advance = if io_stall {
            self.timeline.advance_io_stall_ticks(master_ticks, rates)
        } else {
            self.timeline.advance_master_ticks(master_ticks, rates)
        };
        self.apply_device_advance(advance);
    }

    pub(super) fn advance_cpu_work(&mut self, clocks: u64) {
        let rates = self.device_rates();
        let advance = self.timeline.advance_cpu_clocks(clocks, rates);
        self.apply_device_advance(advance);
        self.elapsed_clocks = self.elapsed_clocks.saturating_add(clocks);
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
        self.advance_master_time(master_ticks, false);
    }

    pub(super) fn advance_halted_ticks(&mut self, master_ticks: u64) {
        let cpu_clocks = self.timeline.cpu_clocks_for_master_ticks_ceil(master_ticks);
        self.advance_master_time(master_ticks, false);
        self.elapsed_clocks = self.elapsed_clocks.saturating_add(cpu_clocks);
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
        let edge_dots = self.video.dots_until_vretrace_start()?;
        self.timeline.cpu_clocks_until(
            timeline::DeviceClock::Vga,
            edge_dots,
            self.video.dot_clock_hz(),
        )
    }

    fn master_ticks_to_vretrace_start(&self) -> Option<u64> {
        let edge_dots = self.video.dots_until_vretrace_start()?;
        self.timeline.master_ticks_until(
            timeline::DeviceClock::Vga,
            edge_dots,
            self.video.dot_clock_hz(),
        )
    }

    /// Advance the DSP reset-settle clock by `micros` microseconds. The run loop
    /// drives this from CPU clocks in advance_devices; this exposes it directly
    /// so a reset-detection golden can settle the DSP without running the CPU.
    pub fn advance_dsp_micros(&mut self, micros: u64) {
        self.dsp.advance_micros(micros);
    }

    /// Drive a PIT counter's GATE line. The PC ties GATE0/GATE1 high; the sound
    /// slice wires GATE2 from port 0x61. Exposed now so the GATE-triggered modes
    /// have a caller outside tests.
    pub fn set_timer_gate(&mut self, channel: usize, level: bool) {
        self.pit.set_gate(channel, level);
    }

    /// Input CLK pulses until channel 0 produces its next OUT rising edge, or None
    /// if the counter cannot fire from its current state. Used by the HLT
    /// fast-forward path added in Task 2b-2.
    pub fn clocks_until_timer0_irq(&self) -> Option<u64> {
        self.pit.clocks_until_channel0_irq()
    }

    /// CPU clocks to advance while halted so the next wake-capable IRQ lands, or
    /// None if nothing can wake the CPU (so HLT is a genuine halt). A halted guest
    /// is woken by timer, audio, storage, RTC, serial, or printer completion.
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
        // The sooner of whichever wakes apply; None only when none can fire.
        let wake = [
            pit_wake,
            dsp_wake,
            wss_wake,
            ata_wake,
            atapi_wake,
            rtc_wake,
            serial_wake,
            serial2_wake,
            lpt_wake,
            lpt2_wake,
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
            .min()
    }

    /// CPU clocks until the next due device event.
    ///
    /// Interrupts are serviced at batch entry and devices advance at batch end,
    /// so known timer, audio, storage, RTC, serial, and printer edges shorten the
    /// batch to the first causal CPU clock in every CPU mode. Fast modes have a
    /// 1 ms fallback; the 386 modes keep a finer DAC-period fallback. A known
    /// edge may be earlier than either fallback.
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
        // Next audio block-IRQ edge. Both counters are expressed in output
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
        if self.margo.display_start_pending()
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

    /// Peek the VGA beam's dot position "now" -- mid-batch, as of whatever this
    /// batch's accumulated core clocks plus the bus clocks recorded into `trace`
    /// since batch entry add up to -- WITHOUT mutating any device state (`video`,
    /// `vga_dots`, `bus_rem` on the owning `Machine` are all untouched; `&self`
    /// makes that compiler-enforced). This is the P4a Slice 1 lazy port-read peek
    /// used by time-dependent port reads.
    ///
    /// Units combined, matching exactly what the real batch-end step in
    /// `run_until_tick`/`advance_cpu_work` will later consume:
    /// - the core portion, BATCH-scoped: `prior_runs_core_clocks` (the
    ///   interrupt-service charge plus every completed straight-line run of this
    ///   batch, republished by the batch loop before each run) plus
    ///   `core_clocks_so_far` (the current run's prior instructions, excluding
    ///   the in-flight instruction's own charge). One batch chains many runs and
    ///   only the batch total feeds the batch-end step, so the run-scoped term
    ///   alone would drop earlier runs' core clocks and jump backward at every
    ///   run boundary; the monotonicity claim below rests on this batch-scoping,
    ///   not on a port read ending the run.
    /// - the bus portion: `trace.elapsed_clocks() - trace_elapsed_at_batch_start`
    ///   raw bus clocks recorded so far this batch, scaled by the SAME (num, den)
    ///   `bus_timing` ratio and the SAME fractional carry (`bus_rem_at_batch_start`)
    ///   the real end-of-batch `scale_bus` call will start from -- no `scale_bus`
    ///   call happens between batch entry and batch end, so the batch-entry carry
    ///   IS the carry the real call uses. This mirrors `scale_bus`'s arithmetic
    ///   shape exactly but reads `bus_rem_at_batch_start` instead of the live
    ///   `bus_rem` and does not write the carry back anywhere (no mutation).
    ///
    /// The in-flight instruction's own fetch/data bus clocks may already be
    /// partially recorded into `trace` by the time this runs; that is fine and
    /// intentional -- the real batch-end total (computed once the whole
    /// instruction has retired) is always a superset of what is recorded here, so
    /// the clock total this predicts from is monotone within the batch and never
    /// exceeds the batch's eventual final total. It never overshoots what the
    /// real advance would show for the same clock total, because it uses the same
    /// formula.
    ///
    /// Predicts POSITION ONLY: the dots-per-frame modulo wrap, never the
    /// frame-boundary side effects (`finalize_frame`, the frame counter) that
    /// `Vga::advance` performs -- those stay exclusively in the real
    /// `advance_devices` at batch end. Shares the exact `predict_dots_core`
    /// arithmetic `Machine::predict_dots` uses (same operation order, same
    /// floor/subtract sequence), so a mid-batch peek can never structurally
    /// diverge from what the later real advance will show for the same clocks.
    pub(super) fn predicted_beam(&self) -> u64 {
        let in_batch_clocks = self.in_batch_clocks();
        let (_, whole_dots) = self
            .timeline_at_batch_start
            .preview_cpu_clocks(in_batch_clocks, self.video.dot_clock_hz());
        let frame = self.video.frame_dots();
        if frame == 0 {
            return self.beam_at_batch_start; // guard: un-programmed CRTC, mirrors Vga::advance
        }
        (self.beam_at_batch_start + whole_dots) % frame
    }

    /// The batch-scoped CPU clock total elapsed as of "now" (mid-batch), the
    /// shared T both `predicted_beam` and `predicted_pit_clocks` build on: batch-
    /// scoped core clocks (`prior_runs_core_clocks + core_clocks_so_far`) plus
    /// in-batch bus clocks recorded into `trace` since batch entry, scaled by the
    /// SAME (num, den) `bus_timing` ratio and fractional carry
    /// (`bus_rem_at_batch_start`) the real end-of-batch `scale_bus` call will
    /// start from. Extracted from `predicted_beam` (P4a Task 2.3) so the PIT lazy
    /// read consumes byte-for-byte the same clock total the beam peek does,
    /// rather than a second hand-rolled copy of this arithmetic.
    fn in_batch_clocks(&self) -> u64 {
        let in_batch_bus_clocks = self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start;
        let scaled = in_batch_bus_clocks * u64::from(self.bus_num_at_batch_start)
            + self.bus_rem_at_batch_start;
        let scaled_bus_clocks = scaled / u64::from(self.bus_den_at_batch_start);
        self.prior_runs_core_clocks + self.core_clocks_so_far + scaled_bus_clocks
    }

    /// Elapsed PIT input CLKs "now" -- mid-batch, WITHOUT mutating `pit_clocks`
    /// (P4a Task 2.3: the lazy port 0x61 bits 4/5 read). Shared by every channel
    /// a caller peeks in the same read (0x61 needs both channel 1 and channel
    /// 2), so a caller that needs more than one channel should compute this ONCE
    /// and pass it to `Pit::out_after` per channel, not call `predicted_pit_out`
    /// (below) once per channel -- the two calls would otherwise redo this exact
    /// conversion redundantly (measured: that redundancy erased most of the
    /// batch-chaining win in the P4a Task 2.3 A/B, see the microbench report).
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
            .preview_cpu_clocks(self.in_batch_clocks(), self.video.dot_clock_hz())
            .0
    }

    /// Peek `channel`'s live PIT OUT level "now" -- mid-batch, WITHOUT stepping
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
