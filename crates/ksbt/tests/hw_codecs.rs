//! USB packet framing and the (hardware-unverified) MediaTek codec, against synthetic data.

use ksbt::framing::{Assembler, Kind};
use ksbt::mtk::*;

fn patch_file(sections: &[(u32, u32)], payload_len: usize) -> Vec<u8> {
    let mut f = vec![0u8; 32 + 64 + 64 * sections.len()];
    f[..14].copy_from_slice(b"20240219111427");
    f[20..22].copy_from_slice(&0x008Au16.to_le_bytes());
    f[22..24].copy_from_slice(&0x008Au16.to_le_bytes());
    f[32 + 12..32 + 16].copy_from_slice(&(sections.len() as u32).to_le_bytes());
    let data_start = f.len();
    f.extend((0..payload_len).map(|i| i as u8));
    for (i, &(offset, size)) in sections.iter().enumerate() {
        let e = 32 + 64 + 64 * i;
        f[e + 4..e + 8].copy_from_slice(&(data_start as u32 + offset).to_le_bytes());
        f[e + 12..e + 16].copy_from_slice(&0x0090_0000u32.to_le_bytes()); // dlAddr
        f[e + 16..e + 20].copy_from_slice(&size.to_le_bytes()); // dlsize
    }
    f
}

#[test]
fn event_stream_reassembly() {
    let mut a = Assembler::new(Kind::Event);
    // One 12-byte event split across three 5-byte transfers, then two events in one transfer.
    let ev: Vec<u8> = [vec![0x3E, 10], (0..10).collect::<Vec<u8>>()].concat();
    assert!(a.push(&ev[..5]).is_empty());
    assert!(a.push(&ev[5..10]).is_empty());
    assert_eq!(a.push(&ev[10..]), vec![ev.clone()]);
    let two = [ev.clone(), vec![0x0E, 1, 0xAA]].concat();
    assert_eq!(a.push(&two), vec![ev, vec![0x0E, 1, 0xAA]]);
    assert_eq!(a.pending(), 0);
}

#[test]
fn acl_stream_reassembly_and_resync() {
    let mut a = Assembler::new(Kind::Acl);
    let pkt: Vec<u8> = [vec![0x40, 0x00, 0x05, 0x00], vec![1, 2, 3, 4, 5]].concat();
    assert!(
        a.push(&pkt[..3]).is_empty(),
        "header split across transfers"
    );
    assert_eq!(a.push(&pkt[3..]), vec![pkt.clone()]);
    // An absurd announced length makes the assembler drop its buffer rather than wait forever.
    assert!(a.push(&[0x40, 0x00, 0xFF, 0xFF, 1, 2]).is_empty());
    assert_eq!(a.pending(), 0);
    assert_eq!(a.push(&pkt), vec![pkt]);
}

#[test]
fn patch_file_parses_sections_and_version() {
    let f = patch_file(&[(0, 600), (600, 100)], 800);
    let p = parse_patch(&f).unwrap();
    assert_eq!(p.build_time, "20240219111427");
    assert_eq!(
        p.version_word(),
        0x008A_008A,
        "formats like the Linux log: HW/SW Version: 0x008a008a"
    );
    assert_eq!(p.sections.len(), 2);
    assert_eq!((p.sections[0].data_len, p.sections[1].data_len), (600, 100));
    assert_eq!(p.sections[0].announce[0], 0, "legacy download mode byte");
    assert_eq!(
        &p.sections[0].announce[5..9],
        &600u32.to_le_bytes(),
        "dlsize is part of the announced map data"
    );
}

#[test]
fn patch_file_rejects_hostile_input() {
    assert_eq!(parse_patch(&[0; 10]), Err(PatchError::TooShort));
    let mut f = patch_file(&[(0, 100)], 100);
    f[32 + 12..32 + 16].copy_from_slice(&1000u32.to_le_bytes());
    assert_eq!(parse_patch(&f), Err(PatchError::BadSectionCount));
    let mut f = patch_file(&[(0, 100)], 100);
    f[32 + 64 + 16..32 + 64 + 20].copy_from_slice(&u32::MAX.to_le_bytes()); // dlsize far beyond the file
    assert_eq!(parse_patch(&f), Err(PatchError::SectionOutOfRange));
    let mut f = patch_file(&[(0, 100)], 100);
    f[32 + 64 + 4..32 + 64 + 8].copy_from_slice(&u32::MAX.to_le_bytes()); // offset overflow
    assert_eq!(parse_patch(&f), Err(PatchError::SectionOutOfRange));
    // Every truncation parses or errors, never panics.
    let f = patch_file(&[(0, 600), (600, 100)], 800);
    for n in 0..f.len() {
        let _ = parse_patch(&f[..n]);
    }
}

