// SPDX-License-Identifier: Apache-2.0
mod support;
use ksparse::madt::{self, Polarity, Trigger};
use ksparse::pci::{self, BarKind, Config};
use ksparse::sdt::{self, Fadt, Rsdp, SdtError};
use ksparse::{mcfg, pci::Capability};
use support::*;

#[test]
fn madt_valid() {
    let m = madt::parse(&sample_madt()).unwrap();
    assert_eq!(m.processors, 1);
    assert_eq!(m.ioapics.len(), 1);
    assert_eq!(m.ioapics[0].address, 0xFEC0_0000);
    assert_eq!(m.isa_irq(0).0, 2);
    assert_eq!(m.isa_irq(9), (9, Polarity::ActiveLow, Trigger::Level));
    assert_eq!(m.isa_irq(4), (4, Polarity::ActiveHigh, Trigger::Edge));
    assert_eq!(m.nmis[0].lint, 1);
    assert_eq!(m.nmis[0].processor_uid, u32::MAX);
    assert_eq!(
        m.lapic_address, 0xFEE0_0000,
        "address override entry applied"
    );
}

#[test]
fn madt_rejects_garbage() {
    assert_eq!(madt::parse(&[]), Err(madt::MadtError::TooShort));
    let mut t = sample_madt();
    t[0] = b'X';
    assert_eq!(madt::parse(&t), Err(madt::MadtError::BadSignature));
    // Zero-length and overlong entries terminate parsing safely.
    assert!(madt::parse(&madt(&[&[0, 0, 0, 0], &[0, 8, 0, 0, 1, 0, 0, 0]])).is_ok());
    assert!(madt::parse(&madt(&[&[2, 250, 0, 0]]))
        .unwrap()
        .overrides
        .is_empty());
}

#[test]
fn madt_every_prefix_is_safe() {
    let t = sample_madt();
    for n in 0..=t.len() {
        let _ = madt::parse(&t[..n]);
    }
}

#[test]
fn rsdp_variants() {
    let v1 = Rsdp::parse(&sample_rsdp(0)).unwrap();
    assert_eq!(v1.root(), (0x1000, false));
    let v2 = Rsdp::parse(&sample_rsdp(2)).unwrap();
    assert_eq!(v2.root(), (0x2000, true));
    let mut bad = sample_rsdp(2);
    bad[10] ^= 1;
    assert_eq!(Rsdp::parse(&bad), Err(SdtError::BadChecksum));
    assert_eq!(Rsdp::parse(&[0u8; 20]), Err(SdtError::BadSignature));
    assert_eq!(Rsdp::parse(&[1, 2, 3]), Err(SdtError::TooShort));
}

#[test]
fn tables_validate() {
    let t = sample_madt();
    assert_eq!(sdt::validate_table(&t).unwrap().len(), t.len());
    let mut corrupted = t.clone();
    let last = corrupted.len() - 1;
    corrupted[last] ^= 0xFF;
    assert_eq!(sdt::validate_table(&corrupted), Err(SdtError::BadChecksum));
    let mut huge = t.clone();
    huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(sdt::validate_table(&huge), Err(SdtError::BadLength));
    assert_eq!(sdt::validate_table(&t[..10]), Err(SdtError::TooShort));
}

#[test]
fn fadt_and_s5() {
    let f = Fadt::parse(&sample_fadt()).unwrap();
    assert_eq!(
        (f.smi_cmd, f.acpi_enable, f.pm1a_cnt, f.pm_tmr_blk),
        (0xB2, 0xA0, 0x604, 0x608)
    );
    assert!(f.tmr_32bit);
    assert_eq!(f.dsdt, 0x1234_0000);
    assert_eq!(sdt::find_s5(&sample_dsdt()), Some((5, 0)));
    assert_eq!(sdt::find_s5(&[0u8; 100]), None);
}

#[test]
fn root_entries_widths() {
    let mut body = Vec::new();
    for a in [0x1000u64, 0x2000, 0x3000] {
        body.extend_from_slice(&a.to_le_bytes());
    }
    let xsdt = sdt(b"XSDT", &body);
    assert_eq!(
        sdt::root_entries(&xsdt, true).collect::<Vec<_>>(),
        vec![0x1000, 0x2000, 0x3000]
    );
    let mut body32 = Vec::new();
    for a in [0x10u32, 0x20] {
        body32.extend_from_slice(&a.to_le_bytes());
    }
    let rsdt = sdt(b"RSDT", &body32);
    assert_eq!(
        sdt::root_entries(&rsdt, false).collect::<Vec<_>>(),
        vec![0x10, 0x20]
    );
}

