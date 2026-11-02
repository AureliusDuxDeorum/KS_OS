// SPDX-License-Identifier: Apache-2.0
//! The Stage 1 screen: the "OS operating" banner and two buttons.
//!
//! Buttons can be driven with the keyboard (Tab / arrows / Enter / Space, or
//! the `P` and `U` accelerators) or the PS/2 mouse.

use crate::fb::{Color, Fb};
use crate::input::{Event, Key};
use crate::ps2::Ps2;
use crate::strbuf::StrBuf;
use core::fmt::Write;

// Placeholder monochrome palette (black / white / gray); the real design language comes later.
const BLACK: Color = Color(0x00, 0x00, 0x00);
const WHITE: Color = Color(0xFF, 0xFF, 0xFF);
const GRAY_LIGHT: Color = Color(0xC8, 0xC8, 0xC8);
const GRAY_MID: Color = Color(0x8A, 0x8A, 0x8A);
const GRAY_DARK: Color = Color(0x3A, 0x3A, 0x3A);
const GRAY_DEEP: Color = Color(0x14, 0x14, 0x14);
const GRAY_HOVER: Color = Color(0x2A, 0x2A, 0x2A);

/// What the user asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    PowerOff,
    BootToFirmware,
}

struct Button {
    label: &'static str,
    action: Action,
    enabled: bool,
}

#[derive(Clone, Copy)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

const CURSOR_W: i32 = 12;
const CURSOR_H: i32 = 19;
const CURSOR: [&str; CURSOR_H as usize] = [
    "X           ",
    "XX          ",
    "X.X         ",
    "X..X        ",
    "X...X       ",
    "X....X      ",
    "X.....X     ",
    "X......X    ",
    "X.......X   ",
    "X........X  ",
    "X.....XXXXX ",
    "X..X..X     ",
    "X.X X..X    ",
    "XX  X..X    ",
    "X    X..X   ",
    "     X..X   ",
    "      X..X  ",
    "      X..X  ",
    "       XX   ",
];

pub struct Ui {
    fb: Fb,
    scale: i32,
    buttons: [Button; 2],
    rects: [Rect; 2],
    focus: usize,
    hover: Option<usize>,
    pressed: Option<usize>,
    mx: i32,
    my: i32,
    left_down: bool,
    saved: [u32; (CURSOR_W * CURSOR_H) as usize],
    cursor_shown: bool,
    status_y: i32,
    has_mouse: bool,
}

impl Ui {
    pub fn new(fb: Fb, firmware_setup_available: bool, has_mouse: bool) -> Self {
        let scale = ((fb.width / 320).min(fb.height / 200)).clamp(1, 6) as i32;
        let buttons = [
            Button {
                label: "Power off",
                action: Action::PowerOff,
                enabled: true,
            },
            Button {
                label: "Boot into UEFI",
                action: Action::BootToFirmware,
                enabled: firmware_setup_available,
            },
        ];
        let bw = Fb::text_width("Boot into UEFI", scale) + 14 * scale;
        let bh = 8 * scale + 14 * scale;
        let gap = 10 * scale;
        let total = 2 * bw + gap;
        let x0 = (fb.width as i32 - total) / 2;
        let y0 = fb.height as i32 / 2;
        let rects = [
            Rect {
                x: x0,
                y: y0,
                w: bw,
                h: bh,
            },
            Rect {
                x: x0 + bw + gap,
                y: y0,
                w: bw,
                h: bh,
            },
        ];
        let (mx, my) = (fb.width as i32 / 2, fb.height as i32 * 3 / 4);
        Self {
            status_y: y0 + bh + 12 * scale,
            fb,
            scale,
            buttons,
            rects,
            focus: 0,
            hover: None,
            pressed: None,
            mx,
            my,
            left_down: false,
            saved: [0; (CURSOR_W * CURSOR_H) as usize],
            cursor_shown: false,
            has_mouse,
        }
    }

    /// Logs the layout on the serial console (consumed by the end-to-end tests).
    pub fn describe(&self) {
        kprintln!(
            "ui: screen {}x{} scale {}",
            self.fb.width,
            self.fb.height,
            self.scale
        );
        for (b, r) in self.buttons.iter().zip(self.rects.iter()) {
            let name = match b.action {
                Action::PowerOff => "PowerOff",
                Action::BootToFirmware => "BootToFirmware",
            };
            kprintln!(
                "ui: button {} rect={},{},{},{} enabled={}",
                name,
                r.x,
                r.y,
                r.w,
                r.h,
                b.enabled
            );
        }
        kprintln!("ui: cursor start {},{}", self.mx, self.my);
    }

    /// Draws the static screen contents.
    pub fn draw_all(&mut self, info: &str) {
        self.hide_cursor();
        let (w, h, s) = (self.fb.width as i32, self.fb.height as i32, self.scale);
        self.fb.fill_rect(0, 0, w, h, BLACK);
        self.fb
            .draw_text_centered(h / 5, "OS operating", 2 * s, WHITE);
        self.fb
            .draw_text_centered(h / 5 + 22 * s, info, s, GRAY_MID);
        let hint = if self.has_mouse {
            "Click a button, or Tab / arrows + Enter.  P = power off, U = UEFI setup, L = log"
        } else {
            "Tab / arrows + Enter.  P = power off, U = UEFI setup, L = log"
        };
        self.fb
            .draw_text_centered(h - 14 * s, hint, (s / 2).max(1), GRAY_MID);
        for i in 0..self.buttons.len() {
            self.draw_button(i);
        }
        self.show_cursor();
    }

