//! Interrupt vector allocation and dispatch for vectors 32..=255.
//!
//! Handlers are plain `fn(&mut TrapFrame)` registered in a lock-free table, so
//! registration and dispatch are safe from any context. The dispatcher sends the
//! end-of-interrupt to the local APIC after the handler returns.

use crate::apic;
use crate::idt::TrapFrame;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

pub type Handler = fn(&mut TrapFrame);

/// First vector available for dynamic allocation (0x20..0x2F is left for stray legacy PIC IRQs).
const FIRST_DYNAMIC: u8 = 0x30;
/// Last vector available for dynamic allocation (above are the fixed timer/error/spurious vectors).
const LAST_DYNAMIC: u8 = 0xDF;

pub const TIMER_VECTOR: u8 = 0xEC;
pub const ERROR_VECTOR: u8 = 0xFE;
pub const SPURIOUS_VECTOR: u8 = 0xFF;

static HANDLERS: [AtomicUsize; 256] = [const { AtomicUsize::new(0) }; 256];
static NEXT_VECTOR: AtomicU32 = AtomicU32::new(FIRST_DYNAMIC as u32);
static UNHANDLED: AtomicU32 = AtomicU32::new(0);

/// Registers `handler` for `vector`. Panics if the vector is already taken.
pub fn register(vector: u8, handler: Handler) {
    let prev = HANDLERS[usize::from(vector)].swap(handler as usize, Ordering::AcqRel);
    assert!(prev == 0, "irq: vector {vector:#x} registered twice");
}

/// Allocates an unused vector in the dynamic range.
pub fn alloc_vector() -> Option<u8> {
    let v = NEXT_VECTOR.fetch_add(1, Ordering::Relaxed);
    (v <= u32::from(LAST_DYNAMIC)).then_some(v as u8)
}

/// Entry from the assembly stubs for every vector >= 32.
pub fn dispatch(frame: &mut TrapFrame) {
    let vector = frame.vector as u8;
    if vector == SPURIOUS_VECTOR {
        return; // spurious interrupts must not be acknowledged
    }
    let h = HANDLERS[usize::from(vector)].load(Ordering::Acquire);
    if h == 0 {
        // Rate-limit: stray interrupts must not flood the console.
        if UNHANDLED.fetch_add(1, Ordering::Relaxed) < 8 {
            kprintln!("irq: unhandled interrupt, vector {vector:#x}");
        }
    } else {
        // SAFETY: only `register` stores into the table, and it stores valid `Handler` pointers.
        let handler: Handler = unsafe { core::mem::transmute::<usize, Handler>(h) };
        handler(frame);
    }
    apic::eoi();
}
