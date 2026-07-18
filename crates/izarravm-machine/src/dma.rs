// This file is part of IzarraVM and is licensed under GNU GPL version 3 only.
// SPDX-License-Identifier: GPL-3.0-only

//! Intel 8237A DMA controller, a master/slave cascade pair.
//!
//! Built clean-room from the Intel 8237A datasheet. Demand, single, block, and
//! auto-init channel state is modeled, along with the command register's
//! controller-disable gate and the memory-to-memory block transfer.
//! Both transfer directions run: memory->device (Sound Blaster playback) and
//! device->memory (the floppy controller's READ DATA on channel 2).
//! A device call is one request/grant/transfer cycle. Demand, single, and block
//! programming share that cycle path; cascade channels do not use its memory
//! datapath.

use izarravm_bus::Memory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DmaChannel {
    pub base_addr: u16,
    pub cur_addr: u16,
    pub base_count: u16,
    pub cur_count: u16,
    pub page: u8,             // high address byte A16-A23 (page register)
    pub addr_decrement: bool, // mode bit5
    pub auto_init: bool,      // mode bit4
    pub transfer_kind: u8,    // mode bits2-3: 0 verify, 1 write(i/o->mem), 2 read(mem->i/o)
    pub transfer_mode: u8,    // mode bits6-7: 0 demand, 1 single, 2 block, 3 cascade
    pub mask: bool,           // mask register bit
    pub reached_tc: bool,
    pub dreq: bool,           // hardware request input level
    pub active: bool,         // a granted transfer cycle is in progress
    pub transfer_cycles: u64, // completed byte or word cycles
}

impl Default for DmaChannel {
    fn default() -> Self {
        Self {
            base_addr: 0,
            cur_addr: 0,
            base_count: 0,
            cur_count: 0,
            page: 0,
            addr_decrement: false,
            auto_init: false,
            transfer_kind: 0,
            transfer_mode: 0,
            mask: true,
            reached_tc: false,
            dreq: false,
            active: false,
            transfer_cycles: 0,
        }
    }
}

impl DmaChannel {
    /// Mode register write (bits 2-3 transfer kind, bit4 auto-init, bit5 addr dec,
    /// bits 6-7 transfer mode). Device calls grant one cycle at a time. Block
    /// mode keeps its grant until terminal count, while cascade has no datapath.
    pub(crate) fn set_mode(&mut self, value: u8) {
        self.transfer_kind = (value >> 2) & 0x3;
        self.auto_init = value & 0x10 != 0;
        self.addr_decrement = value & 0x20 != 0;
        self.transfer_mode = (value >> 6) & 0x3;
    }

    /// Byte address the master (8-bit) drives: page in A23-A16, cur_addr in A15-A0.
    fn byte_address(&self) -> u32 {
        (u32::from(self.page) << 16) | u32::from(self.cur_addr)
    }

    /// Word address the slave (16-bit) drives: page in A23-A17, cur_addr (a word
    /// count) in A16-A1; A0 is tied low so transfers are always word-aligned.
    /// IBM PC/AT 16-bit DMA wiring: the slave's address counter counts words.
    fn word_address(&self) -> u32 {
        (u32::from(self.page) << 17) | (u32::from(self.cur_addr) << 1)
    }

    /// Shared per-transfer step: advance the address counter, decrement the count
    /// through zero to terminal count, then reload (auto-init) or mask (single).
    fn step_transfer(&mut self) {
        self.transfer_cycles = self.transfer_cycles.saturating_add(1);
        self.cur_addr = if self.addr_decrement {
            self.cur_addr.wrapping_sub(1)
        } else {
            self.cur_addr.wrapping_add(1)
        };
        // Count decrements through 0 to 0xFFFF; the 0->0xFFFF step is terminal.
        let next = self.cur_count.wrapping_sub(1);
        self.reached_tc = self.cur_count == 0;
        self.cur_count = next;
        if self.reached_tc {
            if self.auto_init {
                self.cur_addr = self.base_addr;
                self.cur_count = self.base_count;
            } else {
                self.mask = true;
            }
        }
    }

