//! 16550 UART driver (COM1), used as the primary debug console.

use crate::arch::{inb, outb};
use crate::sync::IrqMutex;
use core::fmt::{self, Write};

const COM1: u16 = 0x3F8;

struct Serial;

static SERIAL: IrqMutex<Serial> = IrqMutex::new(Serial);

impl Serial {
    fn put(&mut self, byte: u8) {
        // SAFETY: COM1 register accesses; bounded wait so an absent UART cannot hang us.
        unsafe {
            for _ in 0..100_000 {
                if inb(COM1 + 5) & 0x20 != 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            outb(COM1, byte);
        }
    }
}

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                self.put(b'\r');
            }
            self.put(b);
        }
        Ok(())
    }
}

/// Initialises COM1 at 115200 8N1 with FIFOs enabled.
pub fn init() {
    // SAFETY: standard 16550 initialisation sequence on the legacy COM1 port.
    unsafe {
        outb(COM1 + 1, 0x00); // disable interrupts
        outb(COM1 + 3, 0x80); // DLAB on
        outb(COM1, 0x01); // divisor low: 115200 baud
        outb(COM1 + 1, 0x00); // divisor high
        outb(COM1 + 3, 0x03); // 8 bits, no parity, one stop bit
        outb(COM1 + 2, 0xC7); // enable FIFO, clear, 14-byte threshold
        outb(COM1 + 4, 0x0B); // DTR, RTS, OUT2
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    /// Sends everything to both the UART and the in-memory log, whatever the length.
    struct Tee<'a>(&'a mut Serial);
    impl Write for Tee<'_> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            crate::klog::append(s);
            self.0.write_str(s)
        }
    }
    let mut guard = SERIAL.lock();
    let _ = Tee(&mut guard).write_fmt(args);
}

/// Prints from the panic path without risking a deadlock on the serial lock.
pub fn _print_panic(args: fmt::Arguments<'_>) {
    // SAFETY: we are about to halt; no other context will use the lock again.
    unsafe { SERIAL.force_unlock() };
    _print(args);
}
