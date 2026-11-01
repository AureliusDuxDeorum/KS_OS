//! Builders for valid sample tables, shared by the integration tests.

#![allow(dead_code)]

pub fn with_checksum(mut t: Vec<u8>, checksum_at: usize) -> Vec<u8> {
    t[checksum_at] = 0;
    let sum = t.iter().fold(0u8, |a, &x| a.wrapping_add(x));
    t[checksum_at] = 0u8.wrapping_sub(sum);
    t
}

pub fn sdt(sig: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut t = vec![0u8; 36];
    t[0..4].copy_from_slice(sig);
    t[8] = 1;
    t.extend_from_slice(body);
    let len = t.len() as u32;
    t[4..8].copy_from_slice(&len.to_le_bytes());
    with_checksum(t, 9)
}

pub fn madt(entries: &[&[u8]]) -> Vec<u8> {
    let mut body = vec![0u8; 8];
    body[0..4].copy_from_slice(&0xFEE0_0000u32.to_le_bytes());
    for e in entries {
        body.extend_from_slice(e);
    }
    sdt(b"APIC", &body)
}

pub fn sample_madt() -> Vec<u8> {
    madt(&[
        &[0, 8, 0, 0, 1, 0, 0, 0],
        &[1, 12, 2, 0, 0x00, 0x00, 0xC0, 0xFE, 0, 0, 0, 0],
        &[2, 10, 0, 0, 2, 0, 0, 0, 0, 0],
        &[2, 10, 0, 9, 9, 0, 0, 0, 0x0F, 0],
        &[4, 6, 0xFF, 0x00, 0x00, 1],
        &[5, 12, 0, 0, 0, 0, 0xE0, 0xFE, 0, 0, 0, 0],
    ])
}

pub fn sample_mcfg() -> Vec<u8> {
    let mut body = vec![0u8; 8];
    for (base, start, end) in [(0xB000_0000u64, 0u8, 255u8), (0xC000_0000, 0, 15)] {
        body.extend_from_slice(&base.to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes());
        body.push(start);
        body.push(end);
        body.extend_from_slice(&[0; 4]);
    }
    sdt(b"MCFG", &body)
}

pub fn sample_dsdt() -> Vec<u8> {
    // Name(\_S5_, Package(4){ 0x05, 0x00, 0, 0 })
    let aml = [
        0x08, 0x5F, 0x53, 0x35, 0x5F, 0x12, 0x06, 0x04, 0x0A, 0x05, 0x0A, 0x00, 0x00, 0x00,
    ];
    sdt(b"DSDT", &aml)
}

pub fn sample_fadt() -> Vec<u8> {
    let mut body = vec![0u8; 244 - 36];
    let mut put32 =
        |off: usize, v: u32| body[off - 36..off - 36 + 4].copy_from_slice(&v.to_le_bytes());
    put32(40, 0x1234_0000); // DSDT
    put32(48, 0xB2); // SMI_CMD
    put32(64, 0x604); // PM1a_CNT
    put32(76, 0x608); // PM_TMR_BLK
    put32(112, 1 << 8); // flags: TMR_VAL_EXT
    body[52 - 36] = 0xA0; // ACPI_ENABLE
    sdt(b"FACP", &body)
}

pub fn sample_rsdp(revision: u8) -> Vec<u8> {
    let mut r = vec![0u8; if revision >= 2 { 36 } else { 20 }];
    r[0..8].copy_from_slice(b"RSD PTR ");
    r[15] = revision;
    r[16..20].copy_from_slice(&0x1000u32.to_le_bytes());
    if revision >= 2 {
        r[20..24].copy_from_slice(&36u32.to_le_bytes());
        r[24..32].copy_from_slice(&0x2000u64.to_le_bytes());
    }
    // Basic checksum covers bytes 0..20 only.
    let sum = r[..20].iter().fold(0u8, |a, &x| a.wrapping_add(x));
    r[8] = 0u8.wrapping_sub(sum);
    if revision >= 2 {
        // Extended checksum covers all 36 bytes.
        let sum = r.iter().fold(0u8, |a, &x| a.wrapping_add(x));
        r[32] = 0u8.wrapping_sub(sum);
    }
    r
}

/// A 4 KiB PCI configuration space of an endpoint with an MSI and an MSI-X capability.
pub fn sample_config_space() -> Vec<u8> {
    let mut c = vec![0u8; 4096];
    c[0..2].copy_from_slice(&0x8086u16.to_le_bytes());
    c[2..4].copy_from_slice(&0x1234u16.to_le_bytes());
    c[6..8].copy_from_slice(&0x0010u16.to_le_bytes()); // status: capability list
    c[8] = 1; // revision
    c[9] = 0x30; // prog-if
    c[10] = 0x03; // subclass
    c[11] = 0x0C; // class: serial bus (USB)
    c[0x0E] = 0x80; // header type 0, multifunction
    c[0x34] = 0x40; // capability pointer
                    // MSI at 0x40 (64-bit, 8 vectors capable), next 0x50
    c[0x40] = 0x05;
    c[0x41] = 0x50;
    c[0x42..0x44].copy_from_slice(&(0x0080u16 | (3 << 1)).to_le_bytes());
    // MSI-X at 0x50, table size 8 in BAR 2 at 0x1000, PBA in BAR 2 at 0x2000
    c[0x50] = 0x11;
    c[0x51] = 0x00;
    c[0x52..0x54].copy_from_slice(&7u16.to_le_bytes());
    c[0x54..0x58].copy_from_slice(&(0x1000u32 | 2).to_le_bytes());
    c[0x58..0x5C].copy_from_slice(&(0x2000u32 | 2).to_le_bytes());
    c
}

pub const BOOT_KEYBOARD_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];

pub const BOOT_MOUSE_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x03,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03,
    0x81, 0x06, 0xC0, 0xC0,
];

/// Configuration descriptor: one HID boot-keyboard interface with an interrupt IN endpoint.
pub fn sample_usb_config() -> Vec<u8> {
    let mut c = vec![9, 2, 0, 0, 1, 1, 0, 0xA0, 50];
    c.extend_from_slice(&[9, 4, 0, 0, 1, 3, 1, 1, 0]); // interface: HID, boot, keyboard
    c.extend_from_slice(&[9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0]); // HID descriptor: report length 63
    c.extend_from_slice(&[7, 5, 0x81, 3, 8, 0, 10]); // endpoint 1 IN, interrupt, 8 bytes, interval 10
    let len = c.len() as u16;
    c[2..4].copy_from_slice(&len.to_le_bytes());
    c
}