    /// Read one byte from memory (memory->device read transfer) and step the
    /// channel. Returns None when masked, not programmed for a read transfer, or
    /// already at terminal count.
    pub(crate) fn read_byte(&mut self, memory: &mut Memory) -> Option<u8> {
        if self.mask || self.transfer_kind != 2 {
            return None;
        }
        let byte = memory.read_u8(self.byte_address() as usize).ok()?;
        self.step_transfer();
        Some(byte)
    }

    /// Read one little-endian word from memory on the slave's word-addressed path
    /// (memory->device, 16-bit DMA). The counter steps in words, exactly as the
    /// byte path steps in bytes; only the address formation differs. Returns None
    /// when masked, not programmed for a read transfer, or at terminal count.
    pub(crate) fn read_word(&mut self, memory: &mut Memory) -> Option<u16> {
        if self.mask || self.transfer_kind != 2 {
            return None;
        }
        let addr = self.word_address() as usize;
        let lo = memory.read_u8(addr).ok()?;
        let hi = memory.read_u8(addr + 1).ok()?;
        self.step_transfer();
        Some(u16::from_le_bytes([lo, hi]))
    }

    /// Write one byte to memory (device->memory write transfer) and step the
    /// channel. Returns None when masked, not programmed for a write transfer, or
    /// already at terminal count. The floppy controller's READ DATA path lands
    /// sector bytes through here on channel 2.
    pub(crate) fn write_byte(&mut self, memory: &mut Memory, byte: u8) -> Option<u32> {
        if self.mask || self.transfer_kind != 1 {
            return None;
        }
        let address = self.byte_address();
        memory.write_u8(address as usize, byte).ok()?;
        self.step_transfer();
        Some(address)
    }

    /// Write one little-endian word to memory on the slave's word-addressed path
    /// (device->memory, 16-bit DMA) and step the channel. Returns None when
    /// masked, not programmed for a write transfer, or at terminal count.
    #[allow(dead_code)] // Limit: no Machine-level write wiring yet (see write_byte).
    pub(crate) fn write_word(&mut self, memory: &mut Memory, word: u16) -> Option<()> {
        if self.mask || self.transfer_kind != 1 {
            return None;
        }
        let addr = self.word_address() as usize;
        let [lo, hi] = word.to_le_bytes();
        memory.write_u8(addr, lo).ok()?;
        memory.write_u8(addr + 1, hi).ok()?;
        self.step_transfer();
        Some(())
    }

    /// Verify transfer (transfer_kind 0): step address and count with no memory
    /// access, exactly as the 8237A does for a verify cycle. Returns None when
    /// masked, not programmed for a verify transfer, or already at terminal count.
    #[allow(dead_code)] // Limit: no Machine-level verify wiring yet (see write_byte).
    pub(crate) fn verify(&mut self) -> Option<()> {
        if self.mask || self.transfer_kind != 0 {
            return None;
        }
        self.step_transfer();
        Some(())
    }
}

/// One physical 8237A: four channels plus the shared byte pointer flip-flop and
/// the command/status/request registers. Exposed methods operate on a "local"
/// register index 0..16 (the master's raw port, or the slave's translated port).
#[derive(Debug, Clone, Default)]
pub(crate) struct DmaChip {
    pub(crate) channels: [DmaChannel; 4],
    hi_lo: bool, // byte pointer: false = LSB next, true = MSB next
    command: u8,
    status: u8,      // bit N: channel N reached terminal count
    request_reg: u8, // software DREQ
}

impl DmaChip {
    fn addr_channel(local: u8) -> Option<usize> {
        // local 0,2,4,6 -> address channels 0..3
        if local < 8 && local.is_multiple_of(2) {
            Some((local / 2) as usize)
        } else {
            None
        }
    }

    fn count_channel(local: u8) -> Option<usize> {
        // local 1,3,5,7 -> count channels 0..3
        if local < 8 && local % 2 == 1 {
            Some((local / 2) as usize)
        } else {
            None
        }
    }

