// SPDX-License-Identifier: Apache-2.0
//! DMAR (Intel VT-d) and IVRS (AMD-Vi) table parsing against hand-built tables.
use ksparse::iommu::{parse_dmar, parse_ivrs, IommuTableError};

fn header(sig: &[u8; 4], body_after_36: &[u8]) -> Vec<u8> {
    let mut t = sig.to_vec();
    t.extend_from_slice(&((36 + body_after_36.len()) as u32).to_le_bytes());
    t.resize(36, 0);
    t.extend_from_slice(body_after_36);
    t
}

fn scope(kind: u8, id: u8, bus: u8, path: &[(u8, u8)]) -> Vec<u8> {
    let mut s = vec![kind, (6 + 2 * path.len()) as u8, 0, 0, id, bus];
    for (d, f) in path {
        s.extend_from_slice(&[*d, *f]);
    }
    s
}

fn drhd(flags: u8, seg: u16, base: u64, scopes: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = scopes.concat();
    let mut e = vec![0, 0];
    e.extend_from_slice(&((16 + body.len()) as u16).to_le_bytes());
    e.extend_from_slice(&[flags, 0]);
    e.extend_from_slice(&seg.to_le_bytes());
    e.extend_from_slice(&base.to_le_bytes());
    e.extend_from_slice(&body);
    e
}

fn rmrr(seg: u16, base: u64, limit: u64, scopes: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = scopes.concat();
    let mut e = vec![1, 0];
    e.extend_from_slice(&((24 + body.len()) as u16).to_le_bytes());
    e.extend_from_slice(&[0, 0]);
    e.extend_from_slice(&seg.to_le_bytes());
    e.extend_from_slice(&base.to_le_bytes());
    e.extend_from_slice(&limit.to_le_bytes());
    e.extend_from_slice(&body);
    e
}

#[test]
fn dmar_units_scopes_and_reserved_regions() {
    let mut body = vec![38u8, 0x01]; // host address width 39, flags: interrupt remapping
    body.extend_from_slice(&[0; 10]);
    body.extend(drhd(0, 0, 0xFED9_0000, &[scope(1, 0, 0, &[(2, 0)])]));
    body.extend(drhd(1, 0, 0xFED9_1000, &[]));
    body.extend(rmrr(
        0,
        0xDF00_0000,
        0xDF0F_FFFF,
        &[scope(1, 0, 0, &[(0x14, 0)])],
    ));
    let d = parse_dmar(&header(b"DMAR", &body)).unwrap();
    assert_eq!(d.host_address_width, 39);
    assert_eq!(d.units.len(), 2);
    assert_eq!(d.units[0].register_base, 0xFED9_0000);
    assert!(!d.units[0].include_all && d.units[1].include_all);
    assert_eq!(d.units[0].scopes[0].path, vec![(2, 0)]);
    assert_eq!(d.rmrr[0].limit, 0xDF0F_FFFF);
    assert_eq!(d.rmrr[0].scopes[0].path, vec![(0x14, 0)]);
}

#[test]
fn dmar_rejects_bad_input_and_survives_truncation() {
    assert_eq!(parse_dmar(&[0; 10]), Err(IommuTableError::TooShort));
    assert_eq!(
        parse_dmar(&header(b"XXXX", &[0; 20])),
        Err(IommuTableError::BadSignature)
    );
    let mut body = vec![38u8, 0];
    body.extend_from_slice(&[0; 10]);
    body.extend(drhd(1, 0, 0xFED9_0000, &[scope(1, 0, 0, &[(2, 0)])]));
    let full = header(b"DMAR", &body);
    for cut in 48..full.len() {
        let _ = parse_dmar(full.get(..cut).unwrap());
    }
    // A structure claiming a huge length must not run off the table.
    let mut evil = header(b"DMAR", &{
        let mut b = vec![38u8, 0];
        b.extend_from_slice(&[0; 10]);
        b.extend_from_slice(&[0, 0, 0xFF, 0xFF, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        b
    });
    evil.extend_from_slice(&[0xFF; 8]);
    assert!(parse_dmar(&evil).unwrap().units.is_empty());
}

fn ivhd(kind: u8, device_id: u16, base: u64, seg: u16, entries: &[u8]) -> Vec<u8> {
    let head = if kind == 0x10 { 24 } else { 40 };
    let mut e = vec![kind, 0];
    e.extend_from_slice(&((head + entries.len()) as u16).to_le_bytes());
    e.extend_from_slice(&device_id.to_le_bytes());
    e.extend_from_slice(&[0x40, 0]);
    e.extend_from_slice(&base.to_le_bytes());
    e.extend_from_slice(&seg.to_le_bytes());
    e.resize(head, 0);
    e.extend_from_slice(entries);
    e
}

#[test]
fn ivrs_units_ranges_and_unity_maps() {
    let mut entries = vec![];
    entries.extend_from_slice(&[0x02, 0x10, 0x00, 0x00]); // select 00:02.0
    entries.extend_from_slice(&[0x03, 0x00, 0x08, 0x00]); // range start 08:00
    entries.extend_from_slice(&[0x04, 0xFF, 0x0B, 0x00]); // range end   0b:ff
    entries.extend_from_slice(&[0x42, 0x20, 0x00, 0x00, 0, 0x28, 0, 0]); // alias select (8 bytes)
    let mut body = vec![0u8; 12];
    body.extend(ivhd(0x11, 0x0008, 0xFEB8_0000, 0, &entries));
    let mut ivmd = vec![0x21, 0, 32, 0, 0x30, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    ivmd.extend_from_slice(&0xBB00_0000u64.to_le_bytes());
    ivmd.extend_from_slice(&0x10000u64.to_le_bytes());
    body.extend(ivmd);
    let v = parse_ivrs(&header(b"IVRS", &body)).unwrap();
    assert_eq!(v.units.len(), 1);
    assert_eq!(v.units[0].register_base, 0xFEB8_0000);
    assert_eq!(
        v.units[0].ranges,
        vec![(0x10, 0x10), (0x0800, 0x0BFF), (0x20, 0x20)]
    );
    assert_eq!(v.unity[0].device_start, 0x30);
    assert_eq!(
        (v.unity[0].start, v.unity[0].length),
        (0xBB00_0000, 0x10000)
    );
}

#[test]
fn ivrs_survives_garbage() {
    assert_eq!(parse_ivrs(&[0; 3]), Err(IommuTableError::TooShort));
    let mut x = 0x1234_5678_9abc_def0u64;
    for round in 0..300usize {
        let mut t = header(b"IVRS", &[0; 12]);
        for _ in 0..round {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            t.push((x >> 8) as u8);
        }
        let _ = parse_ivrs(&t);
        let mut t = header(b"DMAR", &[0; 12]);
        t.extend_from_slice(&[0u8; 0]);
        for _ in 0..round {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            t.push((x >> 8) as u8);
        }
        let _ = parse_dmar(&t);
    }
}