#[test]
fn mcfg_regions() {
    let r = mcfg::parse(&sample_mcfg()).unwrap();
    assert_eq!(r.len(), 2);
    assert_eq!(
        r[0].function_address(0, 1, 0),
        Some(0xB000_0000 + (1 << 15))
    );
    assert_eq!(r[1].function_address(16, 0, 0), None);
    assert_eq!(
        r[0].function_address(3, 31, 7),
        Some(0xB000_0000 + (3 << 20) + (31 << 15) + (7 << 12))
    );
    assert_eq!(r[0].function_address(0, 32, 0), None);
    assert!(mcfg::parse(&[0u8; 10]).is_err());
}

#[test]
fn pci_header_and_caps() {
    let c = sample_config_space();
    let h = pci::header(c.as_slice()).unwrap();
    assert_eq!(
        (h.vendor, h.device, h.class, h.subclass, h.prog_if),
        (0x8086, 0x1234, 0x0C, 0x03, 0x30)
    );
    assert!(h.multifunction);
    assert_eq!(h.kind, 0);
    let caps = pci::capabilities(c.as_slice(), &h);
    assert_eq!(
        caps,
        vec![
            Capability {
                id: 0x05,
                offset: 0x40
            },
            Capability {
                id: 0x11,
                offset: 0x50
            }
        ]
    );
    let msi = pci::msi(c.as_slice(), caps[0]).unwrap();
    assert!(msi.is_64bit && !msi.per_vector_mask);
    assert_eq!(msi.max_vectors_log2, 3);
    let x = pci::msix(c.as_slice(), caps[1]).unwrap();
    assert_eq!(
        (
            x.table_size,
            x.table_bar,
            x.table_offset,
            x.pba_bar,
            x.pba_offset
        ),
        (8, 2, 0x1000, 2, 0x2000)
    );
    assert_eq!(x.table_range(), Some((0x1000, 8 * 16)));
}

#[test]
fn pci_absent_device() {
    assert!(pci::header([0xFFu8; 64].as_slice()).is_none());
}

#[test]
fn pci_capability_cycle_terminates() {
    let mut c = sample_config_space();
    c[0x51] = 0x40; // MSI-X points back to MSI
    let h = pci::header(c.as_slice()).unwrap();
    assert_eq!(pci::capabilities(c.as_slice(), &h).len(), 2);
    // A self-referencing list.
    let mut c = sample_config_space();
    c[0x41] = 0x40;
    let h = pci::header(c.as_slice()).unwrap();
    assert_eq!(pci::capabilities(c.as_slice(), &h).len(), 1);
}

#[test]
fn pci_capability_pointer_out_of_range() {
    let mut c = sample_config_space();
    c[0x34] = 0x10; // inside the standard header: invalid
    let h = pci::header(c.as_slice()).unwrap();
    assert!(pci::capabilities(c.as_slice(), &h).is_empty());
}

#[test]
fn bar_decoding() {
    // 32-bit memory BAR, 16 KiB, non-prefetchable.
    let (b, n) = pci::decode_bar(&[0xFEBF_0000, 0], &[0xFFFF_C000, 0], 0).unwrap();
    assert_eq!(
        (b.kind, b.size, b.base, n, b.prefetchable),
        (BarKind::Mem32, 0x4000, 0xFEBF_0000, 1, false)
    );
    // 64-bit prefetchable BAR, 256 MiB, above 4 GiB.
    let (b, n) =
        pci::decode_bar(&[0x0000_000C, 0x0000_0001], &[0xF000_000C, 0xFFFF_FFFF], 0).unwrap();
    assert_eq!(
        (b.kind, b.size, b.base, n, b.prefetchable),
        (BarKind::Mem64, 0x1000_0000, 0x1_0000_0000, 2, true)
    );
    // I/O BAR, 32 ports.
    let (b, n) = pci::decode_bar(&[0xC001, 0], &[0xFFFF_FFE1, 0], 0).unwrap();
    assert_eq!((b.kind, b.size, b.base, n), (BarKind::Io, 0x20, 0xC000, 1));
    // Unimplemented and reserved encodings.
    assert!(pci::decode_bar(&[0], &[0], 0).is_none());
    assert!(pci::decode_bar(&[0x6], &[0xFFFF_FFF6], 0).is_none());
    // A 64-bit BAR in the last slot has no upper half: rejected, not a panic.
    assert!(pci::decode_bar(&[0, 0, 0, 0, 0, 4], &[0, 0, 0, 0, 0, 0xFFFF_FFF4], 5).is_none());
}