#[test]
fn download_chunking_matches_the_documented_flags() {
    let data: Vec<u8> = (0..600u32).map(|i| i as u8).collect();
    let c = chunks(&data);
    assert_eq!(
        c.iter().map(|(f, d)| (*f, d.len())).collect::<Vec<_>>(),
        vec![(1, 250), (2, 250), (3, 100)]
    );
    assert_eq!(c.iter().map(|(_, d)| d.len()).sum::<usize>(), 600);
    assert_eq!(
        chunks(&data[..100])
            .iter()
            .map(|(f, _)| *f)
            .collect::<Vec<_>>(),
        vec![1],
        "a single chunk is only ever 'first'"
    );
    assert_eq!(
        chunks(&data[..500])
            .iter()
            .map(|(f, _)| *f)
            .collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert!(chunks(&[]).is_empty());
}

#[test]
fn wmt_command_and_event_codec() {
    let c = wmt_command(wmt_op::FUNC_CTRL, 0, &[1]);
    assert_eq!(c.opcode, 0xFC6F);
    assert_eq!(c.params, vec![0x01, 0x06, 0x02, 0x00, 0x00, 0x01]);
    // Response: event 0xE4, dir=2, op=6, dlen=4, flag=0, status 0x0001 (big-endian).
    let e = parse_wmt_event(&[0xE4, 7, 0x02, 0x06, 0x04, 0x00, 0x00, 0x00, 0x01]).unwrap();
    assert_eq!((e.op, e.flag, e.status), (6, 0, Some(1)));
    let done = parse_wmt_event(&[0xE4, 5, 0x02, 0x01, 0x01, 0x00, 0x02]).unwrap();
    assert_eq!(patch_state(&done), PatchState::Done);
    assert_eq!(
        patch_state(&WmtEvent {
            op: 1,
            flag: 1,
            status: None
        }),
        PatchState::Progress
    );
    assert_eq!(
        patch_state(&WmtEvent {
            op: 1,
            flag: 0,
            status: None
        }),
        PatchState::Undone
    );
    assert!(parse_wmt_event(&[0x0E, 1, 0]).is_none());
    assert!(
        parse_wmt_event(&[0xE4, 200, 1]).is_none(),
        "declared length beyond the packet"
    );
}

#[test]
fn firmware_file_names_match_what_the_owner_has() {
    // The owner's linux-firmware has BT_RAM_CODE_MT7961_1_2_hdr.bin and ..._1a_2_hdr.bin.
    assert_eq!(
        firmware_name(0x7961, 0x1, false),
        "BT_RAM_CODE_MT7961_1_2_hdr.bin"
    );
    assert_eq!(
        firmware_name(0x7961, 0x1, true),
        "BT_RAM_CODE_MT7961_1a_2_hdr.bin"
    );
    assert_eq!(
        firmware_name(0x1234_7961, 0x8A01, false),
        "BT_RAM_CODE_MT7961_1_2_hdr.bin",
        "only the low 16 bits identify the chip"
    );
    assert_eq!(
        firmware_name(0x7922, 0x1, true),
        "BT_RAM_CODE_MT7922_1_2_hdr.bin",
        "the 'a' flavour applies to MT7961 only"
    );
}

#[test]
fn hostile_input_never_panics() {
    // Deterministic mutation/noise run through the same entry point the fuzzer uses.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let seeds: Vec<Vec<u8>> = vec![
        patch_file(&[(0, 600), (600, 100)], 800),
        vec![0x3E, 12, 0x02, 1, 0, 0, 1, 2, 3, 4, 5, 6, 3, 2, 1, 6, 0xC0],
        vec![
            0x40, 0x00, 0x07, 0x00, 0x03, 0x00, 0x04, 0x00, 0x0A, 0x01, 0x00,
        ],
    ];
    for round in 0..20_000 {
        let mut v = seeds[round % seeds.len()].clone();
        for _ in 0..(1 + next() % 5) {
            match next() % 3 {
                0 if !v.is_empty() => {
                    let i = (next() as usize) % v.len();
                    v[i] = next() as u8;
                }
                1 => v.truncate((next() as usize) % (v.len() + 1)),
                _ => v.extend((0..(next() % 40)).map(|_| next() as u8)),
            }
        }
        ksbt::harness::exercise(&v);
    }
    for n in 0..300 {
        let noise: Vec<u8> = (0..n).map(|_| next() as u8).collect();
        ksbt::harness::exercise(&noise);
    }
}
