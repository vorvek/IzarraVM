// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl CpuGsw {
    const MAX_BUDGETED_REP_ITERATIONS: u32 = 4_096;

    fn index_offset(&self, index: u8, address_size: AddressSize) -> u32 {
        match address_size {
            AddressSize::Word => u32::from(self.read_gpr16(index)),
            AddressSize::Dword => self.read_gpr32(index),
        }
    }

    fn string_count(&self, address_size: AddressSize) -> u32 {
        self.index_offset(1, address_size) // CX / ECX
    }

    fn decrement_string_count(&mut self, address_size: AddressSize) {
        self.decrement_string_count_by(address_size, 1);
    }

    fn decrement_string_count_by(&mut self, address_size: AddressSize, amount: u32) {
        match address_size {
            AddressSize::Word => {
                let cx = self.read_gpr16(1).wrapping_sub(amount as u16);
                self.write_gpr16(1, cx);
            }
            AddressSize::Dword => {
                let ecx = self.read_gpr32(1).wrapping_sub(amount);
                self.write_gpr32(1, ecx);
            }
        }
    }

    fn rep_core_clocks(op: StringOp) -> u32 {
        match op {
            StringOp::Ins => 15,
            StringOp::Outs => 14,
            _ => 4,
        }
    }

    fn rep_memory_accesses(op: StringOp) -> u64 {
        match op {
            StringOp::Movs | StringOp::Cmps => 2,
            StringOp::Scas | StringOp::Stos | StringOp::Lods | StringOp::Ins | StringOp::Outs => 1,
        }
    }

    fn rep_chunk_limit<B: CpuBus>(&self, bus: &B, op: StringOp, width: BusWidth) -> Option<u32> {
        let budget = self.rep_execution.budget?;
        let bus_growth = bus
            .in_batch_scaled_bus_clocks()
            .saturating_sub(budget.bus_at_entry);
        let used = self.core_clocks_so_far.saturating_add(bus_growth);
        let (num, den) = level_timing(self.persona());
        let core_upper = if self.rep_resume_active {
            0
        } else {
            u64::from(Self::rep_core_clocks(op))
                .saturating_mul(u64::from(num))
                .saturating_add(u64::from(den) - 1)
                / u64::from(den)
        };
        let byte_cost = bus.rep_data_byte_cost_upper();
        // A misaligned wide access may split into byte cycles. Use that larger cost as the
        // admission bound even though the aligned bulk path normally charges one wide cycle.
        let access_upper = byte_cost.saturating_mul(u64::from(width.bytes()));
        let per_iteration = access_upper.saturating_mul(Self::rep_memory_accesses(op));
        let paging_setup = if self.is_paging_enabled() {
            let Some(walk_cost) = bus.rep_page_walk_cost_upper() else {
                return Some(0);
            };
            let translations_per_operand = if width == BusWidth::Byte { 1 } else { 2 };
            walk_cost
                .saturating_mul(Self::rep_memory_accesses(op))
                .saturating_mul(translations_per_operand)
        } else {
            0
        };
        let available = budget
            .cap
            .saturating_sub(used)
            .saturating_sub(core_upper)
            .saturating_sub(paging_setup);
        let limit = available
            .checked_div(per_iteration)
            .unwrap_or(u64::from(Self::MAX_BUDGETED_REP_ITERATIONS))
            .min(u64::from(Self::MAX_BUDGETED_REP_ITERATIONS)) as u32;
        Some(limit)
    }

    fn rep_budget_exhausted<B: CpuBus>(&self, bus: &B, op: StringOp) -> bool {
        let Some(budget) = self.rep_execution.budget else {
            return false;
        };
        let bus_growth = bus
            .in_batch_scaled_bus_clocks()
            .saturating_sub(budget.bus_at_entry);
        let (num, den) = level_timing(self.persona());
        let core_upper = if self.rep_resume_active {
            0
        } else {
            u64::from(Self::rep_core_clocks(op))
                .saturating_mul(u64::from(num))
                .saturating_add(u64::from(den) - 1)
                / u64::from(den)
        };
        self.core_clocks_so_far
            .saturating_add(bus_growth)
            .saturating_add(core_upper)
            >= budget.cap
    }

    fn read_string_src<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
        let offset = self.index_offset(6, address_size); // SI / ESI
        self.read_memory_bus_width(bus, segment, offset, width, BusAccessKind::DataRead)
    }

    fn read_string_dst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let offset = self.index_offset(7, address_size); // DI / EDI
        self.read_memory_bus_width(
            bus,
            SegmentIndex::Es,
            offset,
            width,
            BusAccessKind::DataRead,
        )
    }

    fn acc_read(&self, width: BusWidth) -> u32 {
        match width {
            BusWidth::Byte => u32::from(self.read_gpr8(0)),
            BusWidth::Word => u32::from(self.read_gpr16(0)),
            BusWidth::Dword => self.read_gpr32(0),
        }
    }

    fn acc_write(&mut self, width: BusWidth, value: u32) {
        match width {
            BusWidth::Byte => self.write_gpr8(0, value as u8),
            BusWidth::Word => self.write_gpr16(0, value as u16),
            BusWidth::Dword => self.write_gpr32(0, value),
        }
    }

    fn write_string_dst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        width: BusWidth,
        value: u32,
    ) -> ExecResult<()> {
        let offset = self.index_offset(7, address_size); // DI / EDI
        self.write_memory_bus_width(
            bus,
            SegmentIndex::Es,
            offset,
            width,
            value,
            BusAccessKind::DataWrite,
        )
    }

    fn string_forward_chunk_iterations(
        &self,
        segment: SegmentIndex,
        offset: u32,
        address_size: AddressSize,
        width: BusWidth,
        count: u32,
    ) -> u32 {
        if count == 0 {
            return 0;
        }
        let bytes = width.bytes() as usize;
        let mut max_bytes = count as usize * bytes;
        let address_remaining = match address_size {
            AddressSize::Word => 0x1_0000usize - (offset as usize & 0xffff),
            AddressSize::Dword => (u32::MAX - offset) as usize + 1,
        };
        max_bytes = max_bytes.min(address_remaining);

        let descriptor = self.registers.segment(segment);
        if descriptor.base != 0 || descriptor.limit != u32::MAX {
            if offset > descriptor.limit {
                return 0;
            }
            max_bytes = max_bytes.min((descriptor.limit - offset) as usize + 1);
        }

        let linear = descriptor.base.wrapping_add(offset);
        max_bytes = max_bytes.min(0x1000 - (linear as usize & 0x0fff));
        (max_bytes / bytes).min(count as usize) as u32
    }

    fn read_direct_string_value<B: CpuBus>(
        &mut self,
        bus: &mut B,
        physical: u32,
        width: BusWidth,
    ) -> Result<Option<u32>, BusError> {
        let access = width.bytes() as usize;
        if bus.direct_memory_bytes(physical, access, width, BusAccessKind::DataRead) != access {
            return Ok(None);
        }
        let mut bytes = [0u8; 4];
        let got = bus.read_memory_bytes_direct(
            physical,
            &mut bytes[..access],
            width,
            BusAccessKind::DataRead,
        )?;
        debug_assert!(got == 0 || got == access);
        if got != access {
            return Ok(None);
        }
        self.record_data_read(BusAccessKind::DataRead, true);
        Ok(Some(match width {
            BusWidth::Byte => u32::from(bytes[0]),
            BusWidth::Word => u32::from(u16::from_le_bytes([bytes[0], bytes[1]])),
            BusWidth::Dword => u32::from_le_bytes(bytes),
        }))
    }

    fn ranges_overlap(a: u32, b: u32, bytes: usize) -> bool {
        let a = a as usize;
        let b = b as usize;
        let a_end = a.saturating_add(bytes);
        let b_end = b.saturating_add(bytes);
        a < b_end && b < a_end
    }

    fn string_step<B: CpuBus>(
        &mut self,
        bus: &mut B,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
    ) -> ExecResult<()> {
        let bytes = width.bytes();
        match op {
            StringOp::Movs => {
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(6, address_size, bytes);
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Cmps => {
                let a = self.read_string_src(bus, prefixes, address_size, width)?;
                let b = self.read_string_dst(bus, address_size, width)?;
                self.alu_sub(a, b, 0, width); // flags only: [DS:SI] - [ES:DI]
                self.adjust_index_register(6, address_size, bytes);
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Scas => {
                let a = self.acc_read(width);
                let b = self.read_string_dst(bus, address_size, width)?;
                self.alu_sub(a, b, 0, width); // flags only: accumulator - [ES:DI]
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Stos => {
                let value = self.acc_read(width);
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Lods => {
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                self.acc_write(width, value);
                self.adjust_index_register(6, address_size, bytes);
            }
            StringOp::Ins => {
                // INS: [ES:DI] <- port[DX]. ES cannot be overridden.
                let value = bus.read_io(
                    self.read_gpr16(2),
                    width,
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                self.write_string_dst(bus, address_size, width, value)?;
                self.adjust_index_register(7, address_size, bytes);
            }
            StringOp::Outs => {
                // OUTS: port[DX] <- [DS:SI] (segment overridable).
                let value = self.read_string_src(bus, prefixes, address_size, width)?;
                bus.write_io(
                    self.read_gpr16(2),
                    width,
                    value,
                    self.core_clocks_so_far,
                    self.is_ring0_protected(),
                )?;
                self.adjust_index_register(6, address_size, bytes);
            }
        }
        Ok(())
    }

    /// Diagnostic store watchpoint for the string paths. The bulk routes below hand whole
    /// spans straight to the bus, bypassing the per-store checks in `memory.rs`, so a
    /// `REP MOVS`/`REP STOS` into the watched range would otherwise be invisible.
    #[cfg(feature = "watch-write")]
    #[inline(always)]
    fn watch_bulk_write(&self, context: &str, dst: u32, bytes: u32) {
        if crate::write_watch_hits(crate::write_watch_packed(), dst, bytes) {
            crate::report_write_watch(
                context,
                self.registers.cs().selector,
                self.registers.eip,
                dst,
                bytes,
                0,
                self.registers.segment(SegmentIndex::Es).selector,
                self.registers.edi(),
                self.registers.segment(SegmentIndex::Ds).selector,
                self.registers.esi(),
            );
        }
    }

    fn finish_buffered_movs_first<B: CpuBus>(
        &mut self,
        bus: &mut B,
        dst: u32,
        width: BusWidth,
        value: u32,
        address_size: AddressSize,
    ) -> ExecResult<FastStringResult> {
        #[cfg(feature = "watch-write")]
        self.watch_bulk_write("movs1", dst, width.bytes());
        let write = bus.write_memory_direct(dst, width, value, BusAccessKind::DataWrite)?;
        self.record_data_write(BusAccessKind::DataWrite, write.direct);
        self.adjust_index_register(6, address_size, width.bytes());
        self.adjust_index_register(7, address_size, width.bytes());
        self.decrement_string_count(address_size);
        Ok(FastStringResult {
            iterations: 1,
            stop: false,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn try_run_string_fast<B: CpuBus>(
        &mut self,
        bus: &mut B,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
        rep: RepKind,
        max_iterations: u32,
    ) -> ExecResult<Option<FastStringResult>> {
        if self.flag(FLAG_DF) {
            return Ok(None);
        }

        let count = self.string_count(address_size).min(max_iterations);
        if count == 0 {
            return Ok(Some(FastStringResult {
                iterations: 0,
                stop: false,
            }));
        }

        let access = width.bytes() as usize;
        match op {
            StringOp::Movs => {
                let src_segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
                let src_off = self.index_offset(6, address_size);
                let dst_off = self.index_offset(7, address_size);
                let iterations = self
                    .string_forward_chunk_iterations(
                        src_segment,
                        src_off,
                        address_size,
                        width,
                        count,
                    )
                    .min(self.string_forward_chunk_iterations(
                        SegmentIndex::Es,
                        dst_off,
                        address_size,
                        width,
                        count,
                    ));
                if iterations == 0 {
                    return Ok(None);
                }
                let bytes = iterations as usize * access;
                let (_, src) =
                    self.translate_segmented(bus, src_segment, src_off, width.bytes(), false)?;
                let Some(first) = self.read_direct_string_value(bus, src, width)? else {
                    return Ok(None);
                };
                let (_, dst) =
                    self.translate_segmented(bus, SegmentIndex::Es, dst_off, width.bytes(), true)?;
                let bulk_direct = !Self::ranges_overlap(src, dst, bytes)
                    && bus.direct_memory_bytes(src, bytes, width, BusAccessKind::DataRead) == bytes
                    && bus.direct_memory_bytes(dst, bytes, width, BusAccessKind::DataWrite)
                        == bytes;
                if !bulk_direct {
                    return self
                        .finish_buffered_movs_first(bus, dst, width, first, address_size)
                        .map(Some);
                }

                let mut buf = [0u8; 4096];
                let first_bytes = first.to_le_bytes();
                buf[..access].copy_from_slice(&first_bytes[..access]);
                if bytes > access {
                    let got = bus.read_memory_bytes_direct(
                        src.wrapping_add(access as u32),
                        &mut buf[access..bytes],
                        width,
                        BusAccessKind::DataRead,
                    )?;
                    if got != bytes - access {
                        return self
                            .finish_buffered_movs_first(bus, dst, width, first, address_size)
                            .map(Some);
                    }
                }
                #[cfg(feature = "watch-write")]
                self.watch_bulk_write("movs", dst, bytes as u32);
                let put = bus.write_memory_bytes_direct(
                    dst,
                    &buf[..bytes],
                    width,
                    BusAccessKind::DataWrite,
                )?;
                if put != bytes {
                    return Ok(None);
                }
                if bytes > access {
                    let remaining_dst = dst.wrapping_add(access as u32);
                    // G2 out of scope: bulk MOVS already streamed the new bytes into the
                    // destination through write_memory_bytes_direct above, so the old bytes are
                    // gone. A pre-read to compare would be an O(n) tax on the fast string path;
                    // this invalidation stays unconditional.
                    self.note_code_write(remaining_dst, (bytes - access) as u32);
                    self.record_write_page(remaining_dst);
                }
                self.adjust_index_register(6, address_size, bytes as u32);
                self.adjust_index_register(7, address_size, bytes as u32);
                self.decrement_string_count_by(address_size, iterations);
                self.perf.data_direct_reads += u64::from(iterations - 1);
                self.perf.data_direct_writes += u64::from(iterations);
                Ok(Some(FastStringResult {
                    iterations,
                    stop: false,
                }))
            }
            StringOp::Stos => {
                let dst_off = self.index_offset(7, address_size);
                let iterations = self.string_forward_chunk_iterations(
                    SegmentIndex::Es,
                    dst_off,
                    address_size,
                    width,
                    count,
                );
                if iterations == 0 {
                    return Ok(None);
                }
                let bytes = iterations as usize * access;
                let (_, dst) =
                    self.translate_segmented(bus, SegmentIndex::Es, dst_off, bytes as u32, true)?;
                if bus.direct_memory_bytes(dst, bytes, width, BusAccessKind::DataWrite) != bytes {
                    return Ok(None);
                }

                let value = self.acc_read(width);
                let mut pattern = [0u8; 4];
                match width {
                    BusWidth::Byte => pattern[0] = value as u8,
                    BusWidth::Word => pattern[..2].copy_from_slice(&(value as u16).to_le_bytes()),
                    BusWidth::Dword => pattern.copy_from_slice(&value.to_le_bytes()),
                }
                let mut buf = [0u8; 4096];
                for chunk in buf[..bytes].chunks_mut(access) {
                    chunk.copy_from_slice(&pattern[..access]);
                }
                #[cfg(feature = "watch-write")]
                self.watch_bulk_write("stos", dst, bytes as u32);
                let put = bus.write_memory_bytes_direct(
                    dst,
                    &buf[..bytes],
                    width,
                    BusAccessKind::DataWrite,
                )?;
                if put != bytes {
                    return Ok(None);
                }
                self.adjust_index_register(7, address_size, bytes as u32);
                self.decrement_string_count_by(address_size, iterations);
                self.perf.data_direct_writes += u64::from(iterations);
                Ok(Some(FastStringResult {
                    iterations,
                    stop: false,
                }))
            }
            StringOp::Lods => {
                let src_segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
                let src_off = self.index_offset(6, address_size);
                let iterations = self.string_forward_chunk_iterations(
                    src_segment,
                    src_off,
                    address_size,
                    width,
                    count,
                );
                if iterations == 0 {
                    return Ok(None);
                }
                let bytes = iterations as usize * access;
                let (_, src) =
                    self.translate_segmented(bus, src_segment, src_off, bytes as u32, false)?;
                if bus.direct_memory_bytes(src, bytes, width, BusAccessKind::DataRead) != bytes {
                    return Ok(None);
                }

                let mut buf = [0u8; 4096];
                let got = bus.read_memory_bytes_direct(
                    src,
                    &mut buf[..bytes],
                    width,
                    BusAccessKind::DataRead,
                )?;
                if got != bytes {
                    return Ok(None);
                }
                let last = &buf[bytes - access..bytes];
                let value = match width {
                    BusWidth::Byte => u32::from(last[0]),
                    BusWidth::Word => u32::from(u16::from_le_bytes([last[0], last[1]])),
                    BusWidth::Dword => u32::from_le_bytes([last[0], last[1], last[2], last[3]]),
                };
                self.acc_write(width, value);
                self.adjust_index_register(6, address_size, bytes as u32);
                self.decrement_string_count_by(address_size, iterations);
                self.perf.data_direct_reads += u64::from(iterations);
                Ok(Some(FastStringResult {
                    iterations,
                    stop: false,
                }))
            }
            StringOp::Cmps => {
                let src_segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
                let src_off = self.index_offset(6, address_size);
                let dst_off = self.index_offset(7, address_size);
                if self.string_forward_chunk_iterations(
                    src_segment,
                    src_off,
                    address_size,
                    width,
                    1,
                ) == 0
                    || self.string_forward_chunk_iterations(
                        SegmentIndex::Es,
                        dst_off,
                        address_size,
                        width,
                        1,
                    ) == 0
                {
                    return Ok(None);
                }
                let (_, src) =
                    self.translate_segmented(bus, src_segment, src_off, width.bytes(), false)?;
                let Some(a) = self.read_direct_string_value(bus, src, width)? else {
                    return Ok(None);
                };
                let (_, dst) =
                    self.translate_segmented(bus, SegmentIndex::Es, dst_off, width.bytes(), false)?;
                let b = if let Some(value) = self.read_direct_string_value(bus, dst, width)? {
                    value
                } else {
                    let read = bus.read_memory_direct(dst, width, BusAccessKind::DataRead)?;
                    self.record_data_read(BusAccessKind::DataRead, read.direct);
                    // DELIBERATELY not fed to the slow-read page histogram
                    // (`IZARRAVM_SLOW_READ_HISTO`): every other contributor to `data_slow_reads`
                    // buckets a LINEAR page, and `dst` here is already translated. Mixing the two
                    // address spaces in one table would be worse than the omission, which the
                    // report makes visible by printing the histogram total against
                    // `data_slow_reads` -- a REP CMPS-heavy workload shows up as a shortfall
                    // rather than as a silently mislabelled bucket.
                    read.value
                };
                self.alu_sub(a, b, 0, width);
                self.adjust_index_register(6, address_size, width.bytes());
                self.adjust_index_register(7, address_size, width.bytes());
                self.decrement_string_count(address_size);
                let zf = self.flag(FLAG_ZF);
                let stop = match rep {
                    RepKind::Repe => !zf,
                    RepKind::Repne => zf,
                };
                Ok(Some(FastStringResult {
                    iterations: 1,
                    stop,
                }))
            }
            StringOp::Scas => {
                let dst_off = self.index_offset(7, address_size);
                if self.string_forward_chunk_iterations(
                    SegmentIndex::Es,
                    dst_off,
                    address_size,
                    width,
                    1,
                ) == 0
                {
                    return Ok(None);
                }
                let (_, dst) =
                    self.translate_segmented(bus, SegmentIndex::Es, dst_off, width.bytes(), false)?;
                let Some(b) = self.read_direct_string_value(bus, dst, width)? else {
                    return Ok(None);
                };
                self.alu_sub(self.acc_read(width), b, 0, width);
                self.adjust_index_register(7, address_size, width.bytes());
                self.decrement_string_count(address_size);
                let zf = self.flag(FLAG_ZF);
                let stop = match rep {
                    RepKind::Repe => !zf,
                    RepKind::Repne => zf,
                };
                Ok(Some(FastStringResult {
                    iterations: 1,
                    stop,
                }))
            }
            StringOp::Ins | StringOp::Outs => Ok(None),
        }
    }

    pub(super) fn run_string<B: CpuBus>(
        &mut self,
        bus: &mut B,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
    ) -> ExecResult<()> {
        match prefixes.rep {
            None => self.string_step(bus, op, width, prefixes, address_size)?,
            Some(kind) => {
                let mut chunk_iterations = 0u32;
                let mut allowance = self.rep_chunk_limit(bus, op, width);
                loop {
                    if self.string_count(address_size) == 0 {
                        break;
                    }
                    let remaining = match allowance {
                        None => u32::MAX,
                        Some(available) => {
                            let natural = available.min(
                                Self::MAX_BUDGETED_REP_ITERATIONS.saturating_sub(chunk_iterations),
                            );
                            if natural == 0 && self.rep_resume_active && chunk_iterations == 0 {
                                1
                            } else {
                                natural
                            }
                        }
                    };
                    if remaining == 0 {
                        self.rep_execution.yielded = true;
                        break;
                    }
                    if let Some(fast) = self.try_run_string_fast(
                        bus,
                        op,
                        width,
                        prefixes,
                        address_size,
                        kind,
                        remaining,
                    )? {
                        self.perf.rep_string_iterations += u64::from(fast.iterations);
                        self.perf.rep_string_fast_iterations += u64::from(fast.iterations);
                        chunk_iterations = chunk_iterations.saturating_add(fast.iterations);
                        if let Some(available) = allowance.as_mut() {
                            *available = available.saturating_sub(fast.iterations);
                        }
                        if fast.stop {
                            break;
                        }
                        if let Some(refreshed) = self.rep_chunk_limit(bus, op, width) {
                            allowance = Some(
                                allowance.map_or(refreshed, |available| available.min(refreshed)),
                            );
                        }
                        if self.string_count(address_size) != 0
                            && self.rep_execution.budget.is_some()
                            && (allowance == Some(0)
                                || chunk_iterations >= Self::MAX_BUDGETED_REP_ITERATIONS
                                || bus.requires_step_break()
                                || self.rep_budget_exhausted(bus, op))
                        {
                            self.rep_execution.yielded = true;
                            break;
                        }
                        continue;
                    }
                    self.string_step(bus, op, width, prefixes, address_size)?;
                    self.perf.rep_string_iterations += 1;
                    chunk_iterations += 1;
                    if let Some(available) = allowance.as_mut() {
                        *available = available.saturating_sub(1);
                    }
                    self.decrement_string_count(address_size);
                    // CMPS/SCAS also end the repeat on the ZF condition. REPE continues while
                    // ZF is set; REPNE continues while ZF is clear. MOVS/STOS/LODS ignore ZF.
                    if matches!(op, StringOp::Cmps | StringOp::Scas) {
                        let zf = self.flag(FLAG_ZF);
                        let again = match kind {
                            RepKind::Repe => zf,
                            RepKind::Repne => !zf,
                        };
                        if !again {
                            break;
                        }
                    }
                    if let Some(refreshed) = self.rep_chunk_limit(bus, op, width) {
                        allowance =
                            Some(allowance.map_or(refreshed, |available| available.min(refreshed)));
                    }
                    if self.string_count(address_size) != 0
                        && self.rep_execution.budget.is_some()
                        && (allowance == Some(0)
                            || chunk_iterations >= Self::MAX_BUDGETED_REP_ITERATIONS
                            || bus.requires_step_break()
                            || self.rep_budget_exhausted(bus, op))
                    {
                        self.rep_execution.yielded = true;
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    fn adjust_index_register(&mut self, index: u8, address_size: AddressSize, amount: u32) {
        let delta = if self.flag(FLAG_DF) {
            0u32.wrapping_sub(amount)
        } else {
            amount
        };

        match address_size {
            AddressSize::Word => {
                let value = self.read_gpr16(index).wrapping_add(delta as u16);
                self.write_gpr16(index, value);
            }
            AddressSize::Dword => {
                let value = self.read_gpr32(index).wrapping_add(delta);
                self.write_gpr32(index, value);
            }
        }
    }
}
