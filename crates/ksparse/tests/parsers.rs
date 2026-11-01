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