    /// Returns `(fill, border, text)` for button `i` in its current state.
    fn state_color(&self, i: usize) -> (Color, Color, Color) {
        if !self.buttons[i].enabled {
            return (BLACK, GRAY_DARK, GRAY_DARK);
        }
        let pressed = self.pressed == Some(i) && self.left_down;
        let border = if self.focus == i { WHITE } else { GRAY_MID };
        if pressed {
            // Inverted while the mouse button is held.
            (WHITE, WHITE, BLACK)
        } else if self.hover == Some(i) {
            (GRAY_HOVER, border, WHITE)
        } else {
            (GRAY_DEEP, border, GRAY_LIGHT)
        }
    }

    fn draw_button(&mut self, i: usize) {
        let r = self.rects[i];
        let (fill, border, text) = self.state_color(i);
        let s = self.scale;
        self.fb.fill_rect(r.x, r.y, r.w, r.h, fill);
        self.fb.stroke_rect(
            r.x,
            r.y,
            r.w,
            r.h,
            if self.focus == i {
                (s / 2).max(1) + 2
            } else {
                (s / 2).max(1)
            },
            border,
        );
        let label = self.buttons[i].label;
        let tw = Fb::text_width(label, s);
        self.fb.draw_text(
            r.x + (r.w - tw) / 2,
            r.y + (r.h - 8 * s) / 2,
            label,
            s,
            text,
        );
    }

    /// Shows a one-line status message below the buttons.
    pub fn set_status(&mut self, msg: &str, level: Level) {
        self.hide_cursor();
        let s = self.scale;
        let line_h = 8 * s + 4;
        self.fb
            .fill_rect(0, self.status_y, self.fb.width as i32, line_h, BLACK);
        // Severity is conveyed by form, not colour: plain, prefixed, or inverted.
        match level {
            Level::Info => self.fb.draw_text_centered(self.status_y, msg, s, WHITE),
            Level::Warn => {
                let mut line = StrBuf::<96>::new();
                let _ = write!(line, "! {msg}");
                self.fb
                    .draw_text_centered(self.status_y, line.as_str(), s, GRAY_LIGHT);
            }
            Level::Error => {
                let mut line = StrBuf::<96>::new();
                let _ = write!(line, " ERROR: {msg} ");
                let w = Fb::text_width(line.as_str(), s);
                let x = (self.fb.width as i32 - w) / 2;
                self.fb.fill_rect(x, self.status_y, w, line_h, WHITE);
                self.fb
                    .draw_text(x, self.status_y + 2, line.as_str(), s, BLACK);
            }
        }
        self.show_cursor();
    }

    // ---- cursor -----------------------------------------------------------

    fn hide_cursor(&mut self) {
        if !self.cursor_shown || !self.has_mouse {
            return;
        }
        for cy in 0..CURSOR_H {
            for cx in 0..CURSOR_W {
                if CURSOR[cy as usize].as_bytes()[cx as usize] != b' ' {
                    let v = self.saved[(cy * CURSOR_W + cx) as usize];
                    self.fb.set_raw(self.mx + cx, self.my + cy, v);
                }
            }
        }
        self.cursor_shown = false;
    }

    fn show_cursor(&mut self) {
        if self.cursor_shown || !self.has_mouse {
            return;
        }
        let black = self.fb.pixel_value(Color(0, 0, 0));
        let white = self.fb.pixel_value(Color(255, 255, 255));
        for cy in 0..CURSOR_H {
            for cx in 0..CURSOR_W {
                let c = CURSOR[cy as usize].as_bytes()[cx as usize];
                if c == b' ' {
                    continue;
                }
                self.saved[(cy * CURSOR_W + cx) as usize] =
                    self.fb.read_raw(self.mx + cx, self.my + cy);
                self.fb.set_raw(
                    self.mx + cx,
                    self.my + cy,
                    if c == b'X' { black } else { white },
                );
            }
        }
        self.cursor_shown = true;
    }

    // ---- input ------------------------------------------------------------

