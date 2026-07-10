// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl Machine {
    pub(super) fn make_bus(&mut self) -> MachineBus<'_> {
        // Captured before the struct literal below since VEGA and trace are also
        // mutably borrowed by other fields in that same literal.
        let beam_at_batch_start = self.vega.beam_dots();
        let trace_elapsed_at_batch_start = self.trace.elapsed_clocks();
        // Read from the CPU, the same authoritative mode owner that scale_bus
        // uses. Machine's active_mode copy is kept for bus register readback.
        let (bus_num_at_batch_start, bus_den_at_batch_start) = bus_timing(self.cpu.level());
        MachineBus {
            memory: &mut self.memory,
            ram_lookup: &mut self.ram_lookup,
            vega: &mut self.vega,
            pci: &mut self.pci,
            rom: &self.rom,
            serial: &mut self.serial,
            serial2: &mut self.serial2,
            lpt: &mut self.lpt,
            lpt2: &mut self.lpt2,
            device_ports: &mut self.device_ports,
            pic: &mut self.pic,
            pit: &mut self.pit,
            keyboard: &mut self.keyboard,
            speaker: &mut self.speaker,
            rtc: &mut self.rtc,
            dma: &mut self.dma,
            fdc: &mut self.fdc,
            opl: &mut self.opl,
            dsp: &mut self.dsp,
            mixer: &mut self.mixer,
            wavetable_mpu: &mut self.wavetable_mpu,
            midi_mpu: &mut self.midi_mpu,
            wss: &mut self.wss,
            wss_base: self.wss_base,
            wss_enabled: self.wss_enabled,
            ide: &mut self.ide,
            ata: &mut self.ata,
            bmide: &mut self.bmide,
            trace: &mut self.trace,
            pending_soft_int: &mut self.pending_soft_int,
            last_int_vector: &mut self.last_int_vector,
            active_mode: self.active_mode,
            pending_mode: &mut self.pending_mode,
            fast_post: self.fast_post,
            booter_inert: self.booter_inert,
            program_runtime: self.program_runtime,
            pending_toka_service: &mut self.pending_toka_service,
            toka_service_status: self.toka_service_status,
            unittester: &mut self.unittester,
            wait_states: self.profile.wait_states,
            cache: &mut self.cache_model,
            flat_data_cost: self.active_mode.uses_approximate_timing(),
            lazy_port_reads: self.active_mode.uses_approximate_timing(),
            io_touched: &mut self.io_touched,
            isa_io_clocks: &mut self.isa_io_batch_clocks,
            device_wrote_memory: &mut self.device_wrote_memory,
            direct_map_changed: &mut self.direct_map_changed,
            core_clocks_so_far: 0,
            prior_runs_core_clocks: 0,
            timeline_at_batch_start: self.timeline,
            master_ticks_at_batch_start: self.timeline.now_ticks(),
            beam_at_batch_start,
            trace_elapsed_at_batch_start,
            bus_rem_at_batch_start: self.bus_rem,
            bus_num_at_batch_start,
            bus_den_at_batch_start,
        }
    }

    pub fn read_physical_u8(&mut self, address: u32) -> u8 {
        let mut bus = self.make_bus();
        bus.read_phys_u8(address).unwrap_or(0)
    }

    pub fn read_physical_u16(&mut self, address: u32) -> u16 {
        let mut bus = self.make_bus();
        bus.read_memory(address, BusWidth::Word, BusAccessKind::DataRead)
            .map(|value| value as u16)
            .unwrap_or(0)
    }

    pub fn read_physical_u32(&mut self, address: u32) -> u32 {
        let mut bus = self.make_bus();
        bus.read_memory(address, BusWidth::Dword, BusAccessKind::DataRead)
            .unwrap_or(0)
    }

    /// Last byte written to a passive I/O port (such as 0x80, the POST diagnostic
    /// port), or None if the port address is not in the passive port map. A
    /// decoded but never written port reads its default, not None.
    pub fn io_port(&self, port: u16) -> Option<u8> {
        self.device_ports.read_port(port)
    }

    pub fn write_physical_u8(&mut self, address: u32, value: u8) {
        let mut bus = self.make_bus();
        let _ = bus.write_memory_byte(address, value);
    }

    pub fn write_physical_u16(&mut self, address: u32, value: u16) {
        let mut bus = self.make_bus();
        let _ = bus.write_memory(
            address,
            BusWidth::Word,
            u32::from(value),
            BusAccessKind::DataWrite,
        );
    }

    pub fn write_physical_u32(&mut self, address: u32, value: u32) {
        let mut bus = self.make_bus();
        let _ = bus.write_memory(address, BusWidth::Dword, value, BusAccessKind::DataWrite);
    }

    pub fn bus_trace(&self) -> &BusTrace {
        &self.trace
    }

    pub fn set_bus_trace_detailed(&mut self, detailed: bool) {
        self.trace.set_tracing_mode(if detailed {
            TracingMode::Full
        } else {
            TracingMode::Off
        });
    }
}

impl MachineBus<'_> {
    pub(crate) fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        let core_clocks_so_far = self.core_clocks_so_far;
        <Self as CpuBus>::write_io(
            self,
            port,
            width,
            value,
            core_clocks_so_far,
            cpu_is_ring0_pm,
        )
    }
}

/// The A20 gate clears address line 20 when it is closed. With the gate off, any
/// physical address with bit 20 set folds down by 0x100000, so a real-mode
/// program reaching 0x100000-0x10FFEF (the most a seg:off pair can address) wraps
/// back to 0x0-0xFFEF, the classic 1 MiB wraparound the HMA depends on. The
/// effect is intentionally global, matching A20M# on real hardware: bit 20 is
/// cleared on every physical address, so high ROM (0xFFFF0000) and the upper half
/// of the Margo LFB alias down too when the gate is closed. That is unreachable
/// in normal use, since A20 powers on enabled and stays so unless a guest
/// deliberately closes it.
const A20_MASK: u32 = !(1 << 20);

/// The port each byte of a wider-than-byte I/O cycle targets. The IDE/ATA 16-bit
/// data registers (primary `0x1F0`, secondary `0x170`) stream every byte through
/// the same port via their data FIFO, so a word/dword access repeats the port.
/// Every other (8-bit-decoded) port takes consecutive bytes at `port`, `port+1`,
/// ... - exactly the VGA index/data-pair behaviour a single 16-bit `OUT` to
/// `0x3C4`/`0x3CE`/`0x3D4` relies on to set an index and its datum at once.
const fn io_word_sub_port(port: u16, index: u32) -> u16 {
    if port == ata::PRIMARY_CMD_BASE || port == ide::SECONDARY_CMD_BASE {
        port
    } else {
        port.wrapping_add(index as u16)
    }
}

