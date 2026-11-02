// SPDX-License-Identifier: Apache-2.0
//! I/O page tables and VT-d / AMD-Vi entry encoding against a simulated table memory.
use ksiommu::{amd, intel, Domain, Error, Format, TableMem, PAGE};
use std::collections::HashMap;

#[derive(Default)]
struct Arena {
    tables: HashMap<u64, Vec<u64>>,
    next: u64,
    live: usize,
    limit: usize,
}

impl Arena {
    fn new(limit: usize) -> Self {
        Self {
            next: 0x10_0000,
            limit,
            ..Self::default()
        }
    }
}

impl TableMem for Arena {
    fn alloc_table(&mut self) -> Option<u64> {
        if self.live >= self.limit {
            return None;
        }
        let p = self.next;
        self.next += PAGE;
        self.tables.insert(p, vec![0; 512]);
        self.live += 1;
        Some(p)
    }
    fn free_table(&mut self, phys: u64) {
        assert!(
            self.tables.remove(&phys).is_some(),
            "double free of a table page"
        );
        self.live -= 1;
    }
    fn read(&self, table: u64, index: usize) -> u64 {
        self.tables[&table][index]
    }
    fn write(&mut self, table: u64, index: usize, value: u64) {
        self.tables.get_mut(&table).unwrap()[index] = value;
    }
}

fn formats() -> [(Format, u8); 4] {
    [
        (Format::IntelSecondLevel, 3),
        (Format::IntelSecondLevel, 4),
        (Format::AmdV1, 3),
        (Format::AmdV1, 4),
    ]
}

#[test]
fn map_translate_unmap_in_every_format_and_depth() {
    for (fmt, levels) in formats() {
        let mut m = Arena::new(1000);
        let mut d = Domain::new(&mut m, fmt, levels).unwrap();
        d.map(&mut m, 0x20_0000, 0x7000_0000, 3 * PAGE, true)
            .unwrap();
        d.map(&mut m, 0x4000_0000, 0x1234_5000, PAGE, false)
            .unwrap();
        for k in 0..3 {
            let (pa, w) = d.translate(&m, 0x20_0000 + k * PAGE + 0x123).unwrap();
            assert_eq!(pa, 0x7000_0000 + k * PAGE + 0x123, "{fmt:?}/{levels}");
            assert!(w);
        }
        assert_eq!(
            d.translate(&m, 0x4000_0abc),
            Some((0x1234_5abc, false)),
            "read-only mapping"
        );
        assert_eq!(
            d.translate(&m, 0x20_0000 + 3 * PAGE),
            None,
            "just past the mapping"
        );
        assert_eq!(d.translate(&m, 0x1000), None, "deny by default");
        d.unmap(&mut m, 0x20_1000, PAGE);
        assert!(d.translate(&m, 0x20_0000).is_some());
        assert_eq!(
            d.translate(&m, 0x20_1000),
            None,
            "unmapped page is blocked again"
        );
        assert!(d.translate(&m, 0x20_2000).is_some());
        d.destroy(&mut m);
        assert_eq!(m.live, 0, "{fmt:?}/{levels}: all table pages are freed");
    }
}

#[test]
fn argument_validation_and_overlap() {
    let mut m = Arena::new(1000);
    let mut d = Domain::new(&mut m, Format::IntelSecondLevel, 3).unwrap();
    assert_eq!(d.map(&mut m, 0x1001, 0x2000, PAGE, true), Err(Error::Range));
    assert_eq!(d.map(&mut m, 0x1000, 0x2001, PAGE, true), Err(Error::Range));
    assert_eq!(d.map(&mut m, 0x1000, 0x2000, 0, true), Err(Error::Range));
    assert_eq!(d.map(&mut m, 0x1000, 0x2000, 100, true), Err(Error::Range));
    assert_eq!(
        d.map(&mut m, u64::MAX - PAGE + 1, 0x2000, PAGE, true),
        Err(Error::Range)
    );
    assert_eq!(
        d.map(&mut m, d.limit(), 0x2000, PAGE, true),
        Err(Error::Range),
        "beyond 39 bits"
    );
    d.map(&mut m, 0x1000, 0x2000, PAGE, true).unwrap();
    assert_eq!(
        d.map(&mut m, 0x1000, 0x9000, PAGE, true),
        Err(Error::Overlap)
    );
    assert_eq!(
        d.translate(&m, 0x1000),
        Some((0x2000, true)),
        "the original mapping survived"
    );
    assert!(Domain::new(&mut m, Format::AmdV1, 2).is_err());
}

