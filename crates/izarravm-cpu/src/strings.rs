// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl CpuGsw {
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

    fn read_string_src<B: CpuBus>(
        &mut self,
        bus: &mut B,
        prefixes: Prefixes,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let segment = prefixes.segment_override.unwrap_or(SegmentIndex::Ds);
        let offset = self.index_offset(6, address_size); // SI / ESI
        let physical = self.translate_segmented(bus, segment, offset, width.bytes(), false)?;
        if let Some(value) =
            self.read_direct_page_cached(bus, physical, width, BusAccessKind::DataRead)?
        {
            return Ok(value);
        }
        let read = bus.read_memory_direct(physical, width, BusAccessKind::DataRead)?;
        self.record_data_read(BusAccessKind::DataRead, read.direct);
        Ok(read.value)
    }

    fn read_string_dst<B: CpuBus>(
        &mut self,
        bus: &mut B,
        address_size: AddressSize,
        width: BusWidth,
    ) -> ExecResult<u32> {
        let offset = self.index_offset(7, address_size); // DI / EDI
        let physical =
            self.translate_segmented(bus, SegmentIndex::Es, offset, width.bytes(), false)?;
        if let Some(value) =
            self.read_direct_page_cached(bus, physical, width, BusAccessKind::DataRead)?
        {
            return Ok(value);
        }
        let read = bus.read_memory_direct(physical, width, BusAccessKind::DataRead)?;
        self.record_data_read(BusAccessKind::DataRead, read.direct);
        Ok(read.value)
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
        let physical =
            self.translate_segmented(bus, SegmentIndex::Es, offset, width.bytes(), true)?;
        if self.write_direct_page_cached(bus, physical, width, value, BusAccessKind::DataWrite)? {
            return Ok(());
        }
        let write = bus.write_memory_direct(physical, width, value, BusAccessKind::DataWrite)?;
        self.record_data_write(BusAccessKind::DataWrite, write.direct);
        Ok(())
    }

    fn segment_linear_unchecked(&self, segment: SegmentIndex, offset: u32) -> u32 {
        self.registers.segment(segment).base.wrapping_add(offset)
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
        if bus.direct_memory_bytes(physical, width.bytes() as usize, width)
            != width.bytes() as usize
        {
            return Ok(None);
        }
        let read = bus.read_memory_direct(physical, width, BusAccessKind::DataRead)?;
        self.record_data_read(BusAccessKind::DataRead, read.direct);
        if read.direct {
            Ok(Some(read.value))
        } else {
            Ok(None)
        }
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

    fn try_run_string_fast<B: CpuBus>(
        &mut self,
        bus: &mut B,
        op: StringOp,
        width: BusWidth,
        prefixes: Prefixes,
        address_size: AddressSize,
        rep: RepKind,
    ) -> ExecResult<Option<FastStringResult>> {
        if self.flag(FLAG_DF) || self.is_paging_enabled() {
            return Ok(None);
        }

        let count = self.string_count(address_size);
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
                let src = self.segment_linear_unchecked(src_segment, src_off);
                let dst = self.segment_linear_unchecked(SegmentIndex::Es, dst_off);
                if Self::ranges_overlap(src, dst, bytes)
                    || bus.direct_memory_bytes(src, bytes, width) != bytes
                    || bus.direct_memory_bytes(dst, bytes, width) != bytes
                {
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
                let put = bus.write_memory_bytes_direct(
                    dst,
                    &buf[..bytes],
                    width,
                    BusAccessKind::DataWrite,
                )?;
                if put != bytes {
                    return Ok(None);
                }
                self.note_code_write(dst, bytes as u32);
                self.record_write_page(dst);
                self.adjust_index_register(6, address_size, bytes as u32);
                self.adjust_index_register(7, address_size, bytes as u32);
                self.decrement_string_count_by(address_size, iterations);
                self.perf.data_direct_reads += u64::from(iterations);
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
                let dst = self.segment_linear_unchecked(SegmentIndex::Es, dst_off);
                if bus.direct_memory_bytes(dst, bytes, width) != bytes {
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
                let put = bus.write_memory_bytes_direct(
                    dst,
                    &buf[..bytes],
                    width,
                    BusAccessKind::DataWrite,
                )?;
                if put != bytes {
                    return Ok(None);
                }
                self.note_code_write(dst, bytes as u32);
                self.record_write_page(dst);
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
                let src = self.segment_linear_unchecked(src_segment, src_off);
                if bus.direct_memory_bytes(src, bytes, width) != bytes {
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
                let src = self.segment_linear_unchecked(src_segment, src_off);
                let dst = self.segment_linear_unchecked(SegmentIndex::Es, dst_off);
                if bus.direct_memory_bytes(src, access, width) != access
                    || bus.direct_memory_bytes(dst, access, width) != access
                {
                    return Ok(None);
                }
                let Some(a) = self.read_direct_string_value(bus, src, width)? else {
                    return Ok(None);
                };
                let Some(b) = self.read_direct_string_value(bus, dst, width)? else {
                    return Ok(None);
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
                let dst = self.segment_linear_unchecked(SegmentIndex::Es, dst_off);
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
            Some(kind) => loop {
                if self.string_count(address_size) == 0 {
                    break;
                }
                if let Some(fast) =
                    self.try_run_string_fast(bus, op, width, prefixes, address_size, kind)?
                {
                    self.perf.rep_string_iterations += u64::from(fast.iterations);
                    self.perf.rep_string_fast_iterations += u64::from(fast.iterations);
                    if fast.stop {
                        break;
                    }
                    continue;
                }
                self.string_step(bus, op, width, prefixes, address_size)?;
                self.perf.rep_string_iterations += 1;
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
            },
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
