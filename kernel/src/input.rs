// SPDX-License-Identifier: Apache-2.0
//! Unified input events and the queue USB/Bluetooth HID drivers push into.
//!
//! PS/2 is still polled by the UI directly; every other source (xHCI HID now, Bluetooth HID
//! later) calls [`push`] from interrupt context, and the UI drains it with [`pop`].

use crate::sync::IrqMutex;

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
    /// Relative pointer motion (y grows downwards) and the left-button state.
    Mouse {
        dx: i32,
        dy: i32,
        left: bool,
    },
}

const CAPACITY: usize = 128;

struct Queue {
    buf: [Option<Event>; CAPACITY],
    head: usize,
    len: usize,
}

static QUEUE: IrqMutex<Queue> = IrqMutex::new(Queue {
    buf: [None; CAPACITY],
    head: 0,
    len: 0,
});

/// Enqueues an event; when full the oldest event is dropped (the UI would rather see fresh input).
pub fn push(ev: Event) {
    let mut q = QUEUE.lock();
    if q.len == CAPACITY {
        q.head = (q.head + 1) % CAPACITY;
        q.len -= 1;
    }
    let tail = (q.head + q.len) % CAPACITY;
    q.buf[tail] = Some(ev);
    q.len += 1;
}

/// Dequeues the oldest event.
pub fn pop() -> Option<Event> {
    let mut q = QUEUE.lock();
    if q.len == 0 {
        return None;
    }
    let head = q.head;
    let ev = q.buf[head].take();
    q.head = (q.head + 1) % CAPACITY;
    q.len -= 1;
    ev
}

/// Maps a HID keyboard usage (page 0x07) to a UI key.
pub fn usage_to_key(usage: u8, shift: bool) -> Option<Key> {
    Some(match usage {
        0x2B if shift => Key::BackTab,
        0x2B => Key::Tab,
        0x4F => Key::Right,
        0x50 => Key::Left,
        0x51 => Key::Down,
        0x52 => Key::Up,
        0x28 | 0x58 => Key::Enter,
        0x2C => Key::Space,
        0x29 => Key::Escape,
        0x13 => Key::Char(b'p'),
        0x18 => Key::Char(b'u'),
        0x0F => Key::Char(b'l'),
        _ => return None,
    })
}

/// Turns successive decoded HID reports from one device into events: key presses are reported
/// on the report where they first appear, pointer reports whenever anything changed.
#[derive(Default)]
pub struct Tracker {
    prev_keys: alloc::vec::Vec<u8>,
    prev_buttons: u32,
}

impl Tracker {
    pub fn publish(&mut self, input: &ksparse::hid::Input, keyboard: bool, pointer: bool) {
        if keyboard {
            let shift = input.modifiers & 0b0010_0010 != 0;
            for &k in input.keys.iter().filter(|k| !self.prev_keys.contains(k)) {
                if let Some(key) = usage_to_key(k, shift) {
                    push(Event::Key(key));
                }
            }
            self.prev_keys.clone_from(&input.keys);
        }
        if pointer && (input.dx != 0 || input.dy != 0 || input.buttons != self.prev_buttons) {
            push(Event::Mouse {
                dx: input.dx,
                dy: input.dy,
                left: input.buttons & 1 != 0,
            });
            self.prev_buttons = input.buttons;
        }
    }
}
