// SPDX-License-Identifier: Apache-2.0
//! Timekeeping: calibrated TSC clock, local APIC timer tick and sleeping.
//!
//! The ACPI PM timer (fixed 3.579545 MHz) is the reference used to calibrate
//! both the TSC and the local APIC timer.

use crate::acpi::{PmTimer, PM_TIMER_HZ};
use crate::{apic, arch, irq};
use core::sync::atomic::{AtomicU64, Ordering};

/// Scheduler tick rate.
pub const TICK_HZ: u64 = 1000;

static TSC_HZ: AtomicU64 = AtomicU64::new(0);
static TSC_BASE: AtomicU64 = AtomicU64::new(0);
static LAPIC_TIMER_HZ: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);

const DIVIDE_BY_16: u32 = 0b0011;

/// Measures TSC and local-APIC-timer rates against the PM timer; returns `(tsc_hz, lapic_hz)`.
fn measure(pm: &PmTimer) -> (u64, u64) {
    const WINDOW_MS: u64 = 40;
    let ticks = PM_TIMER_HZ * WINDOW_MS / 1000;
    apic::write(apic::REG_TIMER_DIV, DIVIDE_BY_16);
    apic::write(apic::REG_LVT_TIMER, apic::LVT_MASKED);
    apic::write(apic::REG_TIMER_INIT, u32::MAX);
    let (pm0, tsc0) = (pm.read(), arch::rdtsc());
    let mut pm_now;
    loop {
        pm_now = pm.read();
        if u64::from(pm.elapsed(pm0, pm_now)) >= ticks {
            break;
        }
        core::hint::spin_loop();
    }
    let (tsc1, lapic_left) = (arch::rdtsc(), apic::read(apic::REG_TIMER_CUR));
    apic::write(apic::REG_TIMER_INIT, 0);
    let elapsed_pm = u64::from(pm.elapsed(pm0, pm_now));
    let tsc_hz = (tsc1 - tsc0) * PM_TIMER_HZ / elapsed_pm;
    let lapic_hz = u64::from(u32::MAX - lapic_left) * 16 * PM_TIMER_HZ / elapsed_pm;
    (tsc_hz, lapic_hz)
}

/// Calibrates clocks (median of three runs) and starts the periodic tick.
pub fn init(pm: &PmTimer) {
    let mut runs = [measure(pm), measure(pm), measure(pm)];
    runs.sort_unstable_by_key(|r| r.0);
    let (tsc_hz, _) = runs[1];
    runs.sort_unstable_by_key(|r| r.1);
    let (_, lapic_hz) = runs[1];
    TSC_HZ.store(tsc_hz, Ordering::Relaxed);
    LAPIC_TIMER_HZ.store(lapic_hz, Ordering::Relaxed);
    TSC_BASE.store(arch::rdtsc(), Ordering::Relaxed);

    // SAFETY-free: CPUID query only.
    let invariant = arch::cpuid(0x8000_0007, 0).edx & (1 << 8) != 0;
    kprintln!(
        "time: TSC {} MHz (invariant={invariant}), APIC timer {} MHz",
        tsc_hz / 1_000_000,
        lapic_hz / 1_000_000
    );

    irq::register(irq::TIMER_VECTOR, on_tick);
    let count = (lapic_hz / 16 / TICK_HZ).max(1) as u32;
    apic::write(apic::REG_TIMER_DIV, DIVIDE_BY_16);
    apic::write(
        apic::REG_LVT_TIMER,
        apic::LVT_PERIODIC | u32::from(irq::TIMER_VECTOR),
    );
    apic::write(apic::REG_TIMER_INIT, count);
}

fn on_tick(_: &mut crate::idt::TrapFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Timer ticks since the tick was started.
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Monotonic nanoseconds since [`init`].
pub fn now_ns() -> u64 {
    let hz = TSC_HZ.load(Ordering::Relaxed).max(1);
    let d = arch::rdtsc().saturating_sub(TSC_BASE.load(Ordering::Relaxed));
    (u128::from(d) * 1_000_000_000 / u128::from(hz)) as u64
}

/// Sleeps for at least `ms` milliseconds, halting the CPU between interrupts.
pub fn sleep_ms(ms: u64) {
    let deadline = now_ns() + ms * 1_000_000;
    while now_ns() < deadline {
        arch::enable_and_halt();
    }
}