#[test]
fn out_of_memory_is_reported_not_fatal() {
    let mut m = Arena::new(3); // root + two more tables
    let mut d = Domain::new(&mut m, Format::IntelSecondLevel, 4).unwrap();
    assert_eq!(
        d.map(&mut m, 0x1000, 0x2000, PAGE, true),
        Err(Error::OutOfMemory)
    );
}

#[test]
fn entry_encodings_match_the_specifications() {
    // VT-d: root entry present bit, context entry T=0, AW and DID fields.
    assert_eq!(intel::root_entry(0x1234_5000), [0x1234_5001, 0]);
    assert_eq!(
        intel::context_entry(0xABC_D000, 3, 7),
        Some([0xABC_D001, 1 | (7 << 8)])
    );
    assert_eq!(
        intel::context_entry(0xABC_D000, 4, 0xFFFF),
        Some([0xABC_D001, 2 | (0xFFFF << 8)])
    );
    assert_eq!(intel::context_entry(0x1000, 5, 1), None);
    // AMD-Vi: V, TV, mode, root pointer, IR/IW, domain id.
    let e = amd::dte(0x5555_0000, 4, 9);
    assert_eq!(e[0] & 3, 3);
    assert_eq!((e[0] >> 9) & 7, 4);
    assert_eq!(e[0] & 0x000F_FFFF_FFFF_F000, 0x5555_0000);
    assert!(e[0] & (1 << 61) != 0 && e[0] & (1 << 62) != 0);
    assert_eq!(e[1], 9);
    assert_eq!(amd::dte_offset(0x0310), 0x0310 * 32);
    assert_eq!(amd::DEVICE_TABLE_BYTES, 2 * 1024 * 1024);
}

#[test]
fn vtd_root_and_context_tables_isolate_functions() {
    let mut m = Arena::new(1000);
    let t = intel::Tables::new(&mut m).unwrap();
    let mut a = Domain::new(&mut m, Format::IntelSecondLevel, 4).unwrap();
    let mut b = Domain::new(&mut m, Format::IntelSecondLevel, 3).unwrap();
    a.map(&mut m, 0x1000, 0x8000, PAGE, true).unwrap();
    b.map(&mut m, 0x1000, 0x9000, PAGE, true).unwrap();
    t.attach(&mut m, 0, 0x10, &a, 1).unwrap(); // 00:02.0
    t.attach(&mut m, 3, 0x00, &b, 2).unwrap(); // 03:00.0
    assert_eq!(t.lookup(&mut m, 0, 0x10), Some((a.root, 4, 1)));
    assert_eq!(t.lookup(&mut m, 3, 0x00), Some((b.root, 3, 2)));
    assert_eq!(
        t.lookup(&mut m, 0, 0x11),
        None,
        "a neighbour function has no context entry: blocked"
    );
    assert_eq!(
        t.lookup(&mut m, 7, 0x00),
        None,
        "a bus without devices: blocked"
    );
    // The same IOVA reaches different memory depending on the function (isolation).
    assert_eq!(a.translate(&m, 0x1000), Some((0x8000, true)));
    assert_eq!(b.translate(&m, 0x1000), Some((0x9000, true)));
    t.detach(&mut m, 0, 0x10);
    assert_eq!(t.lookup(&mut m, 0, 0x10), None);
    let amd_dom = Domain::new(&mut m, Format::AmdV1, 3).unwrap();
    assert_eq!(
        t.attach(&mut m, 1, 0, &amd_dom, 3),
        Err(Error::Range),
        "format mismatch is refused"
    );
}
