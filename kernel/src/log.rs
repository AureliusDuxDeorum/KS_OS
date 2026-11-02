// SPDX-License-Identifier: Apache-2.0
//! Kernel logging macros. Output goes to the COM1 serial port.

macro_rules! kprint {
    ($($arg:tt)*) => { $crate::serial::_print(format_args!($($arg)*)) };
}

macro_rules! kprintln {
    () => { kprint!("\n") };
    ($($arg:tt)*) => {{ kprint!($($arg)*); kprint!("\n"); }};
}
