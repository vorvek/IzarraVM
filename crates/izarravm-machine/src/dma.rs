//! Intel 8237A DMA controller, a master/slave cascade pair.
//!
//! Built clean-room from the Intel 8237A datasheet cached at
//! dev_docs/reference/8237a/. Single-transfer and auto-init modes are modeled
//! (the two the Sound Blaster uses for 8-bit playback); demand, block, cascade
//! and memory-to-memory modes are out of scope.

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
    pub transfer_kind: u8,    // mode bits2-3: 0 verify, 1 write(m->i/o), 2 read(i/o->m)
    pub mask: bool,           // mask register bit
    pub reached_tc: bool,
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
            mask: true,
            reached_tc: false,
        }
    }
}

impl DmaChannel {
    /// Mode register write (bits 2-3 transfer kind, bit4 auto-init, bit5 addr dec).
    pub(crate) fn set_mode(&mut self, value: u8) {
        self.transfer_kind = (value >> 2) & 0x3;
        self.auto_init = value & 0x10 != 0;
        self.addr_decrement = value & 0x20 != 0;
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
    fn step_after_read(&mut self) {
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
    /// channel. Returns None when masked or already at terminal count.
    pub(crate) fn read_byte(&mut self, memory: &mut Memory) -> Option<u8> {
        if self.mask {
            return None;
        }
        let byte = memory.read_u8(self.byte_address() as usize).ok()?;
        self.step_after_read();
        Some(byte)
    }

    /// Read one little-endian word from memory on the slave's word-addressed path
    /// (memory->device, 16-bit DMA). The counter steps in words, exactly as the
    /// byte path steps in bytes; only the address formation differs. Returns None
    /// when masked or already at terminal count.
    pub(crate) fn read_word(&mut self, memory: &mut Memory) -> Option<u16> {
        if self.mask {
            return None;
        }
        let addr = self.word_address() as usize;
        let lo = memory.read_u8(addr).ok()?;
        let hi = memory.read_u8(addr + 1).ok()?;
        self.step_after_read();
        Some(u16::from_le_bytes([lo, hi]))
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
        if local < 8 && local % 2 == 0 {
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
                    // Status read returns terminal-count bits and clears them.
                    let s = self.status;
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
        self.channels.iter_mut().for_each(|c| c.mask = true);
    }

    /// Read one byte from the device (memory->device) on local channel `ci`,
    /// latching terminal-count into the status register.
    fn read_byte(&mut self, ci: usize, memory: &mut Memory) -> Option<u8> {
        let byte = self.channels[ci].read_byte(memory)?;
        if self.channels[ci].reached_tc {
            self.status |= 1 << ci;
        }
        Some(byte)
    }

    /// Read one 16-bit word from the device (memory->device) on local channel
    /// `ci`, latching terminal-count into the status register.
    fn read_word(&mut self, ci: usize, memory: &mut Memory) -> Option<u16> {
        let word = self.channels[ci].read_word(memory)?;
        if self.channels[ci].reached_tc {
            self.status |= 1 << ci;
        }
        Some(word)
    }
}

/// The master/slave 8237A pair. Channels 0-3 are the master (8-bit); channels
/// 4-7 are the slave (16-bit on real hardware, modeled as byte reads here).
#[derive(Debug, Clone, Default)]
pub(crate) struct DmaController {
    pub(crate) master: DmaChip,
    slave: DmaChip,
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
    fn page_target(port: u16) -> Option<(&'static str, usize)> {
        // Returns ("master"|"slave", local channel index 0..3).
        match port {
            0x83 => Some(("master", 1)),
            0x81 => Some(("master", 2)),
            0x82 => Some(("master", 3)),
            0x87 => Some(("master", 0)),
            0x8B => Some(("slave", 1)),
            0x89 => Some(("slave", 2)),
            0x8A => Some(("slave", 3)),
            0x8F => Some(("slave", 0)),
            _ => None,
        }
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
        false
    }

    pub(crate) fn read_port(&mut self, port: u16) -> Option<u8> {
        if port <= 0x0F {
            return self.master.read_local(port as u8);
        }
        if let Some(local) = Self::slave_local(port) {
            return self.slave.read_local(local);
        }
        // Page registers are write-only on the PC; reads fall through to open bus.
        None
    }

    /// Read one byte for DMA channel `channel` (0-3 master, 4-7 slave).
    pub(crate) fn read_byte(&mut self, channel: usize, memory: &mut Memory) -> Option<u8> {
        if channel < 4 {
            self.master.read_byte(channel, memory)
        } else {
            // Slave channels are 16-bit on real hardware; modeled byte-wise here.
            self.slave.read_byte(channel - 4, memory)
        }
    }

    /// Read one 16-bit word for DMA channel `channel`. The slave (channels 4-7)
    /// drives the word-addressed path; the master channels (0-3, 8-bit) return
    /// None. The sound slice uses channel 5 for SB16 16-bit DMA output.
    pub(crate) fn read_word(&mut self, channel: usize, memory: &mut Memory) -> Option<u16> {
        if channel < 4 {
            None
        } else {
            self.slave.read_word(channel - 4, memory)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use izarravm_bus::Memory;

    fn mem_with(addr: u32, bytes: &[u8]) -> Memory {
        let mut m = Memory::new((addr as usize) + bytes.len()).unwrap();
        for (i, &b) in bytes.iter().enumerate() {
            m.write_u8(addr as usize + i, b).unwrap();
        }
        m
    }

    #[test]
    fn programming_channel_1_round_trips_through_ports() {
        let mut dma = DmaController::default();
        dma.write_port(0x0B, 0x49); // mode register, channel 1: single, read
        dma.write_port(0x02, 0x34); // base/current address LSB
        dma.write_port(0x02, 0x12); // ...MSB -> 0x1234
        dma.write_port(0x03, 0x0F); // base/current count LSB
        dma.write_port(0x03, 0x00); // ...MSB -> 0x000F
        dma.write_port(0x83, 0x05); // page register for channel 1 = 0x05
        dma.write_port(0x0A, 0x01); // clear mask for channel 1

        let ch = &dma.master.channels[1];
        assert_eq!(ch.base_addr, 0x1234);
        assert_eq!(ch.base_count, 0x000F);
        assert_eq!(ch.page, 0x05);
        assert!(!ch.mask);
        // Read-back of current address reuses the same flip-flop (LSB then MSB).
        assert_eq!(dma.read_port(0x02), Some(0x34));
        assert_eq!(dma.read_port(0x02), Some(0x12));
    }

    #[test]
    fn page_registers_use_the_ibm_at_address_order() {
        let mut dma = DmaController::default();
        dma.write_port(0x83, 0x11);
        dma.write_port(0x81, 0x22);
        dma.write_port(0x82, 0x33);
        dma.write_port(0x87, 0x44);
        assert_eq!(dma.master.channels[1].page, 0x11); // 0x83 -> ch1
        assert_eq!(dma.master.channels[2].page, 0x22); // 0x81 -> ch2
        assert_eq!(dma.master.channels[3].page, 0x33); // 0x82 -> ch3
        assert_eq!(dma.master.channels[0].page, 0x44); // 0x87 -> ch0
    }

    #[test]
    fn status_reports_terminal_count_after_a_transfer() {
        let mut dma = DmaController::default();
        // channel 1: address 0x10, page 0, count 0 -> 1 transfer
        dma.write_port(0x0B, 0x49);
        dma.write_port(0x02, 0x10);
        dma.write_port(0x02, 0x00);
        dma.write_port(0x03, 0x00);
        dma.write_port(0x03, 0x00);
        dma.write_port(0x0A, 0x01); // unmask ch1
        let mut mem = mem_with(0x0010, &[0x77]);
        assert_eq!(dma.read_byte(1, &mut mem), Some(0x77));
        // Status bit 1 latched; reading the status register returns and clears it.
        assert_eq!(dma.read_port(0x08), Some(0x02));
        assert_eq!(dma.read_port(0x08), Some(0x00), "TC bits cleared on read");
    }

    #[test]
    fn single_transfer_reads_advances_and_signals_tc() {
        // Channel 1: page 0x00, base address 0x0010, count 2 (3 transfers: n+1).
        let mut ch = DmaChannel {
            base_addr: 0x0010,
            cur_addr: 0x0010,
            base_count: 2,
            cur_count: 2,
            page: 0x00,
            mask: false,
            ..Default::default()
        };
        ch.set_mode(0x49); // single transfer, read, auto-init off, ch1

        let mut mem = mem_with(0x0010, &[0x11, 0x22, 0x33]);
        let b0 = ch.read_byte(&mut mem).unwrap();
        let b1 = ch.read_byte(&mut mem).unwrap();
        let b2 = ch.read_byte(&mut mem).unwrap();
        assert_eq!([b0, b1, b2], [0x11, 0x22, 0x33]);
        assert!(ch.reached_tc);
        assert!(ch.mask, "single mode masks the channel at TC");
        assert_eq!(ch.read_byte(&mut mem), None, "no more data after TC");
    }

    #[test]
    fn auto_init_reloads_from_base_at_tc() {
        let mut ch = DmaChannel {
            base_addr: 0x0008,
            cur_addr: 0x0008,
            base_count: 1, // 2 transfers per cycle
            cur_count: 1,
            mask: false,
            ..Default::default()
        };
        ch.set_mode(0x59); // auto-init on

        let mut mem = mem_with(0x0008, &[0xAA, 0xBB]);
        let _ = ch.read_byte(&mut mem);
        let second = ch.read_byte(&mut mem).unwrap(); // TC -> reload
        assert!(ch.reached_tc);
        assert!(!ch.mask, "auto-init keeps the channel unmasked");
        assert_eq!(second, 0xBB);
        assert_eq!(ch.cur_addr, ch.base_addr, "address reloaded from base");
        assert_eq!(ch.cur_count, ch.base_count, "count reloaded from base");
        assert_eq!(ch.read_byte(&mut mem).unwrap(), 0xAA, "restarts the buffer");
    }

    #[test]
    fn slave_channel_5_reads_word_little_endian_and_steps_in_words() {
        // Channel 5 = slave local channel 1, page 0x8B.
        // Slave ports: 0xC4/0xC6 (stride-2 local 1), mode 0xD6, mask 0xD4.
        let mut dma = DmaController::default();
        dma.write_port(0xD6, 0x49); // mode, slave ch1: single, read, auto-init off
        dma.write_port(0xC4, 0x10); // slave ch1 address LSB
        dma.write_port(0xC4, 0x00); // ...MSB -> word addr 0x0010
        dma.write_port(0xC6, 0x00); // slave ch1 count LSB
        dma.write_port(0xC6, 0x00); // ...MSB -> 0 (1 word transfer)
        dma.write_port(0x8B, 0x01); // page -> byte base 0x01_0000 + (0x0010<<1)
        dma.write_port(0xD4, 0x01); // unmask slave ch1 (channel 5)

        // Seed two bytes at the word-aligned byte address.
        let byte_addr = (0x01u32 << 17) | (0x0010u32 << 1);
        let mut mem = Memory::new(byte_addr as usize + 4).unwrap();
        mem.write_u8(byte_addr as usize, 0x34).unwrap();
        mem.write_u8(byte_addr as usize + 1, 0x12).unwrap();

        let word = dma.read_word(5, &mut mem).expect("a word from channel 5");
        assert_eq!(word, 0x1234, "little-endian word read");
        assert!(dma.slave.channels[1].reached_tc);
        assert!(dma.slave.channels[1].mask, "single mode masks at TC");
        assert_eq!(dma.read_word(5, &mut mem), None);
    }

    #[test]
    fn slave_channel_5_auto_init_reloads_and_keeps_feeding() {
        let mut dma = DmaController::default();
        dma.write_port(0xD6, 0x59); // mode, slave ch1: auto-init, read
        dma.write_port(0xC4, 0x00); // word addr 0
        dma.write_port(0xC4, 0x00);
        dma.write_port(0xC6, 0x01); // count 1 -> 2 word transfers per cycle
        dma.write_port(0xC6, 0x00);
        dma.write_port(0x8B, 0x00); // page 0 -> byte base 0
        dma.write_port(0xD4, 0x01); // unmask slave ch1

        let byte_addr = (0x00u32 << 17) | (0x0000u32 << 1);
        let mut mem = Memory::new(byte_addr as usize + 4).unwrap();
        mem.write_u8(byte_addr as usize, 0x78).unwrap();
        mem.write_u8(byte_addr as usize + 1, 0x56).unwrap();

        let w0 = dma.read_word(5, &mut mem).unwrap();
        let _tc = dma.read_word(5, &mut mem).unwrap(); // TC -> auto-init reload
        assert!(dma.slave.channels[1].reached_tc);
        assert!(
            !dma.slave.channels[1].mask,
            "auto-init keeps the channel live"
        );
        // After reload the address is back at the base, so the next word repeats.
        assert_eq!(w0, 0x5678);
        assert_eq!(dma.read_word(5, &mut mem), Some(0x5678), "buffer restarts");
    }
}
