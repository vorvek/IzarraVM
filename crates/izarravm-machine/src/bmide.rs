// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Intel PIIX4-compatible PCI bus-master IDE registers and PRD engine.

use izarravm_bus::{BusWidth, Memory};
use izarravm_core::MASTER_CLOCK_HZ;
use izarravm_core::{CanonicalFieldWriter, CanonicalStateError};

use crate::ata::{AtaDisk, AtaDmaDirection, AtaDmaRequest};

const COMMAND_START: u8 = 0x01;
const COMMAND_READ_FROM_DISK: u8 = 0x08;
const STATUS_ACTIVE: u8 = 0x01;
const STATUS_ERROR: u8 = 0x02;
const STATUS_INTERRUPT: u8 = 0x04;
const STATUS_DRIVE0_DMA: u8 = 0x20;
const STATUS_DRIVE1_DMA: u8 = 0x40;
const PRD_EOT: u32 = 0x8000_0000;
const PRD_RESERVED: u32 = 0x7fff_0000;
const COMMAND_LATENCY_TICKS: u64 = MASTER_CLOCK_HZ / 10_000; // 100 us

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PrdSpan {
    address: usize,
    len: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmaWriteSpan {
    pub(crate) address: u32,
    pub(crate) len: u32,
}

#[derive(Debug, PartialEq, Eq)]
struct PrdPlan {
    spans: Vec<PrdSpan>,
    retires_eot: bool,
}

#[derive(Debug, PartialEq, Eq)]
struct Transfer {
    direction: AtaDmaDirection,
    spans: Vec<PrdSpan>,
    ticks_remaining: u64,
    byte_len: usize,
    retires_eot: bool,
}

#[derive(Debug, Default)]
struct Channel {
    command: u8,
    status: u8,
    prd_address: u32,
    transfer: Option<Transfer>,
    completion_waits_for_stop: bool,
}

/// Two-channel PIIX4 bus-master IDE register block. Only the primary master has
/// a DMA-capable device; the secondary channel is still a real register bank so
/// PCI IDE drivers can enumerate it without being told ATAPI DMA exists.
#[derive(Debug, Default)]
pub(crate) struct BusMasterIde {
    primary: Channel,
    secondary: Channel,
}

impl BusMasterIde {
    pub(crate) fn owns_io(port: u16, width: BusWidth, base: u16) -> bool {
        let Some(end) = port.checked_add(width.bytes() as u16 - 1) else {
            return false;
        };
        let Some(block_end) = base.checked_add(15) else {
            return false;
        };
        port >= base && end <= block_end
    }

    pub(crate) fn read_io(&self, port: u16, width: BusWidth, base: u16) -> u32 {
        let offset = port - base;
        (0..width.bytes())
            .map(|index| u32::from(self.read_byte(offset + index as u16)) << (index * 8))
            .fold(0, |left, right| left | right)
    }

    pub(crate) fn write_io(
        &mut self,
        port: u16,
        width: BusWidth,
        value: u32,
        disk: Option<&mut AtaDisk>,
        base: u16,
    ) {
        let old_command = self.primary.command;
        let was_active = self.primary.status & STATUS_ACTIVE != 0;
        let offset = port - base;
        for index in 0..width.bytes() {
            self.write_byte(offset + index as u16, ((value >> (index * 8)) & 0xff) as u8);
        }

        let Some(disk) = disk else {
            return;
        };
        let stopped = old_command & COMMAND_START != 0
            && self.primary.command & COMMAND_START == 0
            && (was_active || disk.pending_dma().is_some());
        let changed_direction =
            was_active && (old_command ^ self.primary.command) & COMMAND_READ_FROM_DISK != 0;
        if stopped && self.primary.completion_waits_for_stop {
            self.primary.status &= !STATUS_ACTIVE;
            self.primary.completion_waits_for_stop = false;
        } else if stopped || changed_direction {
            self.fail_primary(disk);
        }
    }

    /// Reconcile controller and device state after an ATA command or PCI command
    /// register write. This also supplies the second half of either arming order.
    pub(crate) fn synchronize(
        &mut self,
        bus_master_enabled: bool,
        memory: &Memory,
        disk: &mut AtaDisk,
    ) {
        if self.primary.transfer.is_some() {
            if !bus_master_enabled || disk.pending_dma().is_none() {
                self.fail_primary(disk);
            }
            return;
        }
        if self.primary.completion_waits_for_stop {
            if !bus_master_enabled {
                self.primary.status &= !STATUS_ACTIVE;
                self.primary.completion_waits_for_stop = false;
            }
            return;
        }
        if !bus_master_enabled || self.primary.command & COMMAND_START == 0 {
            return;
        }
        let Some(request) = disk.pending_dma() else {
            return;
        };
        let read_from_disk = self.primary.command & COMMAND_READ_FROM_DISK != 0;
        if read_from_disk != (request.direction == AtaDmaDirection::DeviceToMemory) {
            self.fail_primary(disk);
            return;
        }
        let plan = match parse_prds(
            memory,
            self.primary.prd_address,
            request.byte_len(),
            request.direction,
        ) {
            Ok(plan) => plan,
            Err(()) => {
                self.fail_primary(disk);
                return;
            }
        };
        self.primary.status |= STATUS_ACTIVE;
        self.primary.completion_waits_for_stop = false;
        self.primary.transfer = Some(Transfer {
            direction: request.direction,
            spans: plan.spans,
            ticks_remaining: transfer_ticks(request),
            byte_len: request.byte_len(),
            retires_eot: plan.retires_eot,
        });
    }

    /// Advance the active transfer on the fixed machine timeline. Returns true
    /// when disk data was written into guest memory.
    #[cfg(test)]
    pub(crate) fn advance_master_ticks(
        &mut self,
        master_ticks: u64,
        memory: &mut Memory,
        disk: &mut AtaDisk,
    ) -> bool {
        self.advance_master_ticks_with_writes(master_ticks, memory, disk)
            .is_some()
    }

    /// Advance the active transfer and return the exact guest-memory spans written by a completed
    /// device-to-memory command. Memory-to-device transfers and incomplete commands return `None`.
    pub(crate) fn advance_master_ticks_with_writes(
        &mut self,
        master_ticks: u64,
        memory: &mut Memory,
        disk: &mut AtaDisk,
    ) -> Option<Vec<DmaWriteSpan>> {
        let transfer = self.primary.transfer.as_mut()?;
        if master_ticks < transfer.ticks_remaining {
            transfer.ticks_remaining -= master_ticks;
            return None;
        }
        self.complete_primary(memory, disk)
    }

    pub(crate) fn ticks_until_completion(&self) -> Option<u64> {
        self.primary
            .transfer
            .as_ref()
            .map(|transfer| transfer.ticks_remaining)
    }

    /// Latch the channel interrupt bit when its legacy IDE interrupt pin is
    /// observed. This applies to PIO, packet, and bus-master completions alike.
    pub(crate) fn note_ide_irq(&mut self, secondary: bool) {
        let channel = if secondary {
            &mut self.secondary
        } else {
            &mut self.primary
        };
        channel.status |= STATUS_INTERRUPT;
    }

    pub(crate) fn reset_primary(&mut self) {
        self.primary = Channel::default();
    }

    fn complete_primary(
        &mut self,
        memory: &mut Memory,
        disk: &mut AtaDisk,
    ) -> Option<Vec<DmaWriteSpan>> {
        let transfer = self.primary.transfer.take()?;
        let result = match transfer.direction {
            AtaDmaDirection::DeviceToMemory => disk.read_dma_payload().and_then(|payload| {
                (payload.len() == transfer.byte_len).then(|| {
                    scatter_write(memory, &transfer.spans, &payload);
                    disk.complete_dma_read(payload.len());
                })
            }),
            AtaDmaDirection::MemoryToDevice => {
                let payload = gather_read(memory, &transfer.spans, transfer.byte_len);
                disk.complete_dma_write(&payload).then_some(())
            }
        };
        if result.is_none() {
            self.fail_primary(disk);
            return None;
        }
        if transfer.retires_eot {
            self.primary.status &= !STATUS_ACTIVE;
            self.primary.completion_waits_for_stop = false;
        } else {
            self.primary.status |= STATUS_ACTIVE;
            self.primary.completion_waits_for_stop = true;
        }
        (transfer.direction == AtaDmaDirection::DeviceToMemory).then(|| {
            transfer
                .spans
                .into_iter()
                .map(|span| DmaWriteSpan {
                    address: span.address as u32,
                    len: span.len as u32,
                })
                .collect()
        })
    }

    fn fail_primary(&mut self, disk: &mut AtaDisk) {
        self.primary.transfer = None;
        self.primary.status &= !STATUS_ACTIVE;
        self.primary.status |= STATUS_ERROR;
        self.primary.completion_waits_for_stop = false;
        disk.abort_dma();
    }

    fn read_byte(&self, offset: u16) -> u8 {
        let (channel, register) = if offset < 8 {
            (&self.primary, offset)
        } else {
            (&self.secondary, offset - 8)
        };
        match register {
            0 => channel.command,
            2 => channel.status,
            4..=7 => (channel.prd_address >> ((register - 4) * 8)) as u8,
            _ => 0,
        }
    }

    fn write_byte(&mut self, offset: u16, value: u8) {
        let (channel, register) = if offset < 8 {
            (&mut self.primary, offset)
        } else {
            (&mut self.secondary, offset - 8)
        };
        match register {
            0 => channel.command = value & (COMMAND_START | COMMAND_READ_FROM_DISK),
            2 => {
                channel.status = (channel.status & !(STATUS_DRIVE0_DMA | STATUS_DRIVE1_DMA))
                    | (value & (STATUS_DRIVE0_DMA | STATUS_DRIVE1_DMA));
                channel.status &= !(value & (STATUS_ERROR | STATUS_INTERRUPT));
            }
            4..=7 if channel.status & STATUS_ACTIVE == 0 => {
                let shift = (register - 4) * 8;
                channel.prd_address =
                    (channel.prd_address & !(0xff << shift)) | (u32::from(value) << shift);
                channel.prd_address &= !3;
            }
            _ => {}
        }
    }
}

fn transfer_ticks(request: AtaDmaRequest) -> u64 {
    let data_ticks = (request.byte_len() as u128 * MASTER_CLOCK_HZ as u128)
        .div_ceil(request.bytes_per_second as u128);
    COMMAND_LATENCY_TICKS.saturating_add(data_ticks.min(u64::MAX as u128) as u64)
}

fn parse_prds(
    memory: &Memory,
    table: u32,
    byte_len: usize,
    direction: AtaDmaDirection,
) -> Result<PrdPlan, ()> {
    if byte_len == 0 || table & 3 != 0 {
        return Err(());
    }
    let table = table as usize;
    let table_page_end = (table & !0xfff).checked_add(0x1000).ok_or(())?;
    let mut spans = Vec::new();
    let mut covered = 0usize;
    let max_entries = ((table_page_end - table) / 8).min(byte_len.div_ceil(2));
    for index in 0..max_entries {
        let entry = table
            .checked_add(index.checked_mul(8).ok_or(())?)
            .ok_or(())?;
        let address = memory.read_u32(entry).map_err(|_| ())?;
        let descriptor = memory
            .read_u32(entry.checked_add(4).ok_or(())?)
            .map_err(|_| ())?;
        if entry.checked_add(8).ok_or(())? > table_page_end {
            return Err(());
        }
        if descriptor & PRD_RESERVED != 0 || address & 1 != 0 {
            return Err(());
        }
        // PIIX4 masks A1 and asserts every byte enable when it reads system
        // memory. Writes honor A1, leaving the lower word of the first dword
        // untouched when the programmed address is 2 mod 4.
        let address = match direction {
            AtaDmaDirection::MemoryToDevice => address & !3,
            AtaDmaDirection::DeviceToMemory => address,
        };
        let encoded_count = descriptor as u16;
        let count = if encoded_count == 0 {
            65_536usize
        } else {
            usize::from(encoded_count)
        };
        if count & 1 != 0 || (address as usize & 0xffff) + count > 0x1_0000 {
            return Err(());
        }
        let address = address as usize;
        let end = address.checked_add(count).ok_or(())?;
        if end > memory.len() {
            return Err(());
        }
        let used = count.min(byte_len - covered);
        spans.push(PrdSpan { address, len: used });
        covered += used;
        if covered == byte_len {
            return Ok(PrdPlan {
                spans,
                retires_eot: used == count && descriptor & PRD_EOT != 0,
            });
        }
        if descriptor & PRD_EOT != 0 {
            return Err(());
        }
    }
    Err(())
}

fn scatter_write(memory: &mut Memory, spans: &[PrdSpan], payload: &[u8]) {
    let mut position = 0;
    for span in spans {
        memory.as_mut_slice()[span.address..span.address + span.len]
            .copy_from_slice(&payload[position..position + span.len]);
        position += span.len;
    }
}

fn gather_read(memory: &Memory, spans: &[PrdSpan], byte_len: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(byte_len);
    for span in spans {
        payload.extend_from_slice(&memory.as_slice()[span.address..span.address + span.len]);
    }
    payload
}

/// Borrowed two-channel bus-master IDE state for canonical comparison.
///
/// Each channel serializes its raw registers, the mid-protocol
/// completion-waits-for-stop latch, and the transfer continuation with its
/// parsed PRD spans (already A1-masked for memory-to-device). Both channels
/// use the same record shape even though only the primary ever carries a
/// transfer in this model, so offsets stay uniform and a future ATAPI DMA
/// extension changes no layout. Capture rejects a primary transfer whose
/// armed ATA DMA request has vanished, since every reconcile seam
/// (task-file, BMIDE, and PCI command writes) otherwise keeps the pair in
/// step at run-loop boundaries; the INT 13h HLE never touches either side,
/// and its synchronous stall runs the ordinary device advance, which is the
/// BMIDE completion path itself.
pub(crate) struct CanonicalBusMasterIde<'a> {
    bmide: &'a BusMasterIde,
}

