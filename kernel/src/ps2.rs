//! Polling driver for the legacy i8042 PS/2 controller (keyboard and mouse).
//!
//! Interrupt-driven input arrives with the interrupt work in a later phase;
//! polling is sufficient for the Stage 1 interface and keeps the boot path free
//! of IDT/APIC dependencies. USB HID is out of scope for Stage 1 (see ADR-0008).

use crate::arch::{inb, outb};

const DATA: u16 = 0x60;
const CMD: u16 = 0x64;

const ST_OUT_FULL: u8 = 1 << 0;
const ST_IN_FULL: u8 = 1 << 1;
const ST_AUX: u8 = 1 << 5;

const SPINS: u32 = 200_000;

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Tab,
    BackTab,
    Left,
    Right,
    Up,
    Down,
    Enter,
    Space,
    Escape,
    /// Lower-case ASCII letter.
    Char(u8),
}

#[derive(Clone, Copy, Debug)]
pub enum Event {
    Key(Key),
    /// Relative mouse motion (y grows downwards) and the left-button state.
    Mouse {
        dx: i32,
        dy: i32,
        left: bool,
    },
}

pub struct Ps2 {
    pub has_keyboard: bool,
    pub has_mouse: bool,
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
            e0: false,
            shift: false,
            packet: [0; 3],
            idx: 0,
        };

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

    /// Polls for one decoded event.
    pub fn poll(&mut self) -> Option<Event> {
        // SAFETY: reading the status register.
        let st = unsafe { inb(CMD) };
        if st & ST_OUT_FULL == 0 {
            return None;
        }
        // SAFETY: output buffer full.
        let b = unsafe { inb(DATA) };
        if st & ST_AUX != 0 {
            self.mouse_byte(b)
        } else {
            self.key_byte(b)
        }
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
                _ => return None,
            }
        };
        Some(Event::Key(key))
    }
}
