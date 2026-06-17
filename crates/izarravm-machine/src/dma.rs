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

    fn address(&self) -> u32 {
        (u32::from(self.page) << 16) | u32::from(self.cur_addr)
    }

    /// Read one byte from memory (memory->device read transfer) and step the
    /// channel. Returns None when masked or already at terminal count.
    pub(crate) fn read_byte(&mut self, memory: &mut Memory) -> Option<u8> {
        if self.mask {
            return None;
        }
        let addr = self.address() as usize;
        let byte = memory.read_u8(addr).ok()?;
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
        Some(byte)
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
    fn single_transfer_reads_advances_and_signals_tc() {
        // Channel 1: page 0x00, base address 0x0010, count 2 (3 transfers: n+1).
        let mut ch = DmaChannel::default();
        ch.base_addr = 0x0010;
        ch.cur_addr = 0x0010;
        ch.base_count = 2;
        ch.cur_count = 2;
        ch.page = 0x00;
        ch.set_mode(0x49); // single transfer, read, auto-init off, ch1
        ch.mask = false;

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
        let mut ch = DmaChannel::default();
        ch.base_addr = 0x0008;
        ch.cur_addr = 0x0008;
        ch.base_count = 1; // 2 transfers per cycle
        ch.cur_count = 1;
        ch.set_mode(0x59); // auto-init on
        ch.mask = false;

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
}