    fn write_local(&mut self, local: u8, value: u8) {
        if let Some(ci) = Self::addr_channel(local) {
            self.write_addr(ci, value);
        } else if let Some(ci) = Self::count_channel(local) {
            self.write_count(ci, value);
        } else {
            match local {
                8 => self.command = value,
                9 => {
                    let ci = (value & 0x03) as usize;
                    if value & 0x04 != 0 {
                        self.request_reg |= 1 << ci;
                    } else {
                        self.request_reg &= !(1 << ci);
                    }
                }
                10 => {
                    // Single mask register: bits 0-1 channel, bit2 set(1)/clear(0).
                    let ci = (value & 0x03) as usize;
                    self.channels[ci].mask = value & 0x04 != 0;
                    if self.channels[ci].mask {
                        self.channels[ci].active = false;
                    }
                }
                11 => {
                    // Mode register: bits 0-1 select the channel.
                    let ci = (value & 0x03) as usize;
                    self.channels[ci].set_mode(value);
                }
                12 => self.hi_lo = false, // reset flip-flop
                13 => self.master_clear(),
                14 => self.channels.iter_mut().for_each(|c| c.mask = false),
                15 => {
                    // Write-all-mask: bits 0-3 set each channel's mask.
                    for ci in 0..4 {
                        self.channels[ci].mask = value & (1 << ci) != 0;
                        if self.channels[ci].mask {
                            self.channels[ci].active = false;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn read_local(&mut self, local: u8) -> Option<u8> {
        if let Some(ci) = Self::addr_channel(local) {
            Some(self.read_addr(ci))
        } else if let Some(ci) = Self::count_channel(local) {
            Some(self.read_count(ci))
        } else {
            match local {
                8 => {
                    // Status read: bits 0-3 are terminal-count (read-clear), bits
                    // 4-7 are the combined hardware and software DREQ levels.
                    // Request levels are not cleared by a status read.
                    let mut requests = self.request_reg & 0x0F;
                    for (ci, channel) in self.channels.iter().enumerate() {
                        if channel.dreq {
                            requests |= 1 << ci;
                        }
                    }
                    let s = (self.status & 0x0F) | (requests << 4);
                    self.status = 0;
                    Some(s)
                }
                13 => Some(0), // temporary register (unused for memory->device)
                _ => None,
            }
        }
    }

    fn write_addr(&mut self, ci: usize, value: u8) {
        let new = if !self.hi_lo {
            (self.channels[ci].base_addr & 0xFF00) | u16::from(value)
        } else {
            (self.channels[ci].base_addr & 0x00FF) | (u16::from(value) << 8)
        };
        // Programming the address register loads both base and current.
        self.channels[ci].base_addr = new;
        self.channels[ci].cur_addr = new;
        self.hi_lo = !self.hi_lo;
    }

    fn write_count(&mut self, ci: usize, value: u8) {
        let new = if !self.hi_lo {
            (self.channels[ci].base_count & 0xFF00) | u16::from(value)
        } else {
            (self.channels[ci].base_count & 0x00FF) | (u16::from(value) << 8)
        };
        self.channels[ci].base_count = new;
        self.channels[ci].cur_count = new;
        // Loading a new count clears a latched TC.
        self.channels[ci].reached_tc = false;
        self.status &= !(1 << ci);
        self.hi_lo = !self.hi_lo;
    }

    fn read_addr(&mut self, ci: usize) -> u8 {
        let v = if !self.hi_lo {
            (self.channels[ci].cur_addr & 0xFF) as u8
        } else {
            (self.channels[ci].cur_addr >> 8) as u8
        };
        self.hi_lo = !self.hi_lo;
        v
    }

    fn read_count(&mut self, ci: usize) -> u8 {
        let v = if !self.hi_lo {
            (self.channels[ci].cur_count & 0xFF) as u8
        } else {
            (self.channels[ci].cur_count >> 8) as u8
        };
        self.hi_lo = !self.hi_lo;
        v
    }

    fn master_clear(&mut self) {
        self.command = 0;
        self.status = 0;
        self.request_reg = 0;
        self.hi_lo = false;
        self.channels.iter_mut().for_each(|channel| {
            channel.mask = true;
            channel.dreq = false;
            channel.active = false;
        });
    }

    /// Command-register bit0: memory-to-memory transfers enabled.
    fn mem_to_mem_enabled(&self) -> bool {
        self.command & 0x01 != 0
    }

    /// Command-register bit1: channel-0 address hold. When set during a
    /// memory-to-memory transfer the source address does not advance, so one
    /// source byte fills the whole destination block.
    fn channel0_hold(&self) -> bool {
        self.command & 0x02 != 0
    }

    /// Command-register bit2: controller disable. When set, the whole chip is
    /// inhibited and no transfer runs.
    fn controller_disabled(&self) -> bool {
        self.command & 0x04 != 0
    }

    fn set_hardware_request(&mut self, ci: usize, asserted: bool) {
        self.channels[ci].dreq = asserted;
        if !asserted && self.channels[ci].transfer_mode != 2 {
            self.channels[ci].active = false;
        }
    }

    fn request_active(&self, ci: usize) -> bool {
        self.channels[ci].dreq || self.request_reg & (1 << ci) != 0
    }

    fn begin_cycle(&mut self, ci: usize) -> bool {
        let block_active = self.channels[ci].transfer_mode == 2 && self.channels[ci].active;
        if self.controller_disabled()
            || self.channels[ci].mask
            || self.channels[ci].transfer_mode == 3
            || (!self.request_active(ci) && !block_active)
        {
            return false;
        }
        self.channels[ci].reached_tc = false;
        self.channels[ci].active = true;
        true
    }

    fn finish_cycle(&mut self, ci: usize, completed: bool) {
        if !completed {
            self.channels[ci].active = false;
            return;
        }
        if self.channels[ci].reached_tc {
            self.status |= 1 << ci;
            self.request_reg &= !(1 << ci);
            self.channels[ci].active = false;
        } else if self.channels[ci].transfer_mode != 2 {
            self.channels[ci].active = false;
        }
    }

    /// Whether a software DREQ on channel 0 is currently armed to launch a
    /// memory-to-memory transfer: mem-to-mem enabled (command bit0), the
    /// controller live, channel 0 unmasked, and its request-register bit set. The
    /// machine checks this after a write to the request register to fire the copy.
    fn mem_to_mem_request_armed(&self) -> bool {
        self.mem_to_mem_enabled()
            && !self.controller_disabled()
            && !self.channels[0].mask
            && self.request_reg & 0x01 != 0
    }

    /// Read one byte from the device (memory->device) on local channel `ci`,
    /// latching terminal-count into the status register. Returns None when the
    /// controller is disabled by command bit2.
    fn read_byte(&mut self, ci: usize, memory: &mut Memory) -> Option<u8> {
        if !self.begin_cycle(ci) {
            return None;
        }
        let byte = self.channels[ci].read_byte(memory);
        self.finish_cycle(ci, byte.is_some());
        let byte = byte?;
        Some(byte)
    }

    /// Read one 16-bit word from the device (memory->device) on local channel
    /// `ci`, latching terminal-count into the status register. Returns None when
    /// the controller is disabled by command bit2.
    fn read_word(&mut self, ci: usize, memory: &mut Memory) -> Option<u16> {
        if !self.begin_cycle(ci) {
            return None;
        }
        let word = self.channels[ci].read_word(memory);
        self.finish_cycle(ci, word.is_some());
        let word = word?;
        Some(word)
    }

    /// Run the 8237A memory-to-memory transfer the command register enables
    /// (bit0). A software request on channel 0 copies a block from channel 0's
    /// current address (the source) to channel 1's current address (the dest),
    /// for channel 1's current word count, one byte per transfer, until channel
    /// 1 reaches terminal count. Channel-0 address hold (command bit1) freezes
    /// the source address so a single source byte fills the destination block.
    ///
    /// Both channels step through the shared `step_transfer` datapath, so address
    /// increment/decrement, the count-through-zero terminal count, and auto-init
    /// reload all match a normal channel. Returns the number of bytes copied, or
    /// None when the controller is disabled, mem-to-mem is not enabled, or
    /// channel 0 is masked.
    // Limit: ceiling is a single-shot block copy in one call, not a per-cycle
    // DREQ/HRQ/HLDA handshake. The 8237A runs mem-to-mem as a burst that holds
    // the bus until channel-1 TC, so doing it in one pass is faithful to the
    // observable result; cycle-accurate bus arbitration is out of scope.
    fn mem_to_mem(&mut self, memory: &mut Memory) -> Option<usize> {
        if self.controller_disabled() || !self.mem_to_mem_enabled() {
            return None;
        }
        if self.channels[0].mask {
            return None;
        }
        let hold = self.channel0_hold();
        let mut copied = 0usize;
        self.channels[0].active = true;
        self.channels[1].active = true;
        loop {
            let src = self.channels[0].byte_address() as usize;
            let dst = self.channels[1].byte_address() as usize;
            let Ok(byte) = memory.read_u8(src) else {
                self.channels[0].active = false;
                self.channels[1].active = false;
                return None;
            };
            if memory.write_u8(dst, byte).is_err() {
                self.channels[0].active = false;
                self.channels[1].active = false;
                return None;
            }
            copied += 1;

            // Channel 1 (the destination) owns the word count and terminal count.
            self.channels[1].step_transfer();
            // Channel 0 (the source) advances its address and count too, unless
            // address hold freezes it for a memory fill.
            if hold {
                let c0 = &mut self.channels[0];
                c0.transfer_cycles = c0.transfer_cycles.saturating_add(1);
                let next = c0.cur_count.wrapping_sub(1);
                c0.cur_count = next;
            } else {
                self.channels[0].step_transfer();
            }

            if self.channels[1].reached_tc {
                self.status |= 1 << 1;
                break;
            }
        }
        // The 8237A resets the software DREQ when the channel reaches terminal
        // count. Clear the source/dest request bits so a later unrelated write to
        // the request register cannot re-trigger this copy.
        self.request_reg &= !0x03;
        self.channels[0].active = false;
        self.channels[1].active = false;
        Some(copied)
    }

    /// Write one byte from the device (device->memory) on local channel `ci`,
    /// latching terminal-count into the status register. The FDC READ DATA
    /// datapath reaches memory through here.
    fn write_byte(&mut self, ci: usize, memory: &mut Memory, byte: u8) -> Option<u32> {
        if !self.begin_cycle(ci) {
            return None;
        }
        let wrote = self.channels[ci].write_byte(memory, byte);
        self.finish_cycle(ci, wrote.is_some());
        wrote
    }

    /// Write one 16-bit word from the device (device->memory) on local channel
    /// `ci`, latching terminal-count into the status register.
    #[allow(dead_code)] // Limit: no Machine-level write wiring yet (see DmaChannel::write_byte).
    fn write_word(&mut self, ci: usize, memory: &mut Memory, word: u16) -> Option<()> {
        if !self.begin_cycle(ci) {
            return None;
        }
        let wrote = self.channels[ci].write_word(memory, word);
        self.finish_cycle(ci, wrote.is_some());
        wrote
    }

    /// Run one verify transfer on local channel `ci`, latching terminal-count
    /// into the status register. No memory is touched.
    #[allow(dead_code)] // Limit: no Machine-level verify wiring yet (see DmaChannel::write_byte).
    fn verify(&mut self, ci: usize) -> Option<()> {
        if !self.begin_cycle(ci) {
            return None;
        }
        let verified = self.channels[ci].verify();
        self.finish_cycle(ci, verified.is_some());
        verified
    }
}

/// The master/slave 8237A pair. Channels 0-3 are the master (8-bit); channels
/// 4-7 are the slave (16-bit on real hardware, modeled as byte reads here).
#[derive(Debug, Clone, Default)]
pub(crate) struct DmaController {
    pub(crate) master: DmaChip,
    slave: DmaChip,
    /// Scratch latches for the page ports that the PC/AT decodes but does not
    /// wire to a DMA channel (0x80, 0x84, 0x85, 0x86, 0x88, 0x8C, 0x8D, 0x8E).
    /// Software reads them back as plain R/W bytes; indexed by port low nibble.
    page_scratch: [u8; 16],
    /// Refresh page register at 0x8F; a read/write latch unrelated to any DMA
    /// channel (the refresh DRAM controller's page on the AT).
    refresh_page: u8,
}

impl DmaController {
    /// Translate a slave-controller port to a local register index, or None.
    fn slave_local(port: u16) -> Option<u8> {
        match port {
            0xC0 | 0xC2 | 0xC4 | 0xC6 | 0xC8 | 0xCA | 0xCC | 0xCE => {
                Some(((port - 0xC0) / 2) as u8)
            }
            0xD0 => Some(8),
            0xD2 => Some(9),
            0xD4 => Some(10),
            0xD6 => Some(11),
            0xD8 => Some(12),
            0xDA => Some(13),
            0xDC => Some(14),
            0xDE => Some(15),
            _ => None,
        }
    }

    /// IBM PC/AT page-register wiring. Note the address order is NOT channel
    /// order: 0x83->ch1, 0x81->ch2, 0x82->ch3, 0x87->ch0 (and the slave set).
    /// 0x8F is the refresh page and 0x84-0x86/0x8C-0x8E/0x80/0x88 are scratch,
    /// so neither appears here. Returns ("master"|"slave", local channel 0..3).
    fn page_target(port: u16) -> Option<(&'static str, usize)> {
        match port {
            0x83 => Some(("master", 1)),
            0x81 => Some(("master", 2)),
            0x82 => Some(("master", 3)),
            0x87 => Some(("master", 0)),
            0x8B => Some(("slave", 1)),
            0x89 => Some(("slave", 2)),
            0x8A => Some(("slave", 3)),
            _ => None,
        }
    }

    /// The page ports the AT decodes but leaves unconnected to any DMA channel.
    /// They behave as plain read/write scratch latches.
    // Limit: 0x80 is the AT's POST/manufacturing-test port and the rest of the
    // machine already latches it as a passive diagnostic register, so the DMA
    // scratch set deliberately excludes it (0x84-0x8E only). Claiming 0x80 here
    // would shadow that POST latch, which is wired ahead of the passive map.
    fn is_scratch_page(port: u16) -> bool {
        matches!(port, 0x84 | 0x85 | 0x86 | 0x88 | 0x8C | 0x8D | 0x8E)
    }

    pub(crate) fn write_port(&mut self, port: u16, value: u8) -> bool {
        if port <= 0x0F {
            self.master.write_local(port as u8, value);
            return true;
        }
        if let Some(local) = Self::slave_local(port) {
            self.slave.write_local(local, value);
            return true;
        }
        if let Some((chip, ci)) = Self::page_target(port) {
            match chip {
                "master" => self.master.channels[ci].page = value,
                _ => self.slave.channels[ci].page = value,
            }
            return true;
        }
        if port == 0x8F {
            self.refresh_page = value;
            return true;
        }
        if Self::is_scratch_page(port) {
            self.page_scratch[(port & 0x0F) as usize] = value;
            return true;
        }
        false
    }

    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        if port <= 0x0F {
            return self.master.read_local(port as u8);
        }
        if let Some(local) = Self::slave_local(port) {
            return self.slave.read_local(local);
        }
        // Page ports are plain R/W latches on the AT, so they read back what was
        // last written: channel pages mirror the channel register, 0x8F is the
        // refresh page, and the rest come from the scratch array.
        if let Some((chip, ci)) = Self::page_target(port) {
            return Some(match chip {
                "master" => self.master.channels[ci].page,
                _ => self.slave.channels[ci].page,
            });
        }
        if port == 0x8F {
            return Some(self.refresh_page);
        }
        if Self::is_scratch_page(port) {
            return Some(self.page_scratch[(port & 0x0F) as usize]);
        }
        None
    }

    /// Read one byte for DMA channel `channel` (0-3 master, 4-7 slave).
    pub(crate) fn read_byte(&mut self, channel: usize, memory: &mut Memory) -> Option<u8> {
        if channel < 4 {
            self.master.set_hardware_request(channel, true);
            let value = self.master.read_byte(channel, memory);
            self.master.set_hardware_request(channel, false);
            value
        } else {
            let local = channel - 4;
            self.slave.set_hardware_request(local, true);
            let value = self.slave.read_byte(local, memory);
            self.slave.set_hardware_request(local, false);
            value
        }
    }

    /// Read one 16-bit word for DMA channel `channel`. The slave (channels 4-7)
    /// drives the word-addressed path; the master channels (0-3, 8-bit) return
    /// None. The sound slice uses channel 5 for SB16 16-bit DMA output.
    pub(crate) fn read_word(&mut self, channel: usize, memory: &mut Memory) -> Option<u16> {
        if channel < 4 {
            None
        } else {
            let local = channel - 4;
            self.slave.set_hardware_request(local, true);
            let value = self.slave.read_word(local, memory);
            self.slave.set_hardware_request(local, false);
            value
        }
    }

    /// Write one byte to memory for DMA channel `channel` (device->memory, the
    /// write transfer direction). The floppy controller drives channel 2 this way
    /// to land read sector bytes in the guest's buffer; the channel's programmed
    /// address, page, count and terminal count all apply. Returns None when the
    /// channel is masked, not programmed for a write transfer, the controller is
    /// disabled, or the channel has already reached terminal count. A successful transfer returns
    /// the physical byte address written so CPU code-cache invalidation can stay range-aware.
    pub(crate) fn write_byte(
        &mut self,
        channel: usize,
        memory: &mut Memory,
        byte: u8,
    ) -> Option<u32> {
        if channel < 4 {
            self.master.set_hardware_request(channel, true);
            let wrote = self.master.write_byte(channel, memory, byte);
            self.master.set_hardware_request(channel, false);
            wrote
        } else {
            let local = channel - 4;
            self.slave.set_hardware_request(local, true);
            let wrote = self.slave.write_byte(local, memory, byte);
            self.slave.set_hardware_request(local, false);
            wrote
        }
    }

    /// Read one byte from memory for DMA channel `channel` on the byte-addressed
    /// (8-bit) path. The floppy controller drives channel 2 this way to pull
    /// WRITE DATA bytes out of the guest's buffer. Mirrors `read_byte` but is kept
    /// distinct so the call site reads as a device->disk pull. Returns None when
    /// masked, not a read transfer, the controller is disabled, or at TC.
    pub(crate) fn pull_byte(&mut self, channel: usize, memory: &mut Memory) -> Option<u8> {
        self.read_byte(channel, memory)
    }

    /// Whether DMA `channel` has reached terminal count. The floppy bridge checks
    /// this to stop a transfer at the programmed byte count even when the disk
    /// could supply more sectors.
    pub(crate) fn at_terminal_count(&self, channel: usize) -> bool {
        let (chip, local) = if channel < 4 {
            (&self.master, channel)
        } else {
            (&self.slave, channel - 4)
        };
        chip.channels[local].reached_tc
    }

    /// Run a memory-to-memory block transfer on the master controller, the only
    /// 8237A that wires mem-to-mem (channel 0 source, channel 1 dest). Driven by
    /// the master's command register: bit0 enables the path, bit1 holds the
    /// source for a fill, bit2 disables the whole controller. Returns the byte
    /// count copied, or None when not enabled or the controller is disabled.
    // Limit: only the master pair carries the mem-to-mem hardware; the slave
    // 8237A never does on the PC/AT, so no slave variant exists.
    pub(crate) fn mem_to_mem(&mut self, memory: &mut Memory) -> Option<usize> {
        self.master.mem_to_mem(memory)
    }

    /// Whether a software DREQ on master channel 0 is armed to launch a
    /// memory-to-memory transfer. The machine checks this after a request-register
    /// write (port 0x09) and, when true, calls `mem_to_mem` to move the block.
    pub(crate) fn mem_to_mem_request_armed(&self) -> bool {
        self.master.mem_to_mem_request_armed()
    }
}

#[cfg(test)]
#[path = "dma_test.rs"]
mod tests;
