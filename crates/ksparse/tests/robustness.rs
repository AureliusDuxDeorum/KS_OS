//! Randomised and exhaustive-mutation robustness tests: no input may panic or hang.
//!
//! These run on stable in `cargo test`; the same entry points are driven by the
//! libFuzzer targets in `fuzz/` for coverage-guided runs.

mod support;
use support::*;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..(1 + rng.next() % 6) {
        match rng.next() % 5 {
            0 if !v.is_empty() => {
                let i = (rng.next() as usize) % v.len();
                v[i] = rng.next() as u8;
            }
            1 if !v.is_empty() => {
                let i = (rng.next() as usize) % v.len();
                v[i] ^= 1 << (rng.next() % 8);
            }
            2 => v.truncate((rng.next() as usize) % (v.len() + 1)),
            3 => {
                let extra = (rng.next() % 64) as usize;
                v.extend((0..extra).map(|_| rng.next() as u8));
            }
            _ => {
                if v.len() >= 4 {
                    let i = (rng.next() as usize) % (v.len() - 3);
                    v[i..i + 4].copy_from_slice(&(rng.next() as u32).to_le_bytes());
                }
            }
        }
    }
    v
}

fn run_all(bytes: &[u8]) {
    ksparse::harness::exercise(bytes);
}

#[test]
fn random_mutations_never_panic() {
    let seeds = [
        sample_madt(),
        sample_mcfg(),
        sample_dsdt(),
        sample_fadt(),
        sample_rsdp(2),
        sample_config_space(),
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for round in 0..60_000 {
        let seed = &seeds[round % seeds.len()];
        run_all(&mutate(&mut rng, seed));
    }
}

#[test]
fn every_byte_value_at_every_offset() {
    for seed in [
        sample_madt(),
        sample_mcfg(),
        sample_dsdt(),
        sample_fadt(),
        sample_rsdp(2),
    ] {
        for off in 0..seed.len() {
            for v in [0u8, 1, 2, 3, 0x7F, 0x80, 0xFE, 0xFF] {
                let mut m = seed.clone();
                m[off] = v;
                run_all(&m);
            }
        }
    }
}

#[test]
fn pure_noise_never_panics() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..20_000 {
        let n = (rng.next() % 300) as usize;
        let noise: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        run_all(&noise);
    }
}

#[test]
fn adversarial_pci_config_space() {
    // All-ones (a dead device), all-zero, and a capability list of maximum-length cycles.
    for fill in [0x00u8, 0xFF, 0x11, 0x05] {
        run_all(&vec![fill; 4096]);
    }
    let mut c = sample_config_space();
    for off in (0x40..0x100).step_by(4) {
        c[off] = 0x05;
        c[off + 1] = ((off + 4) & 0xFC) as u8;
    }
    run_all(&c);
}