#[test]
fn config_trait_on_short_image() {
    let c = [0u8; 8];
    assert_eq!(c.as_slice().read32(0x100), u32::MAX);
}

#[test]
fn efi_descriptors_and_policy() {
    use ksparse::efi::{self, MemoryDescriptor};
    let mut map = vec![0u8; 96];
    map[0..4].copy_from_slice(&efi::TYPE_RUNTIME_CODE.to_le_bytes());
    map[8..16].copy_from_slice(&0x1000u64.to_le_bytes());
    map[24..32].copy_from_slice(&4u64.to_le_bytes());
    map[32..40].copy_from_slice(&efi::ATTR_RUNTIME.to_le_bytes());
    map[48..52].copy_from_slice(&efi::TYPE_MMIO.to_le_bytes());
    map[56..64].copy_from_slice(&0xFFFF_FFFF_FFFF_F000u64.to_le_bytes()); // overflows when extended
    map[72..80].copy_from_slice(&16u64.to_le_bytes());
    let all: Vec<_> = efi::descriptors(&map, 48).collect();
    assert_eq!(all.len(), 2);
    assert!(all[0].1.is_sane() && all[0].1.is_runtime());
    assert_eq!(all[0].1.end(), 0x5000);
    assert!(
        !all[1].1.is_sane(),
        "a region that wraps the address space is rejected"
    );
    assert_eq!(
        efi::descriptors(&map, 8).count(),
        0,
        "descriptor size below the minimum yields nothing"
    );
    assert!(!MemoryDescriptor {
        kind: 0,
        phys: 0,
        pages: u64::MAX,
        attr: 0
    }
    .is_sane());
    assert!(!MemoryDescriptor {
        kind: 0,
        phys: 0x1001,
        pages: 1,
        attr: 0
    }
    .is_sane());
}

#[test]
fn efi_wx_policy_never_writable_and_executable() {
    use ksparse::efi::{self, ATTR_RO, ATTR_XP};
    for kind in [0u32, 5, 6, 11, 12, 99] {
        for attr in [
            0u64,
            ATTR_RO,
            ATTR_XP,
            ATTR_RO | ATTR_XP,
            efi::ATTR_RUNTIME,
            u64::MAX,
        ] {
            let (w, x) = efi::section_permissions(kind, attr);
            assert!(!(w && x), "kind {kind} attr {attr:#x} produced W+X");
        }
    }
    assert_eq!(efi::section_permissions(5, ATTR_RO), (false, true));
    assert_eq!(efi::section_permissions(6, ATTR_XP), (true, false));
}