    fn button_at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.rects.len()).find(|&i| self.rects[i].contains(x, y))
    }

    fn refresh_buttons(&mut self) {
        self.hide_cursor();
        for i in 0..self.buttons.len() {
            self.draw_button(i);
        }
        self.show_cursor();
    }

    fn activate(&self, i: usize) -> Option<Action> {
        Some(self.buttons[i].action)
    }

    /// Draws the full-screen kernel log with `title` in the header bar.
    fn render_log(&mut self, title: &str) {
        self.hide_cursor();
        let scale = if self.fb.width >= 1024 { 2 } else { 1 };
        let (w, h) = (self.fb.width as i32, self.fb.height as i32);
        let cols = (w / (8 * scale) - 2).max(8) as usize;
        let rows = (h / (8 * scale + 2) - 2).max(4) as usize;
        self.fb.fill_rect(0, 0, w, h, BLACK);
        self.fb.fill_rect(0, 0, w, 8 * scale + 4, WHITE);
        self.fb.draw_text(8, 2, title, scale, BLACK);
        for (i, line) in crate::klog::tail(rows, cols).iter().enumerate() {
            self.fb.draw_text(
                8,
                (10 * scale + 4) + i as i32 * (8 * scale + 2),
                line,
                scale,
                GRAY_LIGHT,
            );
        }
    }

    fn redraw_main(&mut self) {
        let mut info = StrBuf::<96>::new();
        info_line(&mut info, &self.fb, "UEFI");
        self.draw_all(info.as_str());
        self.show_cursor();
    }

    /// Full-screen kernel log; any of L / Esc / Enter / Space returns to the main screen.
    fn show_log(&mut self, ps2: &mut Ps2) {
        self.render_log("KERNEL LOG - L / Esc / Enter to return");
        loop {
            match ps2.poll().or_else(crate::input::pop) {
                Some(Event::Key(Key::Char(b'l') | Key::Escape | Key::Enter | Key::Space)) => break,
                Some(_) => {}
                None => crate::arch::enable_and_halt(),
            }
        }
        self.redraw_main();
    }

    fn input_available(ps2: &Ps2) -> bool {
        ps2.has_keyboard
            || ps2.has_mouse
            || crate::usb::hid::bound_count() > 0
            || crate::bt::connected_count() > 0
    }

    /// Shown automatically when no input device works at all: a live, refreshing kernel log, so a
    /// machine whose keyboard/mouse are not (yet) usable can still be diagnosed from the screen.
    fn diagnostic_log(&mut self, ps2: &mut Ps2) {
        kprintln!("ui: no input device; diagnostic log view active");
        loop {
            self.render_log(
                "DIAGNOSTICS - no working input device yet; log refreshes every second",
            );
            for _ in 0..20 {
                if Self::input_available(ps2) || ps2.poll().or_else(crate::input::pop).is_some() {
                    self.redraw_main();
                    return;
                }
                crate::bt::service();
                crate::time::sleep_ms(50);
            }
        }
    }

    /// Blocks until the user activates a button and returns the chosen action.
    pub fn run(&mut self, ps2: &mut Ps2) -> Action {
        self.refresh_buttons();
        let mut diagnosed = false;
        loop {
            let Some(ev) = ps2.poll().or_else(crate::input::pop) else {
                if !diagnosed && !Self::input_available(ps2) {
                    diagnosed = true;
                    self.diagnostic_log(ps2);
                    continue;
                }
                // Bluetooth work happens in short slices while the UI is idle.
                crate::bt::service();
                if let Some(s) = crate::bt::take_status() {
                    self.set_status(&s, Level::Info);
                }
                // Sleep until the next interrupt (input or the 1 kHz tick).
                crate::arch::enable_and_halt();
                continue;
            };
            match ev {
                Event::Key(k) => {
                    let n = self.buttons.len();
                    match k {
                        Key::Tab | Key::Right | Key::Down => self.focus = (self.focus + 1) % n,
                        Key::BackTab | Key::Left | Key::Up => self.focus = (self.focus + n - 1) % n,
                        Key::Enter | Key::Space => {
                            if let Some(a) = self.activate(self.focus) {
                                return a;
                            }
                        }
                        Key::Char(b'p') => return Action::PowerOff,
                        Key::Char(b'u') => return Action::BootToFirmware,
                        Key::Char(b'l') => {
                            self.show_log(ps2);
                            continue;
                        }
                        _ => continue,
                    }
                    self.refresh_buttons();
                }
                Event::Mouse { dx, dy, left } => {
                    // A pointing device may appear at any time (e.g. a Bluetooth mouse connecting).
                    self.has_mouse = true;
                    self.hide_cursor();
                    self.mx = (self.mx + dx).clamp(0, self.fb.width as i32 - 1);
                    self.my = (self.my + dy).clamp(0, self.fb.height as i32 - 1);
                    let over = self.button_at(self.mx, self.my);
                    let was_down = self.left_down;
                    self.left_down = left;
                    let mut result = None;
                    if left && !was_down {
                        self.pressed = over;
                        if let Some(i) = over {
                            self.focus = i;
                        }
                    } else if !left && was_down {
                        if over.is_some() && over == self.pressed {
                            result = over;
                        }
                        self.pressed = None;
                    }
                    let changed = over != self.hover || left != was_down;
                    self.hover = over;
                    if changed {
                        for i in 0..self.buttons.len() {
                            self.draw_button(i);
                        }
                    }
                    self.show_cursor();
                    if let Some(a) = result.and_then(|i| self.activate(i)) {
                        return a;
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// Formats the descriptive line shown under the banner.
pub fn info_line(buf: &mut StrBuf<96>, fb: &Fb, firmware: &str) {
    let _ = write!(
        buf,
        "KS_OS {} | x86_64 | {} | {}x{}",
        crate::VERSION,
        firmware,
        fb.width,
        fb.height
    );
}
