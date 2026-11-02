// SPDX-License-Identifier: Apache-2.0
//! Entry points shared by the host robustness tests and the fuzz targets.

use crate::pci::{self, Config};
use crate::{efi, hid, madt, mcfg, sdt, usb};

/// Drives every parser with the same arbitrary bytes. Used by the robustness tests and the
/// libFuzzer targets; the contract is simply "must return".
pub fn exercise(bytes: &[u8]) {
    let _ = crate::iommu::parse_dmar(bytes);
    let _ = crate::iommu::parse_ivrs(bytes);
    // Reach the table bodies directly: force valid signatures and a minimum length onto the input.
    for sig in [b"DMAR", b"IVRS"] {
        let mut t = bytes.to_vec();
        t.resize(t.len().max(48), 0);
        t.splice(0..4, sig.iter().copied());
        let _ = crate::iommu::parse_dmar(&t);
        let _ = crate::iommu::parse_ivrs(&t);
    }
    let _ = madt::parse(bytes);
    let _ = mcfg::parse(bytes);
    let _ = sdt::validate_table(bytes);
    let _ = sdt::Rsdp::parse(bytes);
    let _ = sdt::Fadt::parse(bytes);
    let _ = sdt::find_s5(bytes);
    let _ = sdt::root_entries(bytes, true).count();
    let _ = sdt::root_entries(bytes, false).count();
    if let Some(h) = pci::header(bytes) {
        let caps = pci::capabilities(bytes, &h);
        for c in caps {
            if let Some(x) = pci::msix(bytes, c) {
                let _ = x.table_range();
            }
            let _ = pci::msi(bytes, c);
        }
    }
    for sz in [0usize, 39, 40, 48, 5000] {
        for (raw, d) in efi::descriptors(bytes, sz) {
            let _ = (raw.len(), d.is_sane(), d.is_runtime());
            if d.is_sane() {
                let _ = d.end();
            }
            let _ = efi::section_permissions(d.kind, d.attr);
        }
    }
    let _ = efi::mat_total_len(bytes);
    let _ = efi::parse_mat(bytes);
    let _ = efi::find_config_table(bytes, &efi::MAT_GUID);
    let _ = usb::parse_device(bytes);
    let _ = usb::parse_hub(bytes);
    if let Some(cfg) = usb::parse_config(bytes) {
        for i in &cfg.interfaces {
            for e in &i.endpoints {
                let _ = (e.dci(), e.kind(), e.number(), e.is_in());
            }
        }
    }
    let desc = hid::parse(bytes);
    let _ = (hid::is_keyboard(&desc), hid::is_pointer(&desc));
    for id in [0u8, 1, 0xFF] {
        let _ = hid::input_report_len(&desc, id);
    }
    // Reuse the same bytes as a report against the parsed descriptor and a known-good one.
    let _ = hid::interpret(&desc, bytes);
    let _ = hid::extract(
        bytes,
        bytes.first().copied().unwrap_or(0).into(),
        bytes.get(1).copied().unwrap_or(8),
        true,
    );
    let words: alloc::vec::Vec<u32> = (0..12u16)
        .map(|i| Config::read32(bytes, i.saturating_mul(4)))
        .collect();
    let (orig, probed) = words.split_at(6);
    for i in 0..6 {
        let _ = pci::decode_bar(orig, probed, i);
    }
}
