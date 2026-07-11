// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
#[cfg(all(
    feature = "jit",
    target_arch = "x86_64",
    any(target_os = "windows", target_os = "linux")
))]
use izarravm_bus::DirectPage;

impl CpuGsw {
    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    fn fast_map_permissions(&self, linear: u32) -> Option<jit::fast_map::PagePermissions> {
        if !self.is_paging_enabled() {
            return Some(jit::fast_map::PagePermissions::UNPAGED);
        }
        self.tlb
            .lookup(linear >> 12)
            .map(|entry| jit::fast_map::PagePermissions {
                writable: entry.writable,
                user: entry.user,
            })
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    fn fast_map_population_enabled(&self) -> bool {
        self.mode().uses_approximate_timing() && !self.jit_regions.auto_admit()
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn populate_fast_map_active(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        write: bool,
    ) -> bool {
        let mapped = if write {
            self.jit_fast_map.has_write_mapping(linear, physical)
        } else {
            self.jit_fast_map.has_read_mapping(linear, physical)
        };
        if mapped {
            return true;
        }
        let Some(permissions) = self.fast_map_permissions(linear) else {
            return false;
        };
        if write {
            self.jit_fast_map
                .populate_write(linear, physical, page, permissions)
        } else {
            self.jit_fast_map
                .populate_read(linear, physical, page, permissions)
        }
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn populate_fast_map(
        &mut self,
        linear: u32,
        physical: u32,
        page: DirectPage,
        write: bool,
    ) -> bool {
        self.fast_map_population_enabled()
            && self.populate_fast_map_active(linear, physical, page, write)
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline(always)]
    fn populate_fast_map_from_cached(
        &mut self,
        linear: u32,
        physical: u32,
        entry: DirectPageCacheEntry,
        write: bool,
    ) {
        if !self.fast_map_population_enabled() {
            return;
        }
        if !self.populate_fast_map_active(
            linear,
            physical,
            DirectPage {
                physical_page: entry.physical_page,
                ptr: entry.ptr,
                len: 0x1000,
                writable: write,
            },
            write,
        ) {
            return;
        }
        if write {
            self.data_write_pages.note_fast_map_linear(physical, linear);
        } else {
            self.data_read_pages.note_fast_map_linear(physical, linear);
        }
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn read_fast_map<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> ExecResult<Option<u32>> {
        if !self.fast_map_population_enabled() {
            return Ok(None);
        }
        let Some(access) = self.jit_fast_map.lookup_access(
            linear,
            width,
            false,
            self.current_privilege_level() == 3,
            self.control.cr0 & CR0_WP != 0,
        ) else {
            return Ok(None);
        };
        bus.charge_direct_memory(access.physical(), width, kind)?;
        self.record_data_read(kind, true);
        self.perf.direct_data_pointer_reads += 1;
        Ok(Some(access.read(width)))
    }

    #[cfg(all(
        feature = "jit",
        target_arch = "x86_64",
        any(target_os = "windows", target_os = "linux")
    ))]
    #[inline]
    fn write_fast_map<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<bool> {
        if !self.fast_map_population_enabled() {
            return Ok(false);
        }
        let Some(access) = self.jit_fast_map.lookup_access(
            linear,
            width,
            true,
            self.current_privilege_level() == 3,
            self.control.cr0 & CR0_WP != 0,
        ) else {
            return Ok(false);
        };
        let physical = access.physical();
        self.record_write_page(physical);
        if access.read(width) != value {
            self.note_code_write(physical, width.bytes());
        }
        bus.charge_direct_memory(physical, width, kind)?;
        access.write(width, value);
        self.record_data_write(kind, true);
        self.perf.direct_data_pointer_writes += 1;
        Ok(true)
    }

    #[inline]
    pub(super) fn record_data_read(&mut self, kind: BusAccessKind, direct: bool) {
        if kind == BusAccessKind::DataRead {
            if direct {
                self.perf.data_direct_reads += 1;
            } else {
                self.perf.data_slow_reads += 1;
            }
        }
    }

    #[inline]
    pub(super) fn record_data_write(&mut self, kind: BusAccessKind, direct: bool) {
        if kind == BusAccessKind::DataWrite {
            if direct {
                self.perf.data_direct_writes += 1;
            } else {
                self.perf.data_slow_writes += 1;
            }
        }
    }

    #[inline]
    fn read_direct_entry(entry: DirectPageCacheEntry, physical: u32, width: BusWidth) -> u32 {
        let offset = (physical & 0x0fff) as usize;
        let ptr = unsafe { entry.ptr.add(offset) };
        match width {
            BusWidth::Byte => unsafe { u32::from(*ptr) },
            BusWidth::Word => unsafe {
                u32::from(u16::from_le(std::ptr::read_unaligned(ptr.cast::<u16>())))
            },
            BusWidth::Dword => unsafe { u32::from_le(std::ptr::read_unaligned(ptr.cast::<u32>())) },
        }
    }

    #[inline]
    fn write_direct_entry(entry: DirectPageCacheEntry, physical: u32, width: BusWidth, value: u32) {
        let offset = (physical & 0x0fff) as usize;
        let ptr = unsafe { entry.ptr.add(offset) };
        match width {
            BusWidth::Byte => unsafe {
                *ptr = value as u8;
            },
            BusWidth::Word => unsafe {
                std::ptr::write_unaligned(ptr.cast::<u16>(), (value as u16).to_le());
            },
            BusWidth::Dword => unsafe {
                std::ptr::write_unaligned(ptr.cast::<u32>(), value.to_le());
            },
        }
    }

    #[inline]
    fn direct_access_page_local(physical: u32, width: BusWidth) -> bool {
        let offset = (physical & 0x0fff) as usize;
        if offset + width.bytes() as usize > 0x1000 {
            return false;
        }
        match width {
            BusWidth::Byte => true,
            BusWidth::Word => physical & 1 == 0,
            BusWidth::Dword => physical & 3 == 0,
        }
    }

    #[inline]
    pub(super) fn read_direct_page_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        _linear: u32,
        physical: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> ExecResult<Option<u32>> {
        if !Self::direct_access_page_local(physical, width) {
            return Ok(None);
        }
        if let Some(entry) = self.data_read_pages.get(physical) {
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            self.populate_fast_map_from_cached(_linear, physical, entry, false);
            bus.charge_direct_memory(physical, width, kind)?;
            self.record_data_read(kind, true);
            self.perf.direct_data_pointer_reads += 1;
            return Ok(Some(Self::read_direct_entry(entry, physical, width)));
        }
        let Some(page) = bus.direct_page(physical, kind)? else {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        };
        let offset = (physical & 0x0fff) as usize;
        if page.len < 0x1000 || offset + width.bytes() as usize > page.len {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        }
        self.perf.direct_page_hits += 1;
        self.data_read_pages.insert(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if self.populate_fast_map(_linear, physical, page, false) {
            self.data_read_pages.note_fast_map_linear(physical, _linear);
        }
        bus.charge_direct_memory(physical, width, kind)?;
        self.record_data_read(kind, true);
        self.perf.direct_data_pointer_reads += 1;
        Ok(Some(Self::read_direct_entry(
            DirectPageCacheEntry {
                physical_page: page.physical_page,
                fast_map_linear_page: _linear & !0x0fff,
                ptr: page.ptr,
            },
            physical,
            width,
        )))
    }

    #[inline]
    pub(super) fn read_direct_byte_page_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        _linear: u32,
        physical: u32,
        kind: BusAccessKind,
    ) -> ExecResult<Option<u8>> {
        if let Some(entry) = self.data_read_pages.get(physical) {
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            self.populate_fast_map_from_cached(_linear, physical, entry, false);
            bus.charge_direct_memory(physical, BusWidth::Byte, kind)?;
            self.record_data_read(kind, true);
            self.perf.direct_data_pointer_reads += 1;
            let offset = (physical & 0x0fff) as usize;
            return Ok(Some(unsafe { *entry.ptr.add(offset) }));
        }
        let Some(page) = bus.direct_page(physical, kind)? else {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        };
        let offset = (physical & 0x0fff) as usize;
        if page.len < 0x1000 || offset >= page.len {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        }
        self.perf.direct_page_hits += 1;
        self.data_read_pages.insert(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if self.populate_fast_map(_linear, physical, page, false) {
            self.data_read_pages.note_fast_map_linear(physical, _linear);
        }
        bus.charge_direct_memory(physical, BusWidth::Byte, kind)?;
        self.record_data_read(kind, true);
        self.perf.direct_data_pointer_reads += 1;
        Ok(Some(unsafe { *page.ptr.add(offset) }))
    }

    #[inline]
    pub(super) fn write_direct_page_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        _linear: u32,
        physical: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<bool> {
        if !Self::direct_access_page_local(physical, width) {
            return Ok(false);
        }
        if let Some(entry) = self.data_write_pages.get(physical) {
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            self.populate_fast_map_from_cached(_linear, physical, entry, true);
            bus.charge_direct_memory(physical, width, kind)?;
            Self::write_direct_entry(entry, physical, width, value);
            self.record_data_write(kind, true);
            self.perf.direct_data_pointer_writes += 1;
            return Ok(true);
        }
        let Some(page) = bus.direct_page(physical, kind)? else {
            self.perf.direct_page_misses += 1;
            return Ok(false);
        };
        let offset = (physical & 0x0fff) as usize;
        if !page.writable || page.len < 0x1000 || offset + width.bytes() as usize > page.len {
            self.perf.direct_page_misses += 1;
            return Ok(false);
        }
        self.perf.direct_page_hits += 1;
        self.data_write_pages.insert(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if self.populate_fast_map(_linear, physical, page, true) {
            self.data_write_pages
                .note_fast_map_linear(physical, _linear);
        }
        bus.charge_direct_memory(physical, width, kind)?;
        Self::write_direct_entry(
            DirectPageCacheEntry {
                physical_page: page.physical_page,
                fast_map_linear_page: _linear & !0x0fff,
                ptr: page.ptr,
            },
            physical,
            width,
            value,
        );
        self.record_data_write(kind, true);
        self.perf.direct_data_pointer_writes += 1;
        Ok(true)
    }

    /// `Some(changed)` means the direct write completed; `None` asks the caller to use the bus path.
    #[inline]
    pub(super) fn write_direct_byte_page_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        _linear: u32,
        physical: u32,
        value: u8,
        kind: BusAccessKind,
    ) -> ExecResult<Option<bool>> {
        if let Some(entry) = self.data_write_pages.get(physical) {
            #[cfg(all(
                feature = "jit",
                target_arch = "x86_64",
                any(target_os = "windows", target_os = "linux")
            ))]
            self.populate_fast_map_from_cached(_linear, physical, entry, true);
            bus.charge_direct_memory(physical, BusWidth::Byte, kind)?;
            let offset = (physical & 0x0fff) as usize;
            let changed = unsafe { *entry.ptr.add(offset) != value };
            unsafe {
                *entry.ptr.add(offset) = value;
            }
            self.record_data_write(kind, true);
            self.perf.direct_data_pointer_writes += 1;
            return Ok(Some(changed));
        }
        let Some(page) = bus.direct_page(physical, kind)? else {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        };
        let offset = (physical & 0x0fff) as usize;
        if !page.writable || page.len < 0x1000 || offset >= page.len {
            self.perf.direct_page_misses += 1;
            return Ok(None);
        }
        self.perf.direct_page_hits += 1;
        self.data_write_pages.insert(page);
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if self.populate_fast_map(_linear, physical, page, true) {
            self.data_write_pages
                .note_fast_map_linear(physical, _linear);
        }
        bus.charge_direct_memory(physical, BusWidth::Byte, kind)?;
        let changed = unsafe { *page.ptr.add(offset) != value };
        unsafe {
            *page.ptr.add(offset) = value;
        }
        self.record_data_write(kind, true);
        self.perf.direct_data_pointer_writes += 1;
        Ok(Some(changed))
    }
    // (`read_rm_u8` was removed with the legacy 0x84 TEST r/m8,reg8 handler — its only remaining
    // caller. The converted flags-misc executor reads the byte r/m via `read_operand_u8` on the
    // pre-decoded operand instead. `write_rm_u8` was removed earlier with the legacy 0x88 MOV
    // r/m8,r8 handler. The sized/read siblings remain in use by the fallback handlers.)

    pub(super) fn read_operand_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
    ) -> ExecResult<u8> {
        match operand {
            RmOperand::Register(index) => Ok(self.read_gpr8(index)),
            RmOperand::Memory(memory) => {
                self.read_memory_u8(bus, memory.segment, memory.offset, BusAccessKind::DataRead)
            }
        }
    }

    pub(super) fn write_operand_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        value: u8,
    ) -> ExecResult<()> {
        match operand {
            RmOperand::Register(index) => {
                self.write_gpr8(index, value);
                Ok(())
            }
            RmOperand::Memory(memory) => self.write_memory_u8(
                bus,
                memory.segment,
                memory.offset,
                value,
                BusAccessKind::DataWrite,
            ),
        }
    }

    pub(super) fn read_operand_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        size: OperandSize,
    ) -> ExecResult<u32> {
        match operand {
            RmOperand::Register(index) => Ok(self.read_gpr_sized(index, size)),
            RmOperand::Memory(memory) => self.read_memory_sized(
                bus,
                memory.segment,
                memory.offset,
                size,
                BusAccessKind::DataRead,
            ),
        }
    }

    pub(super) fn write_operand_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand: RmOperand,
        size: OperandSize,
        value: u32,
    ) -> ExecResult<()> {
        match operand {
            RmOperand::Register(index) => {
                self.write_gpr_sized(index, size, value);
                Ok(())
            }
            RmOperand::Memory(memory) => self.write_memory_sized(
                bus,
                memory.segment,
                memory.offset,
                size,
                value,
                BusAccessKind::DataWrite,
            ),
        }
    }

    pub(super) fn read_memory_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        kind: BusAccessKind,
    ) -> ExecResult<u8> {
        let linear = self.segment_linear_byte(segment, offset, false)?;
        self.read_linear_u8(bus, linear, kind)
    }

    fn read_linear_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        kind: BusAccessKind,
    ) -> ExecResult<u8> {
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some(value) = self.read_fast_map(bus, linear, BusWidth::Byte, kind)? {
            return Ok(value as u8);
        }
        let physical = if self.control.cr0 & CR0_PG == 0 {
            linear
        } else {
            self.translate_linear(bus, linear, false)?
        };
        if let Some(value) = self.read_direct_byte_page_cached(bus, linear, physical, kind)? {
            return Ok(value);
        }
        let read = bus.read_memory_direct(physical, BusWidth::Byte, kind)?;
        self.record_data_read(kind, read.direct);
        Ok(read.value as u8)
    }

    pub(super) fn write_memory_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        value: u8,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        let linear = self.segment_linear_byte(segment, offset, true)?;
        self.write_linear_u8(bus, linear, value, kind)
    }

    fn write_linear_u8<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        value: u8,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if self.write_fast_map(bus, linear, BusWidth::Byte, u32::from(value), kind)? {
            return Ok(());
        }
        let physical = if self.control.cr0 & CR0_PG == 0 {
            self.record_write_page(linear);
            linear
        } else {
            self.translate_linear(bus, linear, true)?
        };
        if let Some(changed) =
            self.write_direct_byte_page_cached(bus, linear, physical, value, kind)?
        {
            if changed {
                self.note_code_write(physical, 1);
            }
            return Ok(());
        }
        self.note_code_write(physical, 1);
        let write = bus.write_memory_direct(physical, BusWidth::Byte, u32::from(value), kind)?;
        self.record_data_write(kind, write.direct);
        Ok(())
    }

    /// Validate a data access's *kind* against the segment descriptor's type field: a
    /// write through a read-only data segment, or any access through an execute-only
    /// code segment loaded into a data-segment register, is #GP (386 PRM 5-12, "Data
    /// segments can be read-only or read/write... Code segments can be execute-only or
    /// execute/read"). Real mode and V86 mode always carry the fully-permissive
    /// `access = 0x93` (`SegmentRegister::real`), so this only ever rejects something in
    /// protected mode; the caller gates on that to skip the check entirely otherwise.
    /// Instruction fetch never routes through here (it uses `code_linear_for_offset`),
    /// so CS's own readability never needs checking on this path -- only the case of a
    /// *data* segment register (DS/ES/FS/GS/SS) that happens to hold a code descriptor.
    pub(super) fn check_segment_access_kind(
        &self,
        segment: SegmentIndex,
        access: u8,
        write: bool,
    ) -> ExecResult<()> {
        if !self.is_protected_mode() || self.is_v86_mode() {
            return Ok(());
        }
        let is_code = access & 0x08 != 0; // descriptor type bit 3
        let ok = if is_code {
            // A code descriptor addressed as data: legal only for a read, and only if
            // the code segment's readable bit (type bit 1) is set.
            !write && access & 0x02 != 0
        } else {
            // A data descriptor: legal for a read always; a write needs the writable
            // bit (type bit 1) set.
            !write || access & 0x02 != 0
        };
        if ok {
            Ok(())
        } else {
            Err(segment_limit_fault(segment))
        }
    }

    #[inline]
    pub(super) fn segment_linear_byte(
        &self,
        segment: SegmentIndex,
        offset: u32,
        write: bool,
    ) -> ExecResult<u32> {
        let descriptor = self.registers.segment(segment);
        self.check_segment_access_kind(segment, descriptor.access, write)?;
        if descriptor.base == 0 && descriptor.limit == u32::MAX {
            return Ok(offset);
        }
        let expand_down = self.is_protected_mode()
            && !self.is_v86_mode()
            && descriptor.access & 0x18 == 0x10
            && descriptor.access & 0x04 != 0;
        let in_limit = if expand_down {
            // 386 PRM 5-12: an expand-down segment's valid offsets are those ABOVE the
            // limit (up to 0xffff, or 0xffff_ffff for a 32-bit-default segment), the
            // reverse of the normal sense.
            let ceiling = if descriptor.default_size_32 {
                u32::MAX
            } else {
                0xffff
            };
            offset > descriptor.limit && offset <= ceiling
        } else {
            offset <= descriptor.limit
        };
        if !in_limit {
            return Err(segment_limit_fault(segment));
        }
        Ok(descriptor.base.wrapping_add(offset))
    }

    pub(super) fn read_memory_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        size: OperandSize,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        self.check_alignment(offset, size.bytes())?;
        self.read_memory_bus_width(bus, segment, offset, size.bus_width(), kind)
    }

    pub(super) fn write_memory_sized<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        size: OperandSize,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        self.check_alignment(offset, size.bytes())?;
        self.write_memory_bus_width(bus, segment, offset, size.bus_width(), value, kind)
    }

    pub(super) fn read_memory_bus_width<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        if width == BusWidth::Byte {
            return self
                .read_memory_u8(bus, segment, offset, kind)
                .map(u32::from);
        }
        let linear = self.segment_linear_range(segment, offset, width.bytes(), false)?;
        if self.is_paging_enabled() && Self::linear_range_crosses_page(linear, width.bytes()) {
            return self.read_paged_cross_page(bus, linear, width.bytes(), kind);
        }
        self.read_linear_fragment(bus, linear, width, kind)
    }

    pub(super) fn write_memory_bus_width<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        if width == BusWidth::Byte {
            return self.write_memory_u8(bus, segment, offset, value as u8, kind);
        }
        let linear = self.segment_linear_range(segment, offset, width.bytes(), true)?;
        if self.is_paging_enabled() && Self::linear_range_crosses_page(linear, width.bytes()) {
            return self.write_paged_cross_page(bus, linear, width.bytes(), value, kind);
        }
        self.write_linear_fragment(bus, linear, width, value, kind)
    }

    #[inline]
    fn linear_range_crosses_page(linear: u32, width: u32) -> bool {
        (linear & 0x0fff) + width > 0x1000
    }

    fn read_paged_cross_page<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: u32,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        let mut value = 0u32;
        let mut completed = 0u32;
        while completed < width {
            let at = linear.wrapping_add(completed);
            let fragment = Self::page_local_fragment_width(at, width - completed);
            value |= self.read_linear_fragment(bus, at, fragment, kind)? << (completed * 8);
            completed += fragment.bytes();
        }
        Ok(value)
    }

    fn write_paged_cross_page<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: u32,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        let mut completed = 0u32;
        while completed < width {
            let at = linear.wrapping_add(completed);
            let fragment = Self::page_local_fragment_width(at, width - completed);
            self.write_linear_fragment(bus, at, fragment, value >> (completed * 8), kind)?;
            completed += fragment.bytes();
        }
        Ok(())
    }

    #[inline]
    fn page_local_fragment_width(linear: u32, remaining: u32) -> BusWidth {
        let page_remaining = 0x1000 - (linear & 0x0fff);
        if remaining >= 4 && page_remaining >= 4 && linear & 3 == 0 {
            BusWidth::Dword
        } else if remaining >= 2 && page_remaining >= 2 && linear & 1 == 0 {
            BusWidth::Word
        } else {
            BusWidth::Byte
        }
    }

    fn read_linear_fragment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: BusWidth,
        kind: BusAccessKind,
    ) -> ExecResult<u32> {
        if width == BusWidth::Byte {
            return self.read_linear_u8(bus, linear, kind).map(u32::from);
        }
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if let Some(value) = self.read_fast_map(bus, linear, width, kind)? {
            return Ok(value);
        }
        let physical = self.translate_linear(bus, linear, false)?;
        if let Some(value) = self.read_direct_page_cached(bus, linear, physical, width, kind)? {
            return Ok(value);
        }
        let read = bus.read_memory_direct(physical, width, kind)?;
        self.record_data_read(kind, read.direct);
        Ok(read.value)
    }

    fn write_linear_fragment<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        width: BusWidth,
        value: u32,
        kind: BusAccessKind,
    ) -> ExecResult<()> {
        if width == BusWidth::Byte {
            return self.write_linear_u8(bus, linear, value as u8, kind);
        }
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if self.write_fast_map(bus, linear, width, value, kind)? {
            return Ok(());
        }
        let physical = self.translate_linear(bus, linear, true)?;
        self.note_code_write(physical, width.bytes());
        if self.write_direct_page_cached(bus, linear, physical, width, value, kind)? {
            return Ok(());
        }
        let write = bus.write_memory_direct(physical, width, value, kind)?;
        self.record_data_write(kind, write.direct);
        Ok(())
    }

    // #AC alignment check (486). A data access faults vector 17 (no error code) when
    // CR0.AM and EFLAGS.AC are both set and the access runs at CPL 3, and the effective
    // address is not naturally aligned for its width (word on a 2-byte boundary, dword on
    // a 4-byte boundary). Supervisor accesses (CPL < 3) and instruction fetches are exempt;
    // fetches never route through this helper. Byte accesses (width 1) are always aligned.
    fn check_alignment(&self, offset: u32, width: u32) -> ExecResult<()> {
        if width <= 1 || !self.alignment_armed {
            return Ok(());
        }
        if self.current_privilege_level() == 3 && offset % width != 0 {
            // Real 486 #AC pushes a zero error code; this core models it without one,
            // matching the rest of the spec's fault contract. Flagged as a divergence.
            return Err(InternalFault::Exception {
                vector: 17,
                error_code: None,
            });
        }
        Ok(())
    }

    pub(super) fn translate_segmented<B: CpuBus>(
        &mut self,
        bus: &mut B,
        segment: SegmentIndex,
        offset: u32,
        width: u32,
        write: bool,
    ) -> ExecResult<(u32, u32)> {
        let linear = self.segment_linear_range(segment, offset, width, write)?;
        let physical = self.translate_linear(bus, linear, write)?;
        if write {
            self.note_code_write(physical, width);
        }
        Ok((linear, physical))
    }

    fn segment_linear_range(
        &self,
        segment: SegmentIndex,
        offset: u32,
        width: u32,
        write: bool,
    ) -> ExecResult<u32> {
        let descriptor = self.registers.segment(segment);
        self.check_segment_access_kind(segment, descriptor.access, write)?;
        let linear = if descriptor.base == 0 && descriptor.limit == u32::MAX {
            offset
        } else {
            let last = offset.saturating_add(width.saturating_sub(1));
            let expand_down = self.is_protected_mode()
                && !self.is_v86_mode()
                && descriptor.access & 0x18 == 0x10
                && descriptor.access & 0x04 != 0;
            let in_limit = if expand_down {
                let ceiling = if descriptor.default_size_32 {
                    u32::MAX
                } else {
                    0xffff
                };
                offset > descriptor.limit && last <= ceiling
            } else {
                offset <= descriptor.limit && last <= descriptor.limit
            };
            if !in_limit {
                return Err(segment_limit_fault(segment));
            }
            descriptor.base.wrapping_add(offset)
        };
        Ok(linear)
    }

    pub(super) fn translate_linear<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
    ) -> ExecResult<u32> {
        self.translate_linear_checked(bus, linear, write, PagingAccessor::Current)
    }

    /// Like `translate_linear`, but for accesses to descriptor tables (GDT/LDT/IDT)
    /// and TSS fields during exception delivery, segment loads, and task switches.
    /// These are architecturally implicit supervisor accesses (386 PRM 6.2, 7.2):
    /// the processor consults them to set up or validate a privilege transition, so
    /// they must not be checked against the CPL of the code that triggered the
    /// transition. A V86 task (always CPL 3) or a ring-3 CS delivering through an
    /// interrupt gate must be able to read its own TSS/GDT even when those pages
    /// are marked supervisor-only (U/S=0), exactly as real silicon does. Forcing
    /// `user = false` here also means a WP-clear supervisor write (the 386 default)
    /// is never blocked by a read-only system-structure page.
    pub(super) fn translate_linear_system<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
    ) -> ExecResult<u32> {
        self.translate_linear_checked(bus, linear, write, PagingAccessor::Supervisor)
    }

    fn translate_linear_checked<B: CpuBus>(
        &mut self,
        bus: &mut B,
        linear: u32,
        write: bool,
        accessor: PagingAccessor,
    ) -> ExecResult<u32> {
        if !self.is_paging_enabled() {
            if write {
                self.record_write_page(linear);
            }
            return Ok(linear);
        }

        // Paging privilege: CPL 3 is a user access, CPL 0-2 are supervisor. A
        // system-structure access is forced supervisor regardless of the current
        // CPL (see `translate_linear_system`).
        let user = match accessor {
            // CPL is the cached quantity (`current_privilege_level`/`self.cpl`), not a live
            // read of CS.selector -- see that method for why a live formula misclassifies
            // the monitor's own ring-0 stack pushes as user during V86-source exception
            // delivery (source CS's RPL bits are irrelevant once cpl has already been set
            // to the entered level).
            PagingAccessor::Current => self.current_privilege_level() == 3,
            PagingAccessor::Supervisor => false,
        };
        // CR0.WP (a 486 addition) makes supervisor writes obey the page R/W bit too.
        // With WP clear, supervisor writes to read-only pages succeed (386 behavior).
        let wp = self.control.cr0 & CR0_WP != 0;

        // The large linear map is the shared translation cache for approximate-timing modes.
        // Read entries are installed only after the page walker has set A; write entries only
        // after it has also set D. It is invalidated with the TLB on every translation-affecting
        // event, while the live permission check below handles a CPL change without a flush.
        #[cfg(all(
            feature = "jit",
            target_arch = "x86_64",
            any(target_os = "windows", target_os = "linux")
        ))]
        if self.mode().uses_approximate_timing()
            && let Some(physical) = self.jit_fast_map.lookup_physical(linear, write, user, wp)
        {
            if write {
                self.record_write_page(physical);
            }
            return Ok(physical);
        }

        // TLB fast path: a cached entry skips the two page-table reads (and the
        // accessed-bit write the fill already did). The protection check is redone
        // from the cached page bits against the *current* accessor (CPL can change
        // without a flush); WP changes flush, so `wp` is consistent within a
        // generation. A write to a page whose dirty bit is not yet set falls through
        // to the walk so the PTE's D bit is updated.
        let page = linear >> 12;
        if let Some(e) = self.tlb.lookup(page) {
            let protection_fault = if user {
                !e.user || (write && !e.writable)
            } else {
                write && wp && !e.writable
            };
            if protection_fault {
                self.control.cr2 = linear;
                return Err(InternalFault::Exception {
                    vector: 14,
                    error_code: Some(page_fault_code(true, write, user)),
                });
            }
            // Serve the hit for a read, or a write to an already-dirty page.
            if !write || e.dirty {
                let physical = e.phys | (linear & 0x0000_0fff);
                if write {
                    self.record_write_page(physical);
                }
                return Ok(physical);
            }
        }

        let directory = self.control.cr3 & 0xffff_f000;
        let directory_address = directory + (((linear >> 22) & 0x03ff) * 4);
        let mut pde = bus.read_memory(
            directory_address,
            BusWidth::Dword,
            BusAccessKind::PageWalkRead,
        )?;
        if pde & 1 == 0 {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(false, write, user)),
            });
        }
        if pde & 0x20 == 0 {
            pde |= 0x20;
            bus.write_memory(
                directory_address,
                BusWidth::Dword,
                pde,
                BusAccessKind::PageWalkWrite,
            )?;
        }

        let table_address = (pde & 0xffff_f000) + (((linear >> 12) & 0x03ff) * 4);
        let mut pte =
            bus.read_memory(table_address, BusWidth::Dword, BusAccessKind::PageWalkRead)?;
        if pte & 1 == 0 {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(false, write, user)),
            });
        }

        // Protection check. The combined R/W and U/S come from ANDing the PDE and
        // PTE bits (bit 1 and bit 2). A page is user-accessible only if both U/S
        // bits are set, and writable only if both R/W bits are set.
        //   - A user access faults if it touches a supervisor page, or writes a
        //     read-only page.
        //   - A supervisor write faults only when CR0.WP is set and the page is
        //     read-only (combined R/W = 0). With WP clear, supervisor writes pass.
        // Either way the fault is present=1 and the error-code U/S bit reflects the
        // access (user), not the page. Checked before the dirty bit is set so a
        // faulting write leaves it clear.
        let writable = pde & pte & 0x2 != 0;
        let user_accessible = pde & pte & 0x4 != 0;
        let protection_fault = if user {
            !user_accessible || (write && !writable)
        } else {
            write && wp && !writable
        };
        if protection_fault {
            self.control.cr2 = linear;
            return Err(InternalFault::Exception {
                vector: 14,
                error_code: Some(page_fault_code(true, write, user)),
            });
        }

        let dirty = if write { 0x40 } else { 0 };
        let accessed_dirty = 0x20 | dirty;
        if pte & accessed_dirty != accessed_dirty {
            pte |= accessed_dirty;
            bus.write_memory(
                table_address,
                BusWidth::Dword,
                pte,
                BusAccessKind::PageWalkWrite,
            )?;
        }

        // Cache the completed translation. Only reached on the success path, so a
        // page that faulted (not present / protection) is never cached. `dirty`
        // records whether the PTE's D bit is now set, so a later read hits but a
        // first write to a still-clean page re-walks to set it.
        self.tlb.insert(
            page,
            pte & 0xffff_f000,
            writable,
            user_accessible,
            pte & 0x40 != 0,
        );

        let physical = (pte & 0xffff_f000) | (linear & 0x0000_0fff);
        if write {
            self.record_write_page(physical);
        }
        Ok(physical)
    }

    pub(super) fn push<B: CpuBus>(
        &mut self,
        bus: &mut B,
        value: u32,
        operand_size: OperandSize,
    ) -> ExecResult<()> {
        let width = operand_size.bytes();
        // The write PRECEDES the (E)SP commit: a push whose stack write faults
        // (#PF on a not-yet-committed stack page under a lazy-commit DPMI host,
        // or a #GP/#SS limit violation) must leave (E)SP at its pre-instruction
        // value so the post-handler restart re-executes cleanly. Committing
        // first left ESP decremented across the fault; CWSDPMI's commit-and-
        // retry stack growth then double-decremented, shifting every later
        // stack slot one down and handing DJGPP code shifted callee-saved
        // registers on the next epilogue (found via Quake's crt1
        // setup_environment crash).
        if self.stack_is_32bit() {
            // SS.B=1: implicit stack references use the full 32-bit ESP, for both
            // 16-bit and 32-bit operand-size pushes (386 PRM 16.2: the B bit picks
            // the stack-pointer width, independent of operand size).
            let esp = self.registers.esp().wrapping_sub(width);
            self.write_memory_sized(
                bus,
                SegmentIndex::Ss,
                esp,
                operand_size,
                value,
                BusAccessKind::DataWrite,
            )?;
            self.registers.set_esp(esp);
        } else {
            // SS.B=0 (real mode, V86, or a 16-bit protected-mode stack): the address
            // comes from SP only, only SP advances, and ESP's high word is preserved
            // (real silicon wraps SP, not ESP, on this stack).
            let sp = self.read_gpr16(4).wrapping_sub(width as u16);
            self.write_memory_sized(
                bus,
                SegmentIndex::Ss,
                u32::from(sp),
                operand_size,
                value,
                BusAccessKind::DataWrite,
            )?;
            self.write_gpr16(4, sp);
        }
        Ok(())
    }

    pub(super) fn pop<B: CpuBus>(
        &mut self,
        bus: &mut B,
        operand_size: OperandSize,
    ) -> ExecResult<u32> {
        let width = operand_size.bytes();
        if self.stack_is_32bit() {
            let esp = self.registers.esp();
            let value = self.read_memory_sized(
                bus,
                SegmentIndex::Ss,
                esp,
                operand_size,
                BusAccessKind::DataRead,
            )?;
            self.registers.set_esp(esp.wrapping_add(width));
            Ok(value)
        } else {
            // SS.B=0: read from SP and advance only SP, preserving ESP's high word.
            let sp = self.read_gpr16(4);
            let value = self.read_memory_sized(
                bus,
                SegmentIndex::Ss,
                u32::from(sp),
                operand_size,
                BusAccessKind::DataRead,
            )?;
            self.write_gpr16(4, sp.wrapping_add(width as u16));
            Ok(value)
        }
    }
}
