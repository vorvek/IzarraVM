//! Raw `.SYS` device-driver loading helpers: header parse, the SYSINIT INIT
//! request packet, and the status read-back. The CPU far-call that runs INIT
//! lives in izarravm-machine; these are the memory-only pieces.

use crate::DosError;
use izarravm_bus::Memory;

/// Length of a DOS device header: next ptr (4) + attribute (2) + strategy (2) +
/// interrupt (2) + name/unit field (8).
pub const DEVICE_HEADER_LEN: usize = 0x12;
/// Length of the INIT request header we build (through the first-drive byte).
pub const INIT_REQUEST_LEN: u8 = 0x17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverLoadError {
    /// MZ/EXE-format driver; raw `.SYS` only in slice 4a.
    UnsupportedFormat,
    /// Header shorter than 18 bytes, or a strategy/interrupt offset past the image.
    MalformedHeader,
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceHeaderInfo {
    pub attributes: u16,
    pub strategy: u16,
    pub interrupt: u16,
    pub name: [u8; 8],
}

impl DeviceHeaderInfo {
    pub fn is_character_device(&self) -> bool {
        self.attributes & 0x8000 != 0
    }
}

/// Validate the device header at offset 0 of a raw `.SYS` image.
pub fn parse_device_header(image: &[u8]) -> Result<DeviceHeaderInfo, DriverLoadError> {
    if image.len() >= 2 && (&image[..2] == b"MZ" || &image[..2] == b"ZM") {
        return Err(DriverLoadError::UnsupportedFormat);
    }
    if image.len() < DEVICE_HEADER_LEN {
        return Err(DriverLoadError::MalformedHeader);
    }
    let attributes = u16::from_le_bytes([image[4], image[5]]);
    let strategy = u16::from_le_bytes([image[6], image[7]]);
    let interrupt = u16::from_le_bytes([image[8], image[9]]);
    let len = image.len();
    if usize::from(strategy) >= len || usize::from(interrupt) >= len {
        return Err(DriverLoadError::MalformedHeader);
    }
    let mut name = [0u8; 8];
    name.copy_from_slice(&image[10..18]);
    Ok(DeviceHeaderInfo {
        attributes,
        strategy,
        interrupt,
        name,
    })
}

/// Build the command-0 (INIT) request header at `req_linear`. `break_default` and
/// `arg_ptr` are (segment, offset) far pointers; `break_default` seeds the break
/// address to the image end, `arg_ptr` points at the CONFIG.SYS argument tail.
pub fn build_init_request(
    mem: &mut Memory,
    req_linear: usize,
    break_default: (u16, u16),
    arg_ptr: (u16, u16),
) -> Result<(), DosError> {
    for i in 0..usize::from(INIT_REQUEST_LEN) {
        mem.write_u8(req_linear + i, 0)?;
    }
    mem.write_u8(req_linear, INIT_REQUEST_LEN)?; // +0x00 header length
    mem.write_u8(req_linear + 0x01, 0)?; // unit
    mem.write_u8(req_linear + 0x02, 0)?; // command 0 = INIT
    mem.write_u16(req_linear + 0x03, 0)?; // status (driver fills)
    mem.write_u16(req_linear + 0x0e, break_default.1)?; // break off
    mem.write_u16(req_linear + 0x10, break_default.0)?; // break seg
    mem.write_u16(req_linear + 0x12, arg_ptr.1)?; // arg off
    mem.write_u16(req_linear + 0x14, arg_ptr.0)?; // arg seg
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct InitResult {
    pub done: bool,
    pub error: bool,
    pub break_seg: u16,
    pub break_off: u16,
}

/// Read the request-header status word and break address after INIT ran.
pub fn read_init_result(mem: &Memory, req_linear: usize) -> Result<InitResult, DosError> {
    let status = mem.read_u16(req_linear + 0x03)?;
    let break_off = mem.read_u16(req_linear + 0x0e)?;
    let break_seg = mem.read_u16(req_linear + 0x10)?;
    Ok(InitResult {
        done: status & 0x0100 != 0,
        error: status & 0x8000 != 0,
        break_seg,
        break_off,
    })
}

#[cfg(test)]
pub(crate) fn tests_char_image() -> Vec<u8> {
    // 18-byte header + a few code bytes. next=FFFFFFFF, attr=0x8000 (char),
    // strategy off=0x12, interrupt off=0x14, name "TESTDEV ".
    let mut img = vec![0u8; 0x20];
    img[0..4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    img[4..6].copy_from_slice(&0x8000u16.to_le_bytes());
    img[6..8].copy_from_slice(&0x0012u16.to_le_bytes());
    img[8..10].copy_from_slice(&0x0014u16.to_le_bytes());
    img[10..18].copy_from_slice(b"TESTDEV ");
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_valid_character_device_header() {
        let info = parse_device_header(&tests_char_image()).unwrap();
        assert_eq!(info.attributes, 0x8000);
        assert_eq!(info.strategy, 0x0012);
        assert_eq!(info.interrupt, 0x0014);
        assert_eq!(&info.name, b"TESTDEV ");
        assert!(info.is_character_device());
    }

    #[test]
    fn rejects_an_mz_image_as_unsupported() {
        let mut img = tests_char_image();
        img[0] = b'M';
        img[1] = b'Z';
        assert!(matches!(
            parse_device_header(&img),
            Err(DriverLoadError::UnsupportedFormat)
        ));
    }

    #[test]
    fn rejects_an_entry_offset_past_the_image() {
        let mut img = tests_char_image();
        img[6..8].copy_from_slice(&0x9000u16.to_le_bytes()); // strategy past end
        assert!(matches!(
            parse_device_header(&img),
            Err(DriverLoadError::MalformedHeader)
        ));
    }

    #[test]
    fn builds_and_reads_back_the_init_request() {
        let mut mem = Memory::new(0x20000).unwrap();
        let req = 0x1000usize;
        build_init_request(&mut mem, req, (0x0700, 0x0010), (0x0050, 0x0005)).unwrap();
        assert_eq!(mem.read_u8(req).unwrap(), 0x17); // +0x00 length
        assert_eq!(mem.read_u8(req + 0x02).unwrap(), 0x00); // INIT command
        assert_eq!(mem.read_u16(req + 0x03).unwrap(), 0x0000); // seeded status
        assert_eq!(mem.read_u16(req + 0x12).unwrap(), 0x0005); // arg off
        assert_eq!(mem.read_u16(req + 0x14).unwrap(), 0x0050); // arg seg
        // Driver writes DONE + break address; read_init_result decodes it.
        mem.write_u16(req + 0x03, 0x0100).unwrap(); // DONE, no error
        mem.write_u16(req + 0x0e, 0x0040).unwrap(); // break off
        mem.write_u16(req + 0x10, 0x0700).unwrap(); // break seg
        let r = read_init_result(&mem, req).unwrap();
        assert!(r.done && !r.error);
        assert_eq!(r.break_seg, 0x0700);
        assert_eq!(r.break_off, 0x0040);
    }
}