#[test]
fn efi_config_tables_and_mat() {
    use ksparse::efi;
    assert_eq!(efi::config_tables_len(3), Some(72));
    assert_eq!(
        efi::config_tables_len(usize::MAX),
        None,
        "hostile counts cannot overflow the length"
    );
    let mut tables = vec![0u8; 48];
    tables[24..40].copy_from_slice(&efi::MAT_GUID);
    tables[40..48].copy_from_slice(&0xABCD_0000u64.to_le_bytes());
    assert_eq!(
        efi::find_config_table(&tables, &efi::MAT_GUID),
        Some(0xABCD_0000)
    );
    assert_eq!(efi::find_config_table(&tables[..30], &efi::MAT_GUID), None);
    // MAT with one valid and one wrapping entry.
    let mut mat = vec![0u8; 16 + 2 * 48];
    mat[4..8].copy_from_slice(&2u32.to_le_bytes());
    mat[8..12].copy_from_slice(&48u32.to_le_bytes());
    mat[16 + 8..16 + 16].copy_from_slice(&0x2000u64.to_le_bytes());
    mat[16 + 24..16 + 32].copy_from_slice(&1u64.to_le_bytes());
    mat[16 + 48 + 8..16 + 48 + 16].copy_from_slice(&u64::MAX.to_le_bytes());
    mat[16 + 48 + 24..16 + 48 + 32].copy_from_slice(&5u64.to_le_bytes());
    assert_eq!(efi::mat_total_len(&mat), Some(112));
    assert_eq!(efi::parse_mat(&mat).unwrap().len(), 1);
    assert_eq!(
        efi::parse_mat(&mat[..60]),
        None,
        "truncated MAT is rejected"
    );
    let mut hostile = mat.clone();
    hostile[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(efi::mat_total_len(&hostile), None);
}

#[test]
fn usb_device_and_config() {
    use ksparse::usb::{self, TransferKind};
    let mut dev = vec![0u8; 18];
    dev[0] = 18;
    dev[1] = 1;
    dev[2..4].copy_from_slice(&0x0200u16.to_le_bytes());
    dev[7] = 64;
    dev[8..10].copy_from_slice(&0x0e8du16.to_le_bytes());
    dev[10..12].copy_from_slice(&0x0608u16.to_le_bytes());
    dev[17] = 1;
    let d = usb::parse_device(&dev).unwrap();
    assert_eq!(
        (d.vendor, d.product, d.max_packet0, d.num_configs),
        (0x0e8d, 0x0608, 64, 1)
    );
    assert!(usb::parse_device(&dev[..17]).is_none());
    let cfg = usb::parse_config(&sample_usb_config()).unwrap();
    assert_eq!(cfg.value, 1);
    assert_eq!(cfg.interfaces.len(), 1);
    let i = &cfg.interfaces[0];
    assert_eq!(
        (i.class, i.subclass, i.protocol, i.hid_report_len),
        (3, 1, 1, Some(63))
    );
    let e = i.endpoints[0];
    assert_eq!(
        (
            e.number(),
            e.is_in(),
            e.kind(),
            e.max_packet,
            e.interval,
            e.dci()
        ),
        (1, true, TransferKind::Interrupt, 8, 10, 3)
    );
    // Truncations and a zero-length descriptor are survived.
    let full = sample_usb_config();
    for n in 0..=full.len() {
        let _ = usb::parse_config(&full[..n]);
    }
    let mut zero = full.clone();
    zero[9] = 0;
    assert!(usb::parse_config(&zero).is_some());
}

#[test]
fn usb_setup_packet_and_hub() {
    use ksparse::usb::{self, Setup};
    let s = Setup::get_descriptor(usb::DT_DEVICE, 0, 18);
    assert_eq!(s.to_u64(), 0x0012_0000_0100_0680);
    assert!(s.is_in());
    assert!(!Setup::set_configuration(1).is_in());
    assert_eq!(
        usb::parse_hub(&[9, 0x29, 4, 0x09, 0x00, 50, 100, 0, 0xFF])
            .unwrap()
            .ports,
        4
    );
    assert!(usb::parse_hub(&[9, 0x29, 0, 0, 0, 0, 0]).is_none());
    let st = usb::HubPortStatus::from_raw(0x0001_0103);
    assert!(st.connected && st.enabled && st.power && st.connect_changed && !st.low_speed);
}

#[test]
fn hid_boot_keyboard() {
    use ksparse::hid;
    let d = hid::parse(BOOT_KEYBOARD_DESC);
    assert!(hid::is_keyboard(&d) && !hid::is_pointer(&d));
    assert!(!d.uses_report_ids);
    assert_eq!(hid::input_report_len(&d, 0), 8);
    let r = hid::interpret(&d, &[0x02, 0x00, 0x13, 0x28, 0, 0, 0, 0]);
    assert_eq!(r.modifiers, 0x02);
    assert_eq!(r.keys, vec![0x13, 0x28]);
    let none = hid::interpret(&d, &[0; 8]);
    assert!(none.keys.is_empty() && none.modifiers == 0);
    // A short report is handled without panicking.
    let _ = hid::interpret(&d, &[0x01]);
}

#[test]
fn hid_boot_mouse() {
    use ksparse::hid;
    let d = hid::parse(BOOT_MOUSE_DESC);
    assert!(hid::is_pointer(&d) && !hid::is_keyboard(&d));
    let r = hid::interpret(&d, &[0x05, 0x05, 0xFB, 0x01]);
    assert_eq!(r.buttons, 0b101);
    assert_eq!((r.dx, r.dy, r.wheel), (5, -5, 1));
}

#[test]
fn hid_report_ids_and_extraction() {
    use ksparse::hid;
    // Two reports: id 1 with 16-bit X, id 2 with an 8-bit Y.
    let desc = [
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x16, 0x00, 0x80, 0x26, 0xFF,
        0x7F, 0x75, 0x10, 0x95, 0x01, 0x81, 0x06, 0x85, 0x02, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F,
        0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xC0,
    ];
    let d = hid::parse(&desc);
    assert!(d.uses_report_ids);
    assert_eq!(hid::interpret(&d, &[1, 0x00, 0xFF]).dx, -256);
    assert_eq!(hid::interpret(&d, &[2, 0xF0]).dy, -16);
    assert_eq!(
        hid::interpret(&d, &[3, 0xF0]).dy,
        0,
        "unknown report id is ignored"
    );
    assert_eq!(hid::extract(&[0b1010_0101], 0, 4, false), Some(5));
    assert_eq!(hid::extract(&[0b1010_0101], 4, 4, true), Some(-6));
    assert_eq!(
        hid::extract(&[0xFF], 4, 8, false),
        None,
        "reads past the end are refused"
    );
    assert_eq!(hid::extract(&[0xFF; 8], u32::MAX, 8, false), None);
    assert_eq!(hid::extract(&[0xFF], 0, 0, false), None);
}

#[test]
fn hid_hostile_descriptors_are_bounded() {
    use ksparse::hid;
    // Huge report size * count, deep push stacks, endless collections and long items.
    let mut hostile = vec![0x75, 0xFF, 0x96, 0xFF, 0xFF, 0x81, 0x02];
    hostile.extend(std::iter::repeat_n([0xA1u8, 0x01], 5000).flatten());
    hostile.extend(std::iter::repeat_n(0xA4u8, 5000));
    hostile.extend_from_slice(&[0xFE, 0xFF, 0x00]);
    let d = hid::parse(&hostile);
    assert!(d.fields.len() <= 512);
    let _ = hid::interpret(&d, &[0u8; 64]);
}

#[test]
fn usb_interval_and_packet_sizes() {
    use ksparse::usb::{self, SPEED_FULL, SPEED_HIGH, SPEED_LOW, SPEED_SUPER};
    assert_eq!(
        usb::interval_exponent(SPEED_FULL, 1),
        3,
        "1 ms = 8 microframes"
    );
    assert_eq!(
        usb::interval_exponent(SPEED_LOW, 10),
        6,
        "10 ms rounds down to 64 microframes"
    );
    assert_eq!(
        usb::interval_exponent(SPEED_FULL, 0),
        3,
        "zero is treated as 1 ms"
    );
    assert_eq!(
        usb::interval_exponent(SPEED_FULL, 255),
        10,
        "clamped to 128 ms"
    );
    assert_eq!(usb::interval_exponent(SPEED_HIGH, 4), 3);
    assert_eq!(
        usb::interval_exponent(SPEED_SUPER, 0),
        0,
        "out-of-range exponent is clamped"
    );
    assert_eq!(usb::interval_exponent(SPEED_SUPER, 255), 15);
    assert_eq!(usb::default_ep0_packet_size(SPEED_HIGH), 64);
    assert_eq!(usb::ep0_packet_size(SPEED_SUPER, 9), Some(512));
    assert_eq!(usb::ep0_packet_size(SPEED_FULL, 64), Some(64));
    assert_eq!(
        usb::ep0_packet_size(SPEED_FULL, 12),
        None,
        "illegal sizes are rejected"
    );
    assert_eq!(usb::ep0_packet_size(SPEED_SUPER, 8), None);
    let e = usb::Endpoint {
        address: 0x81,
        attributes: 3,
        max_packet: 0x1000 | 0x40,
        interval: 1,
        max_burst: 0,
    };
    assert_eq!((e.packet_size(), e.extra_transactions()), (0x40, 2));
}
