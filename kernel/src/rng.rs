//! Hardware random number generator (RDSEED, falling back to RDRAND).
//!
//! Cryptographic keys must never come from a weak source, so there is deliberately no software
//! fallback: if the CPU has no hardware RNG, [`available`] is false and the Bluetooth stack
//! refuses to pair.

use crate::arch;

/// True if the CPU offers RDSEED or RDRAND.
pub fn available() -> bool {
    has_rdseed() || has_rdrand()
}

fn has_rdrand() -> bool {
    arch::cpuid(1, 0).ecx & (1 << 30) != 0
}

fn has_rdseed() -> bool {
    arch::cpuid(7, 0).ebx & (1 << 18) != 0
}

#[target_feature(enable = "rdseed")]
fn rdseed64() -> Option<u64> {
    let mut v = 0u64;
    // Only called after CPUID confirmed RDSEED support.
    (core::arch::x86_64::_rdseed64_step(&mut v) == 1).then_some(v)
}

#[target_feature(enable = "rdrand")]
fn rdrand64() -> Option<u64> {
    let mut v = 0u64;
    // Only called after CPUID confirmed RDRAND support.
    (core::arch::x86_64::_rdrand64_step(&mut v) == 1).then_some(v)
}

fn next_u64() -> u64 {
    for _ in 0..1000 {
        if has_rdseed() {
            // SAFETY: guarded by the CPUID check just above.
            if let Some(v) = unsafe { rdseed64() } {
                return v;
            }
        }
        // SAFETY: as above.
        if has_rdrand() {
            // SAFETY: guarded by the CPUID check just above.
            if let Some(v) = unsafe { rdrand64() } {
                return v;
            }
        }
        core::hint::spin_loop();
    }
    panic!("hardware RNG failed repeatedly");
}

/// The kernel's cryptographic RNG.
pub struct HwRng;

impl ksbt::Rng for HwRng {
    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let r = next_u64().to_le_bytes();
            chunk.copy_from_slice(&r[..chunk.len()]);
        }
    }
}