impl BusMasterIde {
    pub(crate) fn canonical_projection(&self) -> CanonicalBusMasterIde<'_> {
        CanonicalBusMasterIde { bmide: self }
    }
}

impl CanonicalBusMasterIde<'_> {
    /// Writes version 1 of the BMIDE payload: 30 fixed bytes per channel
    /// (primary then secondary) plus 8 bytes per parsed PRD span. The span
    /// count is written even when no transfer is in flight so fixed offsets
    /// never move.
    pub(crate) fn write_payload(
        &self,
        out: &mut CanonicalFieldWriter<'_>,
    ) -> Result<(), CanonicalStateError> {
        for channel in [&self.bmide.primary, &self.bmide.secondary] {
            out.write_u8(channel.command)?;
            out.write_u8(channel.status)?;
            out.write_u32(channel.prd_address)?;
            out.write_bool(channel.completion_waits_for_stop)?;
            let (present, direction, ticks, byte_len, retires_eot) = match &channel.transfer {
                None => (false, 0u8, 0, 0, false),
                Some(transfer) => (
                    true,
                    match transfer.direction {
                        AtaDmaDirection::DeviceToMemory => 0,
                        AtaDmaDirection::MemoryToDevice => 1,
                    },
                    transfer.ticks_remaining,
                    transfer.byte_len as u32,
                    transfer.retires_eot,
                ),
            };
            out.write_bool(present)?;
            out.write_u8(direction)?;
            out.write_u64(ticks)?;
            out.write_u32(byte_len)?;
            out.write_bool(retires_eot)?;
            let spans: &[PrdSpan] = channel
                .transfer
                .as_ref()
                .map_or(&[], |transfer| transfer.spans.as_slice());
            out.write_count(spans.len() as u64)?;
            for span in spans {
                out.write_u32(span.address as u32)?;
                out.write_u32(span.len as u32)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "bmide_test.rs"]
mod tests;