impl CpuBus for MachineBus<'_> {
    fn read_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryRead, BusError> {
        if let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, width.bytes() as usize, width)
        {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            let data = &self.memory.as_slice()[start..end];
            let value = match width {
                BusWidth::Byte => u32::from(data[0]),
                BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
                BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            };
            return Ok(DirectMemoryRead {
                value,
                direct: true,
            });
        }
        self.read_memory(address, width, kind)
            .map(|value| DirectMemoryRead {
                value,
                direct: false,
            })
    }

    fn write_memory_direct(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<DirectMemoryWrite, BusError> {
        if let Some((address, start, _)) =
            self.direct_page_ram_bytes(address, width.bytes() as usize, width)
        {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            match width {
                BusWidth::Byte => self.memory.write_u8(start, value as u8)?,
                BusWidth::Word => self.memory.write_u16(start, value as u16)?,
                BusWidth::Dword => self.memory.write_u32(start, value)?,
            }
            return Ok(DirectMemoryWrite { direct: true });
        }
        self.write_memory(address, width, value, kind)
            .map(|()| DirectMemoryWrite { direct: false })
    }

    fn read_memory_bytes_direct(
        &mut self,
        address: u32,
        out: &mut [u8],
        access_width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if out.is_empty() {
            return Ok(0);
        }
        let access = access_width.bytes() as usize;
        if out.len() % access != 0 {
            return Ok(0);
        }
        let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, out.len(), access_width)
        else {
            return Ok(0);
        };
        for offset in (0..out.len()).step_by(access) {
            let at = address + offset as u32;
            let ws = self.data_access_wait_states(at, access_width);
            self.trace.record(kind, at, access_width, ws);
        }
        out.copy_from_slice(&self.memory.as_slice()[start..end]);
        Ok(out.len())
    }

    fn write_memory_bytes_direct(
        &mut self,
        address: u32,
        data: &[u8],
        access_width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<usize, BusError> {
        if data.is_empty() {
            return Ok(0);
        }
        let access = access_width.bytes() as usize;
        if data.len() % access != 0 {
            return Ok(0);
        }
        let Some((address, start, end)) =
            self.direct_page_ram_bytes(address, data.len(), access_width)
        else {
            return Ok(0);
        };
        for offset in (0..data.len()).step_by(access) {
            let at = address + offset as u32;
            let ws = self.data_access_wait_states(at, access_width);
            self.trace.record(kind, at, access_width, ws);
        }
        self.memory.as_mut_slice()[start..end].copy_from_slice(data);
        Ok(data.len())
    }

    fn direct_memory_bytes(&self, address: u32, bytes: usize, access_width: BusWidth) -> usize {
        self.direct_page_ram_bytes(address, bytes, access_width)
            .map_or(0, |(_, start, end)| end - start)
    }

    #[inline]
    fn direct_page(
        &mut self,
        address: u32,
        kind: BusAccessKind,
    ) -> Result<Option<DirectPage>, BusError> {
        let gated = self.apply_a20(address);
        if gated != address {
            return Ok(None);
        }
        let physical_page = gated & !(RAM_LOOKUP_PAGE_MASK as u32);
        let Some((start, end)) = self.direct_ram_bytes(physical_page, RAM_LOOKUP_PAGE_SIZE) else {
            return Ok(None);
        };
        if end - start != RAM_LOOKUP_PAGE_SIZE {
            return Ok(None);
        }
        Ok(Some(DirectPage {
            physical_page,
            ptr: unsafe { self.memory.as_mut_ptr().add(start) },
            len: RAM_LOOKUP_PAGE_SIZE,
            writable: matches!(kind, BusAccessKind::DataWrite),
        }))
    }

    #[inline]
    fn charge_direct_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        // Only the CPU's DirectPageCache fast paths call this, and a live entry
        // guarantees cacheable RAM under the current A20 state: `direct_page`
        // installs a page only when `apply_a20` is the identity for it (an A20
        // toggle then invalidates the cache via note_a20_changed), and the
        // direct map never covers a device window. Conventional pages sit below
        // the 0xA0000 aperture; extended pages start at 1 MiB, exclude the
        // Distira BAR (whose decode changes rebuild the map AND invalidate the
        // cache), and system RAM ends below the Margo LFB/MMIO and high-ROM
        // bases. So in the Approximate class (`flat_data_cost`) the charge is
        // always the flat L1 cost: skip apply_a20 and the wait-state routing.
        // The Accurate class keeps the full path so its tag arrays stay warm.
        //
        // Accepted residue: a same-instruction REP OUTS that moves the Distira
        // BAR over its own source buffer keeps charging the stale entry's flat
        // cost until the post-instruction io_touched step break invalidates it.
        // That divergence is timing-only; functional behavior is identical.
        if self.flat_data_cost {
            self.trace.record(kind, address, width, self.cache.cost.l1);
            return Ok(());
        }
        let address = self.apply_a20(address);
        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);
        Ok(())
    }

    /// One instruction-fetch access of cacheable RAM: `clocks_for(_, code_fetch_wait_states)` = 2 +
    /// the per-mode I-cache constant. Matches what `charge_instruction_fetch_run`'s cacheable-RAM
    /// fast path records for one access (machine.rs ~9806). The JIT cost-fold folds this per slot.
    fn jit_fetch_cost_clocks(&self) -> u64 {
        2 + u64::from(self.cache.code_fetch_wait_states())
    }

    /// One byte-wide direct data access: `clocks_for(Byte, cost.l1)` = 2 + the flat L1 wait-state,
    /// exactly what `charge_direct_memory` records for a direct-page hit in the Approximate class.
    fn jit_data_byte_cost_clocks(&self) -> u64 {
        2 + u64::from(self.cache.cost.l1)
    }

    /// Flush the JIT cost-fold's accumulated bus clocks into the trace's running total in one op.
    fn charge_bus_clocks_bulk(&mut self, clocks: u64) {
        self.trace.add_elapsed_clocks(clocks);
    }

    /// See the trait doc: the straight-line run loop adds this figure's growth
    /// to its core total against the guest-clock run cap. The same scaled-bus
    /// accounting applies in every CPU mode so a bus-heavy run cannot cross an
    /// earlier master-timeline deadline unnoticed.
    fn in_batch_scaled_bus_clocks(&self) -> u64 {
        let raw = self.trace.elapsed_clocks() - self.trace_elapsed_at_batch_start;
        (raw * u64::from(self.bus_num_at_batch_start) + self.bus_rem_at_batch_start)
            / u64::from(self.bus_den_at_batch_start)
    }

    fn read_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> Result<u32, BusError> {
        let address = self.apply_a20(address);
        let bytes = width.bytes() as usize;

        if let Some(value) = self.vega.read_wide_memory(address, width) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            return Ok(value);
        }

        if should_split(address, width) {
            let mut value = 0u32;
            for offset in 0..width.bytes() {
                let byte = self.read_memory(address + offset, BusWidth::Byte, kind)?;
                value |= byte << (offset * 8);
            }
            return Ok(value);
        }

        if let Some((start, end)) = self.direct_ram_range(address, width) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            let data = &self.memory.as_slice()[start..end];
            return Ok(match width {
                BusWidth::Byte => u32::from(data[0]),
                BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
                BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            });
        }

        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);

        let mut data = [0u8; 4];
        self.read_phys(address, &mut data[..bytes])?;
        Ok(match width {
            BusWidth::Byte => u32::from(data[0]),
            BusWidth::Word => u32::from(u16::from_le_bytes([data[0], data[1]])),
            BusWidth::Dword => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        })
    }

    fn write_memory(
        &mut self,
        address: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> Result<(), BusError> {
        let address = self.apply_a20(address);
        if self.vega.write_wide_memory(address, width, value) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            return Ok(());
        }

        if should_split(address, width) {
            for offset in 0..width.bytes() {
                self.write_memory(
                    address + offset,
                    BusWidth::Byte,
                    (value >> (offset * 8)) & 0xff,
                    kind,
                )?;
            }
            return Ok(());
        }

        if let Some((start, _)) = self.direct_ram_range(address, width) {
            let ws = self.data_access_wait_states(address, width);
            self.trace.record(kind, address, width, ws);
            return match width {
                BusWidth::Byte => self.memory.write_u8(start, value as u8),
                BusWidth::Word => self.memory.write_u16(start, value as u16),
                BusWidth::Dword => self.memory.write_u32(start, value),
            };
        }

        let ws = self.data_access_wait_states(address, width);
        self.trace.record(kind, address, width, ws);

        match width {
            BusWidth::Byte => self.write_memory_byte(address, value as u8),
            BusWidth::Word => {
                for (offset, byte) in (value as u16).to_le_bytes().into_iter().enumerate() {
                    self.write_memory_byte(address + offset as u32, byte)?;
                }
                Ok(())
            }
            BusWidth::Dword => {
                for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
                    self.write_memory_byte(address + offset as u32, byte)?;
                }
                Ok(())
            }
        }
    }

    fn prefetch_memory(&mut self, address: u32, out: &mut [u8]) -> Result<usize, BusError> {
        let address = self.apply_a20(address);
        // Fast path: a prefetch window entirely within conventional RAM (below
        // the 0xA0000 video aperture) is one bounded slice copy instead of a
        // gauntlet-walking read_phys_u8 per byte.
        let ram_end = address as usize + out.len();
        if ram_end <= 0x000A_0000 && ram_end <= self.memory.len() {
            out.copy_from_slice(&self.memory.as_slice()[address as usize..ram_end]);
            return Ok(out.len());
        }
        let mut copied = 0;
        for (offset, byte) in out.iter_mut().enumerate() {
            match self.read_phys_u8(address + offset as u32) {
                Ok(value) => {
                    *byte = value;
                    copied += 1;
                }
                Err(BusError::UnmappedMemory { .. }) if copied > 0 => break,
                Err(err) => return Err(err),
            }
        }
        Ok(copied)
    }

    #[inline]
    fn note_code_fetch_linear(&mut self, linear: u32) {
        // One range compare (0xFF000..0xFF400: the legacy FF00:0000 target
        // through the end of the per-vector stub table) keeps this out of the
        // way of every ordinary fetch.
        if linear.wrapping_sub(BIOS_LEGACY_IRET_LINEAR) < BIOS_STUB_WINDOW_LEN {
            self.note_stub_fetch(linear);
        }
    }

    fn charge_instruction_fetch(&mut self, address: u32) -> Result<(), BusError> {
        let address = self.apply_a20(address);
        let ws = self.code_fetch_wait_states(address);
        self.trace.record(
            BusAccessKind::InstructionPrefetch,
            address,
            BusWidth::Byte,
            ws,
        );
        Ok(())
    }

    fn charge_instruction_fetch_run(&mut self, start: u32, count: u32) -> Result<(), BusError> {
        if count == 0 {
            return Ok(());
        }
        // Stub recognition, run-charge seam: the trigger check runs on the
        // run's START address only (a stub entry is always a fresh run:
        // execution arrives by IVT far transfer or IRET return, never by
        // falling through). `start` here is the run's LINEAR address - the
        // same domain `note_code_fetch_linear` observes on the cold path.
        if start.wrapping_sub(BIOS_LEGACY_IRET_LINEAR) < BIOS_STUB_WINDOW_LEN {
            self.note_stub_fetch(start);
        }
        // Fast path: a run that lies entirely in conventional RAM (below the
        // 0xA0000 video aperture). Every address below 0x100000 has bit 20 clear,
        // so `apply_a20` is the identity there regardless of the gate state;
        // `code_fetch_wait_states` is the per-mode I-cache constant for any
        // address below 0xA0000 (the device-window gate only engages at or above
        // it); and a contiguous run below 0xA0000 is uniform by construction.
        // The classification below would therefore always land in the uniform
        // cacheable-RAM arm and charge ONE I-cache access at the constant
        // wait-state, so charge exactly that in one step. ROM/device/A20-edge
        // runs keep the full classification, byte-for-byte.
        if let Some(end) = start.checked_add(count - 1) {
            if end < 0x000A_0000 {
                self.trace.record_instruction_fetch_run(
                    start,
                    1,
                    self.cache.code_fetch_wait_states(),
                );
                return Ok(());
            }
        }
        let first = self.apply_a20(start);
        let last = self.apply_a20(start.wrapping_add(count - 1));
        let first_ws = self.code_fetch_wait_states(first);
        // Uniform iff every byte lands in the same wait-state region with no A20 wrap
        // between the ends. apply_a20 already folded both ends, so equal wait-states on
        // contiguous post-A20 addresses means the whole run is one region. The endpoint-only
        // test relies on `count` being one instruction's length (at most 15 bytes), far smaller
        // than any wait-state region: a narrower region wholly contained between two matching
        // endpoints cannot exist at that scale. A caller passing a large `count` must not assume
        // this holds; the non-uniform branch's exact per-byte loop is the safe fallback regardless.
        let uniform =
            last == first.wrapping_add(count - 1) && first_ws == self.code_fetch_wait_states(last);
        if uniform {
            // I-cache model: an instruction whose bytes lie in cacheable RAM is
            // delivered by the I-cache in ONE bus access, not one per byte. The
            // per-byte bus cost (>= 2 clocks/byte) is a slow-bus artifact; on a part
            // with an instruction cache a hit returns the whole (pre-decoded) line in
            // a single fetch. Charging per byte here floors every mode's Dhrystone/
            // Sieve far below its era band (the floor is the same clocks in every
            // mode, so the fast modes can never separate). One access per instruction
            // makes the bands reachable for the slower modes and lifts the fast modes
            // toward (though not all the way to, see bench_reference.rs) their targets.
            //
            // ROM / device code (uncached) keeps the exact per-byte charge: those
            // windows are not I-cached, so `is_device_window` routes them to the
            // per-byte loop below to preserve firmware/POST and device-execution
            // timing unchanged.
            if first >= 0x000A_0000 && self.is_device_window(first, BusWidth::Byte) {
                self.trace
                    .record_instruction_fetch_run(first, count, first_ws);
            } else {
                // Single I-cache access for the whole instruction run.
                self.trace.record_instruction_fetch_run(first, 1, first_ws);
            }
        } else {
            for i in 0..count {
                self.charge_instruction_fetch(start.wrapping_add(i))?;
            }
        }
        Ok(())
    }

    fn read_io(
        &mut self,
        port: u16,
        width: BusWidth,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<u32, BusError> {
        // Publish the current run offset for lazy time-dependent reads.
        self.core_clocks_so_far = core_clocks_so_far;
        // Ring-0-monitor port-time exemption (V86 trap tax, Part 1): the TOKAEMM
        // monitor's own device pokes (the vec13 discriminator's PIC OCW3 probe,
        // chiefly) are chipset-side bookkeeping done on the guest's behalf, not
        // guest-visible device activity in their own right. Ending the CPU batch
        // around them (the normal io_touched contract) triples the guest-visible
        // cost of every V86 trap for no fidelity gain: device time is still exact
        // because the batch still ends at the next event_batch_cap edge or the
        // next GUEST port access, and OCW3's read-select is pure register state
        // (see pic.rs -- `read_isr` is a mode bit, not time-derived), so deferring
        // exactly when it is consumed relative to batch-end timing is safe. Gated
        // on `lazy_port_reads` (Approximate class only, i.e. 486/586): the
        // Accurate 386 class keeps byte-identical batch semantics, matching
        // every other lazy-read gate in this function.
        let skip_io_touched = cpu_is_ring0_pm && self.lazy_port_reads;
        // Bus-clock trace recording stays unconditional for every port, both timing
        // classes: `predicted_beam`'s bus term scales exactly the clocks recorded
        // here, so a lazy read that skipped this would under-predict its own beam.
        self.trace.record(
            BusAccessKind::IoRead,
            u32::from(port),
            width,
            self.wait_states.io,
        );

        if let Some(value) = self.pci.read_io(port, width, self.vega) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(value);
        }

        if let Some(base) = self.pci.ide_bus_master_io_base()
            && bmide::BusMasterIde::owns_io(port, width, base)
        {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            if self.pci.ide_io_enabled() {
                return Ok(self.bmide.read_io(port, width, base));
            }
            return Ok(match width {
                BusWidth::Byte => 0xff,
                BusWidth::Word => 0xffff,
                BusWidth::Dword => u32::MAX,
            });
        }

        if width != BusWidth::Byte {
            // A wider-than-byte port access decomposes into byte cycles, the way the
            // ISA bus does for a port that is not 16-bit: the low byte comes from the
            // port and each higher byte from the next port (`io_word_sub_port` keeps
            // the IDE/ATA data registers on the same port). This is the canonical VGA
            // mode-set path - a single 16-bit `OUT 0x3C4`/`0x3CE`/`0x3D4` sets an
            // index and its datum - which used to halt the VM with WidthMismatch.
            // Per-byte io_touched/lazy dispatch happens in the recursive calls below
            // (each byte re-enters read_io), so nothing to set here directly.
            let mut value = 0u32;
            for i in 0..width.bytes() {
                let byte = self.read_io(
                    io_word_sub_port(port, i),
                    BusWidth::Byte,
                    core_clocks_so_far,
                    cpu_is_ring0_pm,
                )?;
                value |= (byte & 0xff) << (8 * i);
            }
            return Ok(value);
        }

        if self.vega.port_disabled(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(0xff);
        }

        if let Some(value) = self.serial.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.serial2.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.lpt.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.lpt2.read_port(port) {
            if !skip_io_touched {
                *self.io_touched = true;
            }
            return Ok(u32::from(value));
        }
        // The VGA status ports (3DA/3BA/3C2) are the ONLY arm in this function that
        // does not unconditionally set io_touched. In the lazy-read case for the
        // Approximate timing class they must NOT end the batch (io_touched stays
        // false) so a poll loop chains as `run_straight_line` continuations. Static
        // per-port dispatch: these three port numbers always land here, whether or
        // not lazy_port_reads is set, so the branch is a single bool test, never a
        // per-access classification.
        //
        // DECISION (batch-retroactive-rate subtlety): a batch shaped [lazy 3DA polls
        // ... OUT 0x3C2 lowering the dot clock] applies the new dot-clock rate to the
        // WHOLE batch at batch end (the pre-existing retroactive-rate behavior of
        // advance_devices/scale_bus), so the batch-end beam can land behind the last
        // lazy-predicted value this loop observed. Accepted as-is, no compensation:
        // a dot-clock switch is not beam-continuous on real hardware either, and the
        // write itself sets io_touched and ends the batch, so no further lazy read
        // can observe the stale prediction within the same batch.
        if matches!(port, 0x3DA | 0x3BA | 0x3C2) && self.vega.port_enabled(port) {
            if self.lazy_port_reads {
                let beam = self.predicted_beam();
                if let Some(value) = self.vega.read_status_port_lazy(port, beam) {
                    return Ok(u32::from(value));
                }
                // Inactive alias (e.g. 3BA polled in a color setup): no side
                // effects, matching `Vga::read_port`'s existing
                // `status1_port_selected` gate, and -- since this arm's static
                // port set is disjoint from every other device's decoded ports
                // (grep-confirmed: nothing else claims 0x3B0..=0x3DF) -- the same
                // 0xFF the non-lazy path's fallthrough to `device_ports`'s passive
                // table would eventually produce. Returned directly, without
                // setting io_touched, so an inactive-alias poll stays lazy too
                // instead of silently falling back to the old behavior.
                return Ok(0xff);
            } else {
                if !skip_io_touched {
                    *self.io_touched = true;
                }
                if let Some(value) = self.vega.read_port(port) {
                    return Ok(u32::from(value));
                }
            }
        } else if self.vega.port_enabled(port) {
            if let Some(value) = self.vega.read_port(port) {
                if !skip_io_touched {
                    *self.io_touched = true;
                }
                return Ok(u32::from(value));
            }
        }
        // Port 0x61 bits 4/5 use the same lazy
        // per-port dispatch discipline as 3DA/3BA/3C2 above -- 0x61 always lands
        // here whether or not lazy_port_reads is set. Bits 0/1 (speaker gate/data)
        // are plain register state that cannot change mid-batch: the only writer
        // is `write_io`, which unconditionally sets io_touched and so ends the
        // batch before a later lazy read in the same batch could observe a stale
        // value. Bits 4/5 come from PIT channels 1/2 OUT, which `out_after`'s
        // GATE-stays-level assumption also depends on: GATE2 is wired from this
        // same port's bit 0, and its only writer is that same batch-ending
        // write_io, so GATE cannot move between this read and the batch end
        // either.
        if port == 0x61 {
            if self.lazy_port_reads {
                // Both channels share the SAME elapsed-PIT-clocks conversion
                // (same rate, same batch-entry carry): computed once here rather
                // than twice inside two separate predicted_pit_out calls, since
                // a redundant conversion on this hot path.
                let elapsed_pit_clocks = self.elapsed_pit_clocks();
                let ch1 = self.pit.out_after(1, elapsed_pit_clocks);
                let ch2 = self.pit.out_after(2, elapsed_pit_clocks);
                if let (Some(ch1_out), Some(ch2_out)) = (ch1, ch2) {
                    let value = (self.speaker.control_bits() & 0x03)
                        | (u8::from(ch1_out) << 4)
                        | (u8::from(ch2_out) << 5);
                    return Ok(u32::from(value));
                }
                // BCD fallback: at least one of channel 1/2 is BCD-programmed, so
                // `out_after` conservatively declined. Fall through to the exact
                // non-lazy path below (io_touched set, today's live read) rather
                // than a second implementation of the same bit composition.
            }
            if !skip_io_touched {
                *self.io_touched = true;
            }
            // Bit 4 is the DRAM-refresh heartbeat: PIT channel 1 OUT (the AT
            // refresh timer, mode 2), not the speaker's standalone toggle. The PIT
            // seeds channel 1 at power-on so this pulses without guest programming.
            let value = (self.speaker.control_bits() & 0x03)
                | (u8::from(self.pit.channel_out(1)) << 4)
                | (u8::from(self.pit.channel_out(2)) << 5);
            return Ok(u32::from(value));
        }
        // OPL status reads are intentionally exact. AdLib detection is a timer
        // probe, and letting the poll continue inside an approximate CPU batch
        // can starve the emulated timer progression enough to fail on fast CPU
        // modes. Keep every AdLib/SB OPL read batch-ending.
        if let Some(resolved) = opl_port(port) {
            // Always end the batch on an OPL status read, even under the ring-0 PM
            // monitor. The skip_io_touched exemption exists for the monitor's OWN
            // chipset pokes (the vec13 PIC OCW3 probe), but an OPL poll reflected
            // from a V86 guest is real guest device I/O: it must end the batch so the
            // OPL timer advances BETWEEN polls. Without this the whole AdLib
            // detection loop runs inside one batch, the timer only advances at batch
            // end (after every poll already read a stale 0x00), and detection fails.
            *self.io_touched = true;
            // Charge the poll its real ISA bus time in the fast modes so it
            // cannot outrun the 80 us OPL timer on a fast CPU (folded into the batch
            // device advance in run_until_tick). The 386 modes do not need this
            // charge because their slower clocks already span the window.
            if self.lazy_port_reads {
                *self.isa_io_clocks += isa_io_clocks(self.active_mode);
            }
            // The chip drives only the status byte on reads; data ports read open-bus.
            return Ok(u32::from(self.opl.read_port(resolved).unwrap_or(0xff)));
        }
        // DSP status reads are intentionally exact. SB reset/probe code polls
        // 0x22E for the reset ACK byte, so keeping that loop inside one
        // approximate CPU batch can starve the DSP settle timer.
        // All remaining reads are exact and end the batch. The ring-0 monitor's
        // PIC OCW3 probe still honors the same skip_io_touched gate.
        if !skip_io_touched {
            *self.io_touched = true;
        }
        if let Some(value) = self.mixer.read_port(port) {
            return Ok(u32::from(value));
        }
        if port == WAVETABLE_MPU_BASE {
            let guest_tick = self.guest_tick_now();
            self.pic.set_irq_level(9, self.midi_mpu.irq_level());
            let value = self.wavetable_mpu.read_data_at(guest_tick);
            self.sync_mpu_irq();
            return Ok(u32::from(value));
        }
        if port == WAVETABLE_MPU_BASE + 1 {
            let guest_tick = self.guest_tick_now();
            let value = self.wavetable_mpu.status_at(guest_tick);
            self.sync_mpu_irq();
            return Ok(u32::from(value));
        }
        if port == MIDI_MPU_BASE {
            let guest_tick = self.guest_tick_now();
            self.pic.set_irq_level(9, self.wavetable_mpu.irq_level());
            let value = self.midi_mpu.read_data_at(guest_tick);
            self.sync_mpu_irq();
            return Ok(u32::from(value));
        }
        if port == MIDI_MPU_BASE + 1 {
            let guest_tick = self.guest_tick_now();
            let value = self.midi_mpu.status_at(guest_tick);
            self.sync_mpu_irq();
            return Ok(u32::from(value));
        }
        // AD1848 / Windows Sound System: 4 config-region ports at wss_base plus
        // the 4 codec ports at wss_base+4. read_port takes the in-region offset
        // and returns a u8, so the range MUST be checked before the call. The
        // region (default 0x530-0x537) never overlaps the SB16 (0x220-0x22F),
        // CT1745 mixer (0x224/5), or OPL (0x388/9) ports.
        if let Some(offset) = self.wss_offset(port) {
            return Ok(u32::from(self.wss.read_port(offset)));
        }
        if ide::IdeChannel::owns_port(port) {
            return Ok(u32::from(self.ide.read_port(port).unwrap_or(0xff)));
        }
        if ata::AtaDisk::owns_port(port) {
            // The primary channel: a mounted disk drives the task file; an empty
            // channel reads open-bus (0xFF), so a probe sees no device.
            let value = self
                .ata
                .as_mut()
                .and_then(|d| d.read_port(port))
                .unwrap_or(0xff);
            return Ok(u32::from(value));
        }
        if fdc::Fdc::owns_port(port) {
            return Ok(u32::from(self.fdc.read_port(port).unwrap_or(0xff)));
        }
        if let Some(value) = self.dsp.read_port(port) {
            // A guest ISR acknowledges the DSP interrupt by reading 0x22E (8-bit)
            // or 0x22F (16-bit); that read also clears the mixer's 0x82 source bit.
            if port == 0x22E || port == 0x22F {
                self.mixer.clear_irq_status();
            }
            return Ok(u32::from(value));
        }
        if let Some(value) = self.pit.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.pic.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.dma.read_port(port) {
            return Ok(u32::from(value));
        }
        if port == 0x00e0 {
            return Ok(u32::from(LOTURA_ID_VALUE));
        }
        if port == 0x00e1 {
            return Ok(u32::from(self.active_mode.register_code()));
        }
        if port == 0x00e2 {
            // Lotura POST-pacing flag: 1 = fast (skip cosmetic delays), 0 = full.
            return Ok(u32::from(u8::from(self.fast_post)));
        }
        if port == 0x00e3 {
            // Toka-DOS service status: 0 ok, 1 absent, other = error.
            return Ok(u32::from(self.toka_service_status));
        }
        if port == 0x0092 {
            // System control port A: bit 1 mirrors the A20 gate (the 8042 output
            // port is the single source of truth). Other bits read 0.
            return Ok(u32::from(u8::from(self.keyboard.a20_enabled()) << 1));
        }
        if (0x0200..=0x0207).contains(&port) {
            // Game port with no joystick attached: the four one-shot axis timers
            // (bits 0-3) have no pots to charge through so they read expired (0),
            // and the button inputs (bits 4-7) float high (open switches,
            // active-low) -- the same absent-joystick answer INT 15h AH=84h gives.
            // A routine joystick probe must see "no joystick", not an
            // UnsupportedPort fault that halts the machine. The ISA gameport
            // decodes the whole 0x200-0x207 range as aliases of one register
            // (TSUMERA probes 0x200, not 0x201).
            return Ok(0xf0);
        }
        if let Some(value) = self.unittester.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.rtc.read_port(port) {
            return Ok(u32::from(value));
        }
        if let Some(value) = self.keyboard.read_port(port) {
            // Reading the data register drops the 8042's keyboard or auxiliary
            // output line in the same I/O cycle. Keep the PIC's electrical input
            // in step here so LTIM cannot reassert a byte the guest consumed.
            if port == 0x60 {
                self.pic.set_irq_level(1, self.keyboard.irq1_level());
                self.pic.set_irq_level(12, self.keyboard.irq12_level());
            }
            return Ok(u32::from(value));
        }
        self.device_ports
            .read_port(port)
            .map(u32::from)
            .ok_or(BusError::UnsupportedPort { port })
    }

    fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        core_clocks_so_far: u64,
        cpu_is_ring0_pm: bool,
    ) -> Result<(), BusError> {
        self.core_clocks_so_far = core_clocks_so_far;
        // See read_io's matching comment (V86 trap tax, Part 1): the ring-0
        // monitor's own device pokes (e.g. the vec13 discriminator's PIC OCW3
        // select write) are chipset bookkeeping, not guest-visible activity, so
        // they are exempted from ending the batch in the Approximate class only.
        //
        // A20 carve-out: the batch loop's A20 seam ("any A20 write ... ends this
        // step" -- the before/after compare at batch entry) depends on EVERY
        // write that can move the A20 gate ending the batch, ring-0 or not.
        // Ports 0x92 (system control A), 0x60/0x64 (the 8042 path) can; keep
        // them batch-ending unconditionally. TOKAEMM's a20_apply is PTE-based
        // today (the real gate never drops), so this is belt-and-braces for a
        // future monitor that pokes the real gate, at zero hot-path cost (the
        // monitor's hot pokes are the PIC/EOI ports, not these three).
        //
        // PCI configuration writes are also batch-ending. A BAR or command
        // update rebuilds the direct-RAM map below, so the CPU must discard its
        // cached direct pages before it executes another guest instruction.
        let skip_io_touched = cpu_is_ring0_pm
            && self.lazy_port_reads
            && !matches!(port, 0x60 | 0x64 | 0x92)
            && !(PCI_CONFIG_ADDRESS_PORT..=PCI_CONFIG_DATA_END).contains(&port);
        if !skip_io_touched {
            *self.io_touched = true;
        }
        self.trace.record(
            BusAccessKind::IoWrite,
            u32::from(port),
            width,
            self.wait_states.io,
        );

        let pci_decode = self.vega.memory_decode_key();
        if self.pci.write_io(port, width, value, self.vega) {
            if self.vega.memory_decode_key() != pci_decode {
                self.ram_lookup.rebuild(self.memory.len(), self.vega);
                *self.direct_map_changed = true;
            }
            if let Some(disk) = self.ata.as_mut() {
                self.bmide
                    .synchronize(self.pci.ide_bus_master_enabled(), self.memory, disk);
            }
            return Ok(());
        }

        if let Some(base) = self.pci.ide_bus_master_io_base()
            && bmide::BusMasterIde::owns_io(port, width, base)
        {
            if self.pci.ide_io_enabled() {
                self.bmide
                    .write_io(port, width, value, self.ata.as_mut(), base);
                if let Some(disk) = self.ata.as_mut() {
                    self.bmide
                        .synchronize(self.pci.ide_bus_master_enabled(), self.memory, disk);
                }
            }
            return Ok(());
        }

        if width != BusWidth::Byte {
            // A wider-than-byte port write decomposes into byte cycles, mirroring
            // `read_io`: the low byte goes to the port and each higher byte to the
            // next port (`io_word_sub_port` keeps the IDE/ATA data registers on the
            // same port). The VGA index/data idiom (a single 16-bit `OUT 0x3C4`/
            // `0x3CE`/`0x3D4`) depends on this; it used to halt the VM with
            // WidthMismatch.
            for i in 0..width.bytes() {
                <Self as CpuBus>::write_io(
                    self,
                    io_word_sub_port(port, i),
                    BusWidth::Byte,
                    value >> (8 * i),
                    core_clocks_so_far,
                    cpu_is_ring0_pm,
                )?;
            }
            return Ok(());
        }

        if self.vega.port_disabled(port) {
            return Ok(());
        }

        if let Some(opl_port) = opl_port(port) {
            self.opl.write_port(opl_port, value as u8);
            return Ok(());
        }
        if self.mixer.write_port(port, value as u8) {
            return Ok(());
        }
        if port == WAVETABLE_MPU_BASE {
            let guest_tick = self.guest_tick_now();
            self.wavetable_mpu.write_data(value as u8, guest_tick);
            self.sync_mpu_irq();
            return Ok(());
        }
        if port == WAVETABLE_MPU_BASE + 1 {
            let guest_tick = self.guest_tick_now();
            self.wavetable_mpu.write_command_at(value as u8, guest_tick);
            self.sync_mpu_irq();
            return Ok(());
        }
        if port == MIDI_MPU_BASE {
            let guest_tick = self.guest_tick_now();
            self.midi_mpu.write_data(value as u8, guest_tick);
            self.sync_mpu_irq();
            return Ok(());
        }
        if port == MIDI_MPU_BASE + 1 {
            let guest_tick = self.guest_tick_now();
            self.midi_mpu.write_command_at(value as u8, guest_tick);
            self.sync_mpu_irq();
            return Ok(());
        }
        // AD1848 / Windows Sound System write path. write_port takes the in-region
        // offset and returns (), so the range is checked first (mirrors read_io).
        if let Some(offset) = self.wss_offset(port) {
            self.wss.write_port(offset, value as u8);
            return Ok(());
        }
        if ide::IdeChannel::owns_port(port) {
            self.ide.write_port(port, value as u8);
            return Ok(());
        }
        if ata::AtaDisk::owns_port(port) {
            // Writes to an empty primary channel are dropped; a probe of a bare
            // channel must not fault. A mounted disk takes the task-file write.
            if let Some(disk) = self.ata.as_mut() {
                disk.write_port(port, value as u8);
                self.bmide
                    .synchronize(self.pci.ide_bus_master_enabled(), self.memory, disk);
            }
            return Ok(());
        }
        if fdc::Fdc::owns_port(port) {
            let now_ticks = self.guest_tick_now();
            self.fdc.write_port_at(port, value as u8, now_ticks);
            return Ok(());
        }
        if self.dsp.write_port(port, value as u8) {
            return Ok(());
        }
        if self.dma.write_port(port, value as u8) {
            // The 8237A runs a memory-to-memory block transfer when the guest
            // arms a software DREQ on channel 0 (a write to the request register,
            // port 0x09) with mem-to-mem enabled in the command register. The
            // write above recorded the request; fire the block copy here.
            if port == 0x09 && self.dma.mem_to_mem_request_armed() {
                self.dma.mem_to_mem(self.memory);
                // A DMA block copy wrote guest RAM directly. The run loop honors
                // the flag at the end of the step and drops cached code.
                *self.device_wrote_memory = true;
            }
            return Ok(());
        }
        if port == 0x61 {
            self.speaker.write_control(value as u8);
            self.pit.set_gate(2, value & 1 != 0);
            return Ok(());
        }
        if port == 0x0092 {
            // Fast A20 gate: bit 1 drives A20, routed through the 8042 so every A20
            // method agrees. Bit 0 (fast CPU reset) is not modeled.
            self.keyboard.set_a20(value & 0x02 != 0);
            return Ok(());
        }
        if (0x0200..=0x0207).contains(&port) {
            // Game port (0x200-0x207 aliases): an OUT fires the four axis
            // one-shots. With no joystick they expire immediately, so there is
            // no state to keep.
            return Ok(());
        }
        if port == 0x00e1 {
            if let Some(mode) = GswMode::from_register_code(value as u8) {
                *self.pending_mode = Some(mode);
            }
            return Ok(());
        }
        if port == 0x00e3 {
            // Toka-DOS service command: 1 = Repair. Format and LoadBootRecord were
            // removed with the retired HLE DOS kernel.
            // The run loop performs it after this cycle (it needs &mut self).
            *self.pending_toka_service = Some(value as u8);
            return Ok(());
        }
        if self.unittester.write_port(port, value as u8) {
            return Ok(());
        }
        if port == 0x00e7 {
            // Lotura port 0xE7: bank a code-page font page into the window at
            // CODEPAGE_FONT_WINDOW. sel = cp*3 + size_index where size_index
            // 0=8x16 (4096 bytes), 1=8x14 (3584 bytes), 2=8x8 (2048 bytes).
            // Valid selectors are 0..14 (five code pages, three sizes each).
            // An out-of-range selector is silently ignored.
            let sel = value as usize;
            let cp = sel / 3;
            let size_index = sel % 3;
            if cp < 5 {
                let (size_off, len) = [(0usize, 4096usize), (4096, 3584), (7680, 2048)][size_index];
                let off = cp * 9728 + size_off;
                let page = &izarravm_firmware::CODEPAGE_FONTS[off..off + len];
                for (i, &byte) in page.iter().enumerate() {
                    let _ = self
                        .memory
                        .write_u8(CODEPAGE_FONT_WINDOW as usize + i, byte);
                }
            }
            return Ok(());
        }
        if self.rtc.write_port(port, value as u8) {
            return Ok(());
        }
        if self.serial.write_port(port, value as u8)
            || self.serial2.write_port(port, value as u8)
            || self.lpt.write_port(port, value as u8)
            || self.lpt2.write_port(port, value as u8)
            || (self.vega.port_enabled(port) && self.vega.write_port(port, value as u8))
            || self.pit.write_port(port, value as u8)
            || self.pic.write_port(port, value as u8)
            || self.keyboard.write_port(port, value as u8)
            || self.device_ports.write_port(port, value as u8)
        {
            Ok(())
        } else {
            Err(BusError::UnsupportedPort { port })
        }
    }

    fn interrupt_pending(&self) -> bool {
        self.pic.interrupt_pending()
    }

    fn acknowledge_interrupt(&mut self) -> Option<u8> {
        self.pic.acknowledge()
    }

    #[inline]
    fn requires_step_break(&self) -> bool {
        // The exact condition the batch loop checks after each instruction: a port access touched
        // time-dependent device state, or an HLE software interrupt is pending. The straight-line
        // run executor ends its run on this so the machine services it at the old per-instruction
        // boundary.
        *self.io_touched || self.pending_soft_int.is_some()
    }

    fn interrupt_acknowledge(&mut self, vector: u8, _ax: u16) -> Result<(), BusError> {
        self.trace.record(
            BusAccessKind::InterruptAcknowledge,
            u32::from(vector),
            BusWidth::Byte,
            self.wait_states.io,
        );
        // THE LANDING ADDRESS IS THE ONLY POSTER. A host-serviced BIOS INT is
        // recognized where the dispatch LANDS (`note_stub_fetch` on a
        // per-vector ROM stub), never at the `INT n` opcode: posting here as
        // well double-serviced two standard dispatch shapes (a guest hook
        // chaining to the saved default, and a copied vector landing on
        // another vector's stub). Real-hardware semantics follow from
        // landing-only posting: a hook that fully handles without chaining
        // gets NO HLE service (the hook replaced the ROM), a hook that chains
        // gets exactly one service at the landing, and a copied vector
        // services as the LANDED vector, once.
        //
        // This arm still posts the two shapes whose landing the fetch seam
        // cannot see:
        // (a) raw-program INT 20h/21h: their IVT entries target the low-RAM
        //     IRET at 0x600, not the per-vector table (0x27 IS table-seeded
        //     and rides the fetch seam like everything else).
        if self.program_runtime && matches!(vector, 0x20 | 0x21) {
            *self.pending_soft_int = Some(vector);
            return Ok(());
        }
        // (b) the legacy shared chain target FF00:0000, which period booters
        //     hardcode (IVT[0x13] -> FF00:0000, or a hook chaining there).
        //     That single address serves every vector, so the fetch seam
        //     cannot attribute a landing by address alone: stash the vector
        //     here and let the FF00:0000 fetch post it (consumed there; a
        //     per-vector stub landing also disarms it). Known corner, accepted
        //     for this legacy-only path: a nested intercepted INT inside a
        //     hook body overwrites the stash before the hook chains to
        //     FF00:0000, dropping the outer service; and a stash left armed by
        //     a non-chaining hook posts once if the guest later jumps to
        //     FF00:0000 outside any INT context.
        if self.soft_int_intercepted(vector)? {
            *self.last_int_vector = Some(vector);
        }
        Ok(())
    }
}

