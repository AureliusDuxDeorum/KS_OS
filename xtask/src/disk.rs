// SPDX-License-Identifier: Apache-2.0
//! Disk image construction: a FAT ESP wrapped in a GPT, built byte-for-byte reproducibly.

use crate::firmware::Blob;
use crate::limine::Limine;
use crate::Result;
use std::io::{Cursor, Write};

const SECTOR: usize = 512;
const ESP_MIB: usize = 32;
const FIRST_LBA: u64 = 2048;

/// Renders `limine.conf`; `cmdline` is passed to the kernel (empty for production images).
fn limine_conf(cmdline: &str, modules: &[Blob]) -> String {
    let mut conf = String::from(
        "timeout: 0\neditor_enabled: no\nserial: yes\n\n/KS_OS\n    protocol: limine\n    path: boot():/boot/kernel\n",
    );
    for m in modules {
        conf.push_str(&format!("    module_path: boot():/boot/fw/{}\n", m.name));
    }
    if !cmdline.is_empty() {
        conf.push_str(&format!("    cmdline: {cmdline}\n"));
    }
    conf
}

/// Builds the EFI System Partition contents.
pub fn make_esp(
    limine: &Limine,
    kernel: &[u8],
    cmdline: &str,
    modules: &[Blob],
) -> Result<Vec<u8>> {
    let mut storage = Cursor::new(vec![0u8; ESP_MIB * 1024 * 1024]);
    fatfs::format_volume(
        &mut storage,
        fatfs::FormatVolumeOptions::new().volume_label(*b"KS_OS ESP  "),
    )?;
    {
        let fs = fatfs::FileSystem::new(&mut storage, fatfs::FsOptions::new())?;
        let root = fs.root_dir();
        root.create_dir("EFI")?.create_dir("BOOT")?;
        root.create_dir("boot")?;
        let write = |path: &str, data: &[u8]| -> Result<()> {
            let mut f = root.create_file(path)?;
            f.write_all(data)?;
            f.flush()?;
            Ok(())
        };
        write("EFI/BOOT/BOOTX64.EFI", &limine.bootx64)?;
        root.create_dir("boot/fw")?;
        for m in modules {
            write(&format!("boot/fw/{}", m.name), &m.data)?;
        }
        write("limine.conf", limine_conf(cmdline, modules).as_bytes())?;
        write("boot/kernel", kernel)?;
    }
    Ok(storage.into_inner())
}

fn guid_bytes(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&d1.to_le_bytes());
    g[4..6].copy_from_slice(&d2.to_le_bytes());
    g[6..8].copy_from_slice(&d3.to_le_bytes());
    g[8..16].copy_from_slice(&d4);
    g
}

fn gpt_header(
    current: u64,
    backup: u64,
    first_usable: u64,
    last_usable: u64,
    disk_guid: [u8; 16],
    entries_lba: u64,
    entries_crc: u32,
) -> [u8; SECTOR] {
    let mut h = [0u8; SECTOR];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&current.to_le_bytes());
    h[32..40].copy_from_slice(&backup.to_le_bytes());
    h[40..48].copy_from_slice(&first_usable.to_le_bytes());
    h[48..56].copy_from_slice(&last_usable.to_le_bytes());
    h[56..72].copy_from_slice(&disk_guid);
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[80..84].copy_from_slice(&128u32.to_le_bytes());
    h[84..88].copy_from_slice(&128u32.to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    let crc = crc32fast::hash(&h[0..92]);
    h[16..20].copy_from_slice(&crc.to_le_bytes());
    h
}

/// Wraps an ESP image into a GPT-partitioned disk with protective MBR and backup table.
pub fn wrap_gpt(esp: &[u8]) -> Vec<u8> {
    let part_sectors = (esp.len() / SECTOR) as u64;
    let last_part_lba = FIRST_LBA + part_sectors - 1;
    // 32 sectors of backup entries + 1 backup header, plus alignment slack.
    let total = (last_part_lba + 1 + 2048 + 33).next_multiple_of(2048);
    let last_lba = total - 1;
    let last_usable = last_lba - 33;
    let backup_entries_lba = last_lba - 32;

    let mut disk = vec![0u8; total as usize * SECTOR];

    // Protective MBR.
    let mbr = &mut disk[0..SECTOR];
    mbr[446 + 1..446 + 4].copy_from_slice(&[0x00, 0x02, 0x00]);
    mbr[446 + 4] = 0xEE;
    mbr[446 + 5..446 + 8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr[446 + 12..446 + 16]
        .copy_from_slice(&(((total - 1).min(u64::from(u32::MAX))) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;

    // Partition entry 0: EFI System Partition.
    let mut entries = vec![0u8; 128 * 128];
    let esp_type = guid_bytes(
        0xC12A_7328,
        0xF81F,
        0x11D2,
        [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
    );
    let part_guid = guid_bytes(
        0x4B53_4F53,
        0x0001,
        0x4000,
        [0x80, 0x00, b'K', b'S', b'_', b'O', b'S', b'1'],
    );
    let disk_guid = guid_bytes(
        0x4B53_4F53,
        0x0000,
        0x4000,
        [0x80, 0x00, b'K', b'S', b'_', b'O', b'S', b'0'],
    );
    entries[0..16].copy_from_slice(&esp_type);
    entries[16..32].copy_from_slice(&part_guid);
    entries[32..40].copy_from_slice(&FIRST_LBA.to_le_bytes());
    entries[40..48].copy_from_slice(&last_part_lba.to_le_bytes());
    for (i, c) in "EFI System".encode_utf16().enumerate() {
        entries[56 + i * 2..58 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
    let entries_crc = crc32fast::hash(&entries);

    let primary = gpt_header(1, last_lba, 34, last_usable, disk_guid, 2, entries_crc);
    let backup = gpt_header(
        last_lba,
        1,
        34,
        last_usable,
        disk_guid,
        backup_entries_lba,
        entries_crc,
    );

    disk[SECTOR..2 * SECTOR].copy_from_slice(&primary);
    disk[2 * SECTOR..2 * SECTOR + entries.len()].copy_from_slice(&entries);
    let be = backup_entries_lba as usize * SECTOR;
    disk[be..be + entries.len()].copy_from_slice(&entries);
    let bh = last_lba as usize * SECTOR;
    disk[bh..bh + SECTOR].copy_from_slice(&backup);
    let ps = FIRST_LBA as usize * SECTOR;
    disk[ps..ps + esp.len()].copy_from_slice(esp);
    disk
}
