// SPDX-License-Identifier: Apache-2.0
//! Polling driver for the legacy i8042 PS/2 controller (keyboard and mouse).
//!
//! Interrupt-driven input arrives with the interrupt work in a later phase;
//! polling is sufficient for the Stage 1 interface and keeps the boot path free
//! of IDT/APIC dependencies. USB HID is out of scope for Stage 1 (see ADR-0008).

use crate::arch::{inb, outb};
use crate::idt::TrapFrame;
use crate::input::{Event, Key};
use core::sync::atomic::{AtomicU16, AtomicUsize, Ordering};

const DATA: u16 = 0x60;
const CMD: u16 = 0x64;

const ST_OUT_FULL: u8 = 1 << 0;
const ST_IN_FULL: u8 = 1 << 1;
const ST_AUX: u8 = 1 << 5;

const SPINS: u32 = 200_000;

// Lock-free single-producer (IRQ handler) / single-consumer (`Ps2::poll`) byte ring.
// Each slot holds `(from_mouse << 8) | byte`.
const RING: usize = 128;
static RING_BUF: [AtomicU16; RING] = [const { AtomicU16::new(0) }; RING];
static RING_HEAD: AtomicUsize = AtomicUsize::new(0);
static RING_TAIL: AtomicUsize = AtomicUsize::new(0);
static IRQ_COUNT: AtomicUsize = AtomicUsize::new(0);

fn ring_push(v: u16) {
    let head = RING_HEAD.load(Ordering::Relaxed);
    let next = (head + 1) % RING;
    if next == RING_TAIL.load(Ordering::Acquire) {
        return; // full: drop the byte rather than block in an interrupt handler
    }
    RING_BUF[head].store(v, Ordering::Relaxed);
    RING_HEAD.store(next, Ordering::Release);
}

fn ring_pop() -> Option<u16> {
    let tail = RING_TAIL.load(Ordering::Relaxed);
    if tail == RING_HEAD.load(Ordering::Acquire) {
        return None;
    }
    let v = RING_BUF[tail].load(Ordering::Relaxed);
    RING_TAIL.store((tail + 1) % RING, Ordering::Release);
    Some(v)
}

/// Number of PS/2 interrupts taken so far.
pub fn irq_count() -> usize {
    IRQ_COUNT.load(Ordering::Relaxed)
}

/// Interrupt handler shared by the keyboard (IRQ 1) and mouse (IRQ 12) lines:
/// drains the controller's output buffer into the ring.
pub fn irq_handler(_: &mut TrapFrame) {
    IRQ_COUNT.fetch_add(1, Ordering::Relaxed);
    for _ in 0..16 {
        // SAFETY: reading the i8042 status and data registers.
        let st = unsafe { inb(CMD) };
        if st & ST_OUT_FULL == 0 {
            break;
        }
        // SAFETY: output buffer is full.
        let b = unsafe { inb(DATA) };
        ring_push((u16::from(st & ST_AUX != 0) << 8) | u16::from(b));
    }
}

