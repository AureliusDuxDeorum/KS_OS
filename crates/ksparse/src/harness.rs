//! Entry points shared by the host robustness tests and the fuzz targets.

use crate::pci::{self, Config};
use crate::{efi, madt, mcfg, sdt};

/// Drives every parser with the same arbitrary bytes. Used by the robustness tests and the
/// libFuzzer targets; the contract is simply "must return".
pub fn exercise(bytes: &[u8]) {
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
    let words: alloc::vec::Vec<u32> = (0..12).map(|i| Config::read32(bytes, (i * 4) as u16)).collect();
    for i in 0..6 {
        let _ = pci::decode_bar(&words[..6], &words[6..], i);
    }
}
