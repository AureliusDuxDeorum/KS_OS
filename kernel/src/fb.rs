//! Linear-framebuffer renderer: rectangles, bitmap text and a software cursor.

use crate::limine;

/// An 8-bit-per-channel colour.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8);

/// A 32-bit linear framebuffer with arbitrary RGB channel layout.
pub struct Fb {
    base: *mut u32,
    pub width: usize,
    pub height: usize,
    stride: usize, // in pixels
    shifts: [u8; 3],
    sizes: [u8; 3],
}

impl Fb {
    /// Builds a renderer from the bootloader framebuffer; only 32 bpp RGB is supported.
    ///
    /// # Safety
    /// `fb.address` must point to a mapped framebuffer of `pitch * height` bytes
    /// that nothing else writes to.
    pub unsafe fn new(fb: &limine::Framebuffer) -> Option<Self> {
        if fb.bpp != 32 || fb.memory_model != limine::FRAMEBUFFER_RGB || !fb.pitch.is_multiple_of(4)
        {
            return None;
        }
        Some(Self {
            base: fb.address.cast::<u32>(),
            width: fb.width as usize,
            height: fb.height as usize,
            stride: (fb.pitch / 4) as usize,
            shifts: [fb.red_mask_shift, fb.green_mask_shift, fb.blue_mask_shift],
            sizes: [fb.red_mask_size, fb.green_mask_size, fb.blue_mask_size],
        })
    }

    #[inline]
    fn pack(&self, c: Color) -> u32 {
        let ch = |v: u8, i: usize| {
            (u32::from(v) >> (8 - u32::from(self.sizes[i]).min(8))) << self.shifts[i]
        };
        ch(c.0, 0) | ch(c.1, 1) | ch(c.2, 2)
    }

    #[inline]
    fn put_raw(&mut self, x: usize, y: usize, v: u32) {
        // SAFETY: callers bounds-check against width/height; the framebuffer is mapped.
        unsafe { self.base.add(y * self.stride + x).write_volatile(v) };
    }

    #[inline]
    pub fn get_raw(&self, x: usize, y: usize) -> u32 {
        // SAFETY: callers bounds-check against width/height.
        unsafe { self.base.add(y * self.stride + x).read_volatile() }
    }

    /// Writes a pre-packed pixel if it is on screen.
    pub fn set_raw(&mut self, x: i32, y: i32, v: u32) {
        if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
            self.put_raw(x as usize, y as usize, v);
        }
    }

    /// Reads a packed pixel, or 0 if off screen.
    pub fn read_raw(&self, x: i32, y: i32) -> u32 {
        if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
            self.get_raw(x as usize, y as usize)
        } else {
            0
        }
    }

    pub fn pixel_value(&self, c: Color) -> u32 {
        self.pack(c)
    }

    /// Fills a rectangle, clipped to the screen.
    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: Color) {
        let v = self.pack(c);
        let x0 = x.max(0) as usize;
        let y0 = y.max(0) as usize;
        let x1 = ((x + w).max(0) as usize).min(self.width);
        let y1 = ((y + h).max(0) as usize).min(self.height);
        for yy in y0..y1 {
            for xx in x0..x1 {
                self.put_raw(xx, yy, v);
            }
        }
    }

    /// Draws a rectangle outline of the given thickness (inside the rectangle).
    pub fn stroke_rect(&mut self, x: i32, y: i32, w: i32, h: i32, t: i32, c: Color) {
        self.fill_rect(x, y, w, t, c);
        self.fill_rect(x, y + h - t, w, t, c);
        self.fill_rect(x, y + t, t, h - 2 * t, c);
        self.fill_rect(x + w - t, y + t, t, h - 2 * t, c);
    }

    /// Draws one glyph at integer `scale`. Non-ASCII characters render as `?`.
    pub fn draw_char(&mut self, x: i32, y: i32, ch: char, scale: i32, c: Color) {
        let idx = if (ch as u32) < 128 {
            ch as usize
        } else {
            b'?' as usize
        };
        let glyph = font8x8::legacy::BASIC_LEGACY[idx];
        let v = self.pack(c);
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..8 {
                if bits >> col & 1 == 1 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            self.set_raw(x + col * scale + dx, y + row as i32 * scale + dy, v);
                        }
                    }
                }
            }
        }
    }

    /// Draws a string and returns its pixel width.
    pub fn draw_text(&mut self, x: i32, y: i32, s: &str, scale: i32, c: Color) -> i32 {
        let mut cx = x;
        for ch in s.chars() {
            self.draw_char(cx, y, ch, scale, c);
            cx += 8 * scale;
        }
        cx - x
    }

    /// Pixel width of `s` at `scale`.
    pub fn text_width(s: &str, scale: i32) -> i32 {
        s.chars().count() as i32 * 8 * scale
    }

    /// Draws `s` horizontally centred on the screen.
    pub fn draw_text_centered(&mut self, y: i32, s: &str, scale: i32, c: Color) {
        let w = Self::text_width(s, scale);
        self.draw_text((self.width as i32 - w) / 2, y, s, scale, c);
    }
}