fn wait_input_clear() -> bool {
    for _ in 0..SPINS {
        // SAFETY: reading the i8042 status register.
        if unsafe { inb(CMD) } & ST_IN_FULL == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn wait_output_full() -> bool {
    for _ in 0..SPINS {
        // SAFETY: reading the i8042 status register.
        if unsafe { inb(CMD) } & ST_OUT_FULL != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

fn command(c: u8) -> bool {
    if !wait_input_clear() {
        return false;
    }
    // SAFETY: writing a controller command byte.
    unsafe { outb(CMD, c) };
    true
}

fn write_data(b: u8) -> bool {
    if !wait_input_clear() {
        return false;
    }
    // SAFETY: writing to the i8042 data port.
    unsafe { outb(DATA, b) };
    true
}

fn read_data() -> Option<u8> {
    if wait_output_full() {
        // SAFETY: output buffer is full.
        Some(unsafe { inb(DATA) })
    } else {
        None
    }
}

fn flush() {
    for _ in 0..64 {
        // SAFETY: draining stale bytes.
        if unsafe { inb(CMD) } & ST_OUT_FULL == 0 {
            break;
        }
        // SAFETY: output buffer is full.
        let _ = unsafe { inb(DATA) };
    }
}

/// Sends a byte to the keyboard (`mouse == false`) or mouse and waits for ACK.
fn device_write(mouse: bool, b: u8) -> bool {
    for _ in 0..3 {
        if mouse && !command(0xD4) {
            return false;
        }
        if !write_data(b) {
            return false;
        }
        match read_data() {
            Some(0xFA) => return true,
            Some(0xFE) => continue, // resend
            _ => return false,
        }
    }
    false
}

pub struct Ps2 {
    pub has_keyboard: bool,
    pub has_mouse: bool,
    irq: bool,
    e0: bool,
    shift: bool,
    packet: [u8; 3],
    idx: usize,
}

impl Ps2 {
    /// Initialises the controller and enables whichever devices answer.
    pub fn init() -> Self {
        let mut s = Self {
            has_keyboard: false,
            has_mouse: false,
            irq: false,
            e0: false,
            shift: false,
            packet: [0; 3],
            idx: 0,
        };

        // No controller at all (USB-only machines float the bus high): leave everything off.
        // SAFETY: reading the i8042 status register.
        if unsafe { inb(CMD) } == 0xFF {
            return s;
        }
        flush();
        command(0xAD); // disable keyboard port
        command(0xA7); // disable mouse port
        flush();

        // Config byte: no IRQs (we poll), translation on so we see scancode set 1.
        command(0x20);
        let mut cfg = read_data().unwrap_or(0x45);
        cfg &= !((1 << 0) | (1 << 1));
        cfg |= 1 << 6;
        command(0x60);
        write_data(cfg);

        // Controller self-test; some controllers reset the config byte, so rewrite it.
        command(0xAA);
        let _ = read_data();
        command(0x60);
        write_data(cfg);

        command(0xAE); // enable keyboard port
        command(0xA8); // enable mouse port
        flush();

        s.has_keyboard = device_write(false, 0xF4); // enable scanning
        if device_write(true, 0xF6) {
            // defaults
            s.has_mouse = device_write(true, 0xF4); // enable streaming
        }
        flush();
        s
    }

    /// Switches the controller to interrupt-driven operation (after the IRQ lines are routed).
    pub fn enable_irq(&mut self) {
        flush();
        command(0x20);
        let cfg = read_data().unwrap_or(0x45) | 0b11; // enable keyboard and mouse IRQs
        command(0x60);
        write_data(cfg);
        self.irq = true;
    }

    /// Returns one decoded event, if any input is pending.
    pub fn poll(&mut self) -> Option<Event> {
        if !self.has_keyboard && !self.has_mouse {
            return None; // no controller: the status port floats high and would look "always full"
        }
        for _ in 0..64 {
            let (aux, b) = if self.irq {
                let v = ring_pop()?;
                (v >> 8 != 0, v as u8)
            } else {
                // SAFETY: reading the status register.
                let st = unsafe { inb(CMD) };
                if st == 0xFF || st & ST_OUT_FULL == 0 {
                    return None; // 0xFF: bus floating high, no device behind the port
                }
                // SAFETY: output buffer full.
                (st & ST_AUX != 0, unsafe { inb(DATA) })
            };
            let ev = if aux {
                self.mouse_byte(b)
            } else {
                self.key_byte(b)
            };
            if ev.is_some() {
                return ev;
            }
        }
        None
    }

    fn mouse_byte(&mut self, b: u8) -> Option<Event> {
        if self.idx == 0 && b & 0x08 == 0 {
            return None; // out of sync: first byte always has bit 3 set
        }
        self.packet[self.idx] = b;
        self.idx += 1;
        if self.idx < 3 {
            return None;
        }
        self.idx = 0;
        let [flags, x, y] = self.packet;
        if flags & 0xC0 != 0 {
            return None; // overflow: discard
        }
        let dx = i32::from(x) - if flags & 0x10 != 0 { 256 } else { 0 };
        let dy = i32::from(y) - if flags & 0x20 != 0 { 256 } else { 0 };
        Some(Event::Mouse {
            dx,
            dy: -dy,
            left: flags & 1 != 0,
        })
    }

    fn key_byte(&mut self, b: u8) -> Option<Event> {
        if b == 0xE0 {
            self.e0 = true;
            return None;
        }
        let release = b & 0x80 != 0;
        let code = b & 0x7F;
        let e0 = core::mem::take(&mut self.e0);

        if matches!(code, 0x2A | 0x36) && !e0 {
            self.shift = !release;
            return None;
        }
        if release {
            return None;
        }
        let key = if e0 {
            match code {
                0x48 => Key::Up,
                0x50 => Key::Down,
                0x4B => Key::Left,
                0x4D => Key::Right,
                0x1C => Key::Enter,
                _ => return None,
            }
        } else {
            match code {
                0x01 => Key::Escape,
                0x0F if self.shift => Key::BackTab,
                0x0F => Key::Tab,
                0x1C => Key::Enter,
                0x39 => Key::Space,
                0x19 => Key::Char(b'p'),
                0x16 => Key::Char(b'u'),
                0x26 => Key::Char(b'l'),
                _ => return None,
            }
        };
        Some(Event::Key(key))
    }
}