impl MachineBus<'_> {
    /// The one interception predicate for host-serviced software interrupts,
    /// shared by the two dispatch seams: `note_stub_fetch` (execution reaching
    /// the vector's ROM stub by any route - an `INT n` opcode's IVT dispatch,
    /// a DPMI host's simulate-real-mode-interrupt far dispatch, or a guest
    /// chaining to a saved default vector) and `interrupt_acknowledge` (the
    /// legacy-chain stash and the raw-program low-RAM vectors).
    ///
    /// The DOS multiplex vector (INT 2Fh) HLE -- including the AX=1686h/1687h
    /// DPMI-install check -- only stands in for a real handler when none
    /// exists: once a guest hooks IVT[0x2F] (JEMMEX, DOS/32A's stub) the hook
    /// owns it, same for the absent-resident-API vectors. In booter-inert mode
    /// 2Fh also stands down so a self-booting disk owns it through the IVT.
    /// The pure DOS vectors 0x20-0x2E are not intercepted at all outside the
    /// raw-program runtime now that the Rust DOS kernel is retired, and INT
    /// 67h is never intercepted (the TOKAEMM guest driver owns the EMS API).
    fn soft_int_intercepted(&mut self, vector: u8) -> Result<bool, BusError> {
        let dos_multiplex = vector == 0x2F && self.vector_points_at_rom_iret(vector)?;
        let absent_resident_api = matches!(vector, 0x5C | 0x60 | 0x68 | 0x6F | 0x7A | 0x86 | 0xE4)
            && self.vector_points_at_rom_iret(vector)?;
        // A `new_raw_program` machine keeps INT 20h/21h/27h intercepted so the run
        // loop's guarded raw-program arm (`handle_raw_program_int`) services them.
        // Outside that runtime nothing intercepts the pure-DOS vectors any more.
        let raw_program_vector = self.program_runtime && matches!(vector, 0x20 | 0x21 | 0x27);
        let intercepted = matches!(
            vector,
            0x10 | 0x11 | 0x12 | 0x13 | 0x14 | 0x15 | 0x17 | 0x18 | 0x19 | 0x1A | 0x40 | 0x42
        ) || raw_program_vector
            || absent_resident_api
            || dos_multiplex;
        Ok(intercepted && !(self.booter_inert && vector == 0x2F))
    }

    /// The fetch-seam half of software-interrupt interception: execution has
    /// reached a per-vector ROM stub entry (see BIOS_INT_STUB_TABLE_ROM_OFFSET)
    /// or the legacy shared chain target FF00:0000. Posts the vector for the
    /// run loop's deferred HLE dispatch. Both landing shapes lead with a NOP,
    /// which guarantees a post-instruction break fires before the IRET, so the
    /// service still sees the INT frame on the stack. This is the ONLY poster
    /// for every dispatch route - INT opcode, a DPMI host's simulated
    /// real-mode interrupt, a guest chaining to a saved default - so each
    /// dispatch services exactly once. Repeated fetch charges of the same
    /// visit are absorbed by the pending_soft_int check (the pending vector is
    /// only cleared at the next batch entry, after the service ran and
    /// execution moved to the IRET byte, whose odd offset never posts).
    ///
    /// `address` is a LINEAR address on both seams (`note_code_fetch_linear`
    /// per cold-fetched byte, `charge_instruction_fetch_run` per cached run):
    /// the stub table's identity is architectural, and a paging guest that
    /// shadows the BIOS F-page (JemmEx) still dispatches through linear
    /// FF00:02xx while backing it with another physical page. Residual
    /// divergence, recorded per review: a pmode guest running unrelated code
    /// AT linear 0xFF0xx/0xFF2xx (mapped wherever) posts a bogus service;
    /// deliberate-hostile only, no real DOS stack does this.
    pub(super) fn note_stub_fetch(&mut self, address: u32) {
        if address == BIOS_LEGACY_IRET_LINEAR {
            // The legacy shared nop;iret at FF00:0000: one address for every
            // vector, so attribution comes from the stash the `INT n` opcode
            // arm left behind. Consumed here; a landing with no armed stash
            // (a simulated jump with no preceding INT) stays a no-op.
            let stashed = self.last_int_vector.take();
            if self.pending_soft_int.is_none()
                && let Some(vector) = stashed
                && self.soft_int_intercepted(vector).unwrap_or(false)
            {
                *self.pending_soft_int = Some(vector);
            }
            return;
        }
        let offset = address.wrapping_sub(BIOS_INT_STUB_TABLE_LINEAR);
        if offset >= BIOS_INT_STUB_TABLE_LEN || offset & 1 != 0 {
            return; // outside the table, or the IRET byte (mid-stub resume)
        }
        let vector = (offset / 2) as u8;
        let intercepted = self.soft_int_intercepted(vector).unwrap_or(false);
        // An INTERCEPTED landing supersedes any armed legacy stash: the
        // dispatch the stash described has been attributed here by address
        // instead. A NON-intercepted landing must leave the stash alone - its
        // ack never armed it, and the machine's own timer ISR chains INT 1Ch
        // (stub 0x1C) every tick, so an unconditional disarm would race a
        // hardware IRQ against a live hook-chain attribution and drop the
        // chained service (round-2 review finding 1).
        if intercepted {
            *self.last_int_vector = None;
        }
        if let Some(pending) = *self.pending_soft_int {
            // The dedup above is vector-blind; the only legitimate repeat is a
            // re-charge of the SAME pending visit (round-1 review finding 3).
            debug_assert!(
                pending == vector,
                "stub fetch posted vector {vector:#04x} while {pending:#04x} is still pending"
            );
            return;
        }
        if intercepted {
            *self.pending_soft_int = Some(vector);
        }
    }

    fn vector_points_at_rom_iret(&mut self, vector: u8) -> Result<bool, BusError> {
        let address = usize::from(vector) * 4;
        let off = self.memory.read_u16(address)?;
        let seg = self.memory.read_u16(address + 2)?;
        // A vector is "still the BIOS default" when it points at its per-vector
        // ROM stub. The legacy shared IRET at FF00:0000 is also accepted:
        // period booters hardcode that address (IVT[0x13] -> FF00:0000 to chain
        // disk calls), and pre-table guests may restore a saved default.
        Ok(seg == BIOS_ROM_IRET_SEG && (off == bios_int_stub_off(vector) || off == 0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DevicePorts {
    ports: std::collections::BTreeMap<u16, u8>,
}

impl Default for DevicePorts {
    fn default() -> Self {
        let mut ports = std::collections::BTreeMap::new();
        for port in known_passive_ports() {
            ports.insert(port, 0xff);
        }
        Self { ports }
    }
}

impl DevicePorts {
    fn read_port(&self, port: u16) -> Option<u8> {
        self.ports.get(&port).copied()
    }

    fn write_port(&mut self, port: u16, value: u8) -> bool {
        let Some(slot) = self.ports.get_mut(&port) else {
            return false;
        };
        *slot = value;
        true
    }
}

fn known_passive_ports() -> impl Iterator<Item = u16> {
    let ranges = [
        0x0000..=0x000f, // DMA controller 1
        0x0062..=0x0063, // system control port B (speaker now owns 0x61)
        0x0080..=0x009f, // DMA page registers
        0x00c0..=0x00df, // DMA controller 2
        0x0220..=0x022f, // Sound Blaster base
        0x0280..=0x028f, // C/MS Game Blaster alternate-base probe range (Prince of
        // Persia's sound detect reads 0x283 and must see open bus,
        // not a fault -- the port-0x201 joystick-stub precedent)
        0x0388..=0x038b, // OPL2/OPL3 (intercepted by the chip, kept as a fallback)
        0x03b0..=0x03df, // MDA/CGA/EGA/VGA registers
        0x5658..=0x565b, // VMware backdoor probe (DX=0x5658, EAX='VMXh'): real,
                         // non-VMware hardware has nothing at this port, so a guest's `IN
                         // EAX, DX` detection probe must read open bus (all-ones), never the
                         // VMware magic response and never an UnsupportedPort fault. A dword
                         // IN decomposes into four byte reads at 0x5658-0x565b (the same
                         // io_word_sub_port widening as every other wide port access), so all
                         // four bytes are covered here. JEMMEX runs this probe during its own
                         // hypervisor-presence check and used to halt the machine with
                         // CpuError("unsupported I/O port 0x5658") before this stub existed.
    ];
    ranges.into_iter().flatten()
}

impl MachineBus<'_> {
    fn sync_mpu_irq(&mut self) {
        self.pic.set_irq_level(
            9,
            self.wavetable_mpu.irq_level() || self.midi_mpu.irq_level(),
        );
    }

    /// In-region offset (0..=7) of `port` within the AD1848 / WSS port window
    /// `[wss_base, wss_base + 8)`, or `None` when the codec is disabled or the
    /// port lies outside the window. The codec's read_port/write_port take this
    /// offset; the caller dispatches to them only on `Some`.
    fn wss_offset(&self, port: u16) -> Option<u16> {
        if !self.wss_enabled {
            return None;
        }
        port.checked_sub(self.wss_base).filter(|&off| off < 8)
    }

    /// Apply the A20 gate to a physical address before it reaches memory. The gate
    /// is the single 8042 output-port bit (shared with fast-A20 port 0x92); when
    /// it is closed, address line 20 is forced low. This is the motherboard-level
    /// effect, so it sits at the one CPU bus seam and covers fetches and data
    /// alike. Host-side pokes (write_physical_u8 and friends) deliberately bypass
    /// it: they address exact physical cells, not the guest's gated bus.
    fn apply_a20(&self, address: u32) -> u32 {
        if self.keyboard.a20_enabled() {
            address
        } else {
            address & A20_MASK
        }
    }

    #[inline]
    fn direct_ram_range(&self, address: u32, width: BusWidth) -> Option<(usize, usize)> {
        self.direct_ram_bytes(address, width.bytes() as usize)
    }

    #[inline]
    pub(super) fn direct_ram_bytes(&self, address: u32, bytes: usize) -> Option<(usize, usize)> {
        let start = address as usize;
        let end = start.checked_add(bytes)?;
        if end <= 0x000A_0000 && end <= self.memory.len() {
            return Some((start, end));
        }
        self.ram_lookup.direct_bytes(address, bytes)
    }

    #[inline]
    fn direct_page_ram_bytes(
        &self,
        address: u32,
        bytes: usize,
        access_width: BusWidth,
    ) -> Option<(u32, usize, usize)> {
        let gated = self.apply_a20(address);
        if gated != address || bytes == 0 {
            return None;
        }
        if should_split(gated, access_width)
            || ((gated as usize & RAM_LOOKUP_PAGE_MASK) + bytes > RAM_LOOKUP_PAGE_SIZE)
        {
            return None;
        }
        self.direct_ram_bytes(gated, bytes)
            .map(|(start, end)| (gated, start, end))
    }

    fn read_phys_u8(&mut self, address: u32) -> Result<u8, BusError> {
        let mut byte = [0];
        self.read_phys(address, &mut byte)?;
        Ok(byte[0])
    }

    fn read_phys(&mut self, address: u32, out: &mut [u8]) -> Result<(), BusError> {
        let width = out.len();
        if width == 0 {
            return Ok(());
        }

        if let Some((start, end)) = self.direct_ram_bytes(address, width) {
            out.copy_from_slice(&self.memory.as_slice()[start..end]);
            return Ok(());
        }

        if let Some(offset) = rom_offset(address, width) {
            out.copy_from_slice(&self.rom[offset..offset + width]);
            return Ok(());
        }

        if self.vega.read_memory(address, out) {
            return Ok(());
        }

        if is_open_bus_uma(address, width) {
            // Unoccupied upper memory: open bus reads as 0xFF, matching a real
            // machine's floating data bus over an adapter-free UMA hole.
            out.fill(0xff);
            return Ok(());
        }

        let end = address as usize + width;
        if end <= self.memory.len() {
            out.copy_from_slice(&self.memory.as_slice()[address as usize..end]);
            return Ok(());
        }

        // No device or memory answers this physical address. The data lines
        // float high instead of raising a CPU-visible memory fault.
        out.fill(0xff);
        Ok(())
    }

    fn write_memory_byte(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        if let Some((addr, _)) = self.direct_ram_bytes(address, 1) {
            return self.memory.write_u8(addr, value);
        }

        if rom_offset(address, 1).is_some() {
            return Ok(());
        }

        if is_open_bus_uma(address, 1) {
            // Unoccupied upper memory: open bus, a write with nothing wired to
            // receive it.
            return Ok(());
        }

        if self.vega.write_memory_u8(address, value) {
            return Ok(());
        }

        if (address as usize) < self.memory.len() {
            return self.memory.write_u8(address as usize, value);
        }

        // Writes to an unclaimed physical address have no receiver.
        Ok(())
    }

    /// Wait-states to charge for a DATA access at the post-A20 physical `address`,
    /// routed through the cosmetic cache so its tag state stays warm. The cache
    /// tiers ONLY cacheable RAM: a ROM or video/MMIO window keeps its existing
    /// `memory_wait_states` cost UNCHANGED (it is never cached, so it must not warm
    /// the model nor be re-timed by it). Cacheable RAM (conventional `< 0xA0000`
    /// and any extended RAM that is not a device window) is tiered, and the resolved
    /// tier's per-mode cost is charged.
    fn data_access_wait_states(&mut self, address: u32, width: BusWidth) -> u8 {
        if address >= 0x000A_0000 && self.is_device_window(address, width) {
            // Device/ROM: untiered, unchanged timing (both classes).
            return self.memory_wait_states(address);
        }
        if self.flat_data_cost {
            // Approximate class (486/586): charge the flat L1-resident cost and skip
            // the per-access tag-array tiering. The
            // benchmarks are L1-resident so cyc/iter stays near the accurate model;
            // the win is skipping ~3M tag lookups per run. Guest-invisible: only time.
            return self.cache.cost.l1;
        }
        self.cache.data_wait_states(address, width)
    }

    /// Wait-states for a single code-fetch byte at the post-A20 physical `address`.
    /// Code in cacheable RAM is charged the per-mode L1 constant (code is assumed
    /// I-cache resident); code fetched from ROM/device keeps `memory_wait_states`,
    /// so firmware/POST and any execution out of a device window are unchanged.
    fn code_fetch_wait_states(&self, address: u32) -> u8 {
        if address >= 0x000A_0000 && self.is_device_window(address, BusWidth::Byte) {
            self.memory_wait_states(address)
        } else {
            self.cache.code_fetch_wait_states()
        }
    }

    /// True iff `address` (post-A20, width `width`) lands in a ROM or video/MMIO
    /// window the cache must not tier. Mirrors the device-classification arm of
    /// `memory_wait_states_device` (the `wait_states.rom`/`wait_states.video`
    /// branches); the fall-through (cacheable RAM) returns false. Only called for
    /// `address >= 0xA0000`, so conventional RAM never reaches here.
    fn is_device_window(&self, address: u32, width: BusWidth) -> bool {
        let bytes = width.bytes() as usize;
        rom_offset(address, bytes).is_some() || self.vega.owns_memory(address, bytes)
    }

    #[inline]
    fn memory_wait_states(&self, address: u32) -> u8 {
        // Conventional RAM (below the 0xA0000 video aperture) is never overlapped
        // by a ROM, VGA, Margo, or Distira window, so it always runs at RAM speed.
        // The hot fetch/data path hits this on every access, so keep it a tiny
        // inlinable check and defer the device-window gauntlet to a cold helper.
        // This matches the fall-through the gauntlet would reach anyway (it already
        // classifies by the base address only).
        if address < 0x000A_0000 {
            return self.wait_states.ram;
        }
        self.memory_wait_states_device(address)
    }

    #[cold]
    fn memory_wait_states_device(&self, address: u32) -> u8 {
        if rom_offset(address, 1).is_some() {
            self.wait_states.rom
        } else if self.vega.owns_memory(address, 1) {
            // The Approximate class charges the era bus latency of a real video
            // card (see `video_wait_states_approx`); the Accurate class keeps the
            // frozen profile value bit-for-bit.
            if self.active_mode.uses_approximate_timing() {
                video_wait_states_approx(self.active_mode.persona())
            } else {
                self.wait_states.video
            }
        } else {
            self.wait_states.ram
        }
    }
}

fn should_split(address: u32, width: BusWidth) -> bool {
    match width {
        BusWidth::Byte => false,
        BusWidth::Word => address & 0x1 != 0,
        BusWidth::Dword => address & 0x3 != 0,
    }
}

fn rom_offset(address: u32, width: usize) -> Option<usize> {
    let offset = if (HIGH_ROM_BASE..=u32::MAX).contains(&address) {
        address.wrapping_sub(HIGH_ROM_BASE)
    } else if (LOW_BIOS_BASE..LOW_BIOS_BASE + BIOS_ROM_SIZE as u32).contains(&address) {
        address - LOW_BIOS_BASE
    } else {
        return None;
    } as usize;

    (offset + width <= BIOS_ROM_SIZE).then_some(offset)
}

/// True if `address` (for an access of `width` bytes, entirely) falls in the
/// unoccupied part of the upper-memory area: the UMB-able holes between the
/// video option ROM span and the system BIOS, 0xC8000-0xEFFFF. On a real
/// machine nothing answers there unless an adapter or a memory manager's
/// page-frame claims it; this machine's own occupants (VGA BIOS data tables,
/// the code-page font bank, TOKAEMM's linear-to-extended-RAM UMB remap) all
/// live below 0xC8000 or are reached through paging at a physical address
/// above 1 MiB, so this range check never needs to special-case them.
///
/// Guests that probe the UMA for a free window (JEMMEX and other EMS/UMB
/// managers scanning for a page frame) rely on this reading as open bus
/// (conventionally 0xFF, writes ignored), exactly like the existing
/// 0x201/0x280-0x28F/0x5658 open-bus port conventions but for memory instead
/// of I/O space.
fn is_open_bus_uma(address: u32, width: usize) -> bool {
    let uma_occupied_end = UPPER_MEMORY_BASE + VGA_BIOS_SPAN_SIZE;
    let Some(end) = address.checked_add(width as u32) else {
        return false;
    };
    address >= uma_occupied_end && end <= SYSTEM_ROM_BASE
}
