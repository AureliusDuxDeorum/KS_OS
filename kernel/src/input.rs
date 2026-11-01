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
