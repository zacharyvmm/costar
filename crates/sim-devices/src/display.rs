//! Virtual display peripheral.
//!
//! A `VirtualDisplay` models a framebuffer-backed display that C firmware
//! can draw to via set_pixel, fill_rect, and draw_bitmap.  Dirty region
//! tracking lets the GUI efficiently re-render only changed areas.

use std::fmt;

/// Color mode for a virtual display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayColorMode {
    /// RGB565 — 2 bytes per pixel
    Rgb565,
    /// RGB888 — 3 bytes per pixel
    Rgb888,
    /// ARGB8888 — 4 bytes per pixel
    Argb8888,
}

impl DisplayColorMode {
    /// Return bytes per pixel for this mode.
    pub fn bytes_per_pixel(&self) -> usize {
        match self {
            DisplayColorMode::Rgb565 => 2,
            DisplayColorMode::Rgb888 => 3,
            DisplayColorMode::Argb8888 => 4,
        }
    }
}

impl fmt::Display for DisplayColorMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DisplayColorMode::Rgb565 => write!(f, "rgb565"),
            DisplayColorMode::Rgb888 => write!(f, "rgb888"),
            DisplayColorMode::Argb8888 => write!(f, "argb8888"),
        }
    }
}

/// A rectangular region of the display that has been modified.
#[derive(Debug, Clone)]
pub struct DisplayRect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// Base64-encoded pixel data for this region.
    pub data_base64: String,
}

/// Virtual display device — tracks a framebuffer that C firmware can
/// draw to via set_pixel / fill_rect / draw_bitmap.
///
/// The framebuffer stores raw pixels in the configured color mode.
/// Dirty rects track which regions have changed since last inspection.
pub struct VirtualDisplay {
    /// Device instance ID.
    pub id: u32,

    /// Width in pixels.
    pub width: u16,

    /// Height in pixels.
    pub height: u16,

    /// Color mode.
    pub color_mode: DisplayColorMode,

    /// Whether the display is enabled.
    pub enabled: bool,

    /// Backlight level 0-100.
    pub backlight: u8,

    /// Raw framebuffer: width * height * bytes_per_pixel bytes.
    framebuffer: Vec<u8>,

    /// Dirty rectangles accumulated since last take_dirty_rects().
    dirty_rects: Vec<DisplayRect>,

    /// Maximum number of dirty rects before collapsing to full-frame.
    max_dirty_rects: usize,
}

impl VirtualDisplay {
    /// Create a new virtual display.
    pub fn new(id: u32, width: u16, height: u16, color_mode: DisplayColorMode) -> Self {
        let bpp = color_mode.bytes_per_pixel();
        let fb_size = width as usize * height as usize * bpp;
        Self {
            id,
            width,
            height,
            color_mode,
            enabled: true,
            backlight: 100,
            framebuffer: vec![0u8; fb_size],
            dirty_rects: Vec::new(),
            max_dirty_rects: 32,
        }
    }

    /// Set a single pixel.
    pub fn set_pixel(&mut self, x: u16, y: u16, color: u32) {
        if x >= self.width || y >= self.height {
            return;
        }
        let bpp = self.color_mode.bytes_per_pixel();
        let offset = (y as usize * self.width as usize + x as usize) * bpp;
        let end = offset + bpp;
        if end > self.framebuffer.len() {
            return;
        }
        match self.color_mode {
            DisplayColorMode::Rgb565 => {
                let c = color as u16;
                self.framebuffer[offset] = ((c >> 8) & 0xFF) as u8;
                self.framebuffer[offset + 1] = (c & 0xFF) as u8;
            }
            DisplayColorMode::Rgb888 => {
                self.framebuffer[offset] = ((color >> 16) & 0xFF) as u8;
                self.framebuffer[offset + 1] = ((color >> 8) & 0xFF) as u8;
                self.framebuffer[offset + 2] = (color & 0xFF) as u8;
            }
            DisplayColorMode::Argb8888 => {
                let bytes = color.to_le_bytes();
                self.framebuffer[offset..end].copy_from_slice(&bytes);
            }
        }
        self.mark_dirty_single(x, y);
    }

    /// Fill a rectangle.
    pub fn fill_rect(&mut self, x: u16, y: u16, w: u16, h: u16, color: u32) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        if x_end <= x || y_end <= y {
            return;
        }
        let bpp = self.color_mode.bytes_per_pixel();
        let row_stride = self.width as usize * bpp;

        for py in y..y_end {
            let row_start = py as usize * row_stride + x as usize * bpp;
            let row_end = row_start + (x_end - x) as usize * bpp;
            if row_end > self.framebuffer.len() {
                break;
            }
            let row_slice = &mut self.framebuffer[row_start..row_end];
            match self.color_mode {
                DisplayColorMode::Rgb565 => {
                    let c = color as u16;
                    let hi = ((c >> 8) & 0xFF) as u8;
                    let lo = (c & 0xFF) as u8;
                    for chunk in row_slice.chunks_exact_mut(2) {
                        chunk[0] = hi;
                        chunk[1] = lo;
                    }
                }
                DisplayColorMode::Rgb888 => {
                    let r = ((color >> 16) & 0xFF) as u8;
                    let g = ((color >> 8) & 0xFF) as u8;
                    let b = (color & 0xFF) as u8;
                    for chunk in row_slice.chunks_exact_mut(3) {
                        chunk[0] = r;
                        chunk[1] = g;
                        chunk[2] = b;
                    }
                }
                DisplayColorMode::Argb8888 => {
                    let bytes = color.to_le_bytes();
                    for chunk in row_slice.chunks_exact_mut(4) {
                        chunk.copy_from_slice(&bytes);
                    }
                }
            }
        }
        self.mark_dirty_rect(x, y, x_end - x, y_end - y);
    }

    /// Draw a bitmap.
    pub fn draw_bitmap(&mut self, x: u16, y: u16, w: u16, h: u16, data: &[u8]) {
        let bpp = self.color_mode.bytes_per_pixel();
        let expected_len = w as usize * h as usize * bpp;
        if data.len() < expected_len {
            return;
        }
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        if x_end <= x || y_end <= y {
            return;
        }
        let row_stride = self.width as usize * bpp;
        let src_row_stride = w as usize * bpp;

        for py in y..y_end {
            let src_offset = (py - y) as usize * src_row_stride;
            let dest_start = py as usize * row_stride + x as usize * bpp;
            let copy_len = (x_end - x) as usize * bpp;
            if dest_start + copy_len <= self.framebuffer.len()
                && src_offset + copy_len <= data.len()
            {
                self.framebuffer[dest_start..dest_start + copy_len]
                    .copy_from_slice(&data[src_offset..src_offset + copy_len]);
            }
        }
        self.mark_dirty_rect(x, y, x_end - x, y_end - y);
    }

    /// Mark a single pixel as dirty (approximated as a 1x1 rect).
    fn mark_dirty_single(&mut self, x: u16, y: u16) {
        self.mark_dirty_rect(x, y, 1, 1);
    }

    /// Mark a rectangular region as dirty. Merges overlapping rects.
    fn mark_dirty_rect(&mut self, x: u16, y: u16, w: u16, h: u16) {
        if w == 0 || h == 0 {
            return;
        }
        // Try to merge with existing rects.
        for existing in &mut self.dirty_rects {
            // Check if rects overlap or are adjacent
            let ex = existing.x;
            let ey = existing.y;
            let ew = existing.w;
            let eh = existing.h;

            let overlap_x = x < ex + ew && x + w > ex;
            let overlap_y = y < ey + eh && y + h > ey;

            if overlap_x && overlap_y {
                // Merge: expand to bounding box
                let new_x = existing.x.min(x);
                let new_y = existing.y.min(y);
                let new_r = (existing.x + existing.w).max(x + w);
                let new_b = (existing.y + existing.h).max(y + h);
                existing.x = new_x;
                existing.y = new_y;
                existing.w = new_r - new_x;
                existing.h = new_b - new_y;
                // Invalidate data (caller must re-extract)
                existing.data_base64.clear();
                return;
            }
        }

        // No merge possible — add new rect.
        if self.dirty_rects.len() < self.max_dirty_rects {
            self.dirty_rects.push(DisplayRect {
                x,
                y,
                w,
                h,
                data_base64: String::new(),
            });
        } else {
            // Too many — collapse to full frame.
            self.dirty_rects.clear();
            self.dirty_rects.push(DisplayRect {
                x: 0,
                y: 0,
                w: self.width,
                h: self.height,
                data_base64: String::new(),
            });
        }
    }

    /// Consume all dirty rects, resetting the dirty list.
    pub fn take_dirty_rects(&mut self) -> Vec<DisplayRect> {
        std::mem::take(&mut self.dirty_rects)
    }

    /// Access the raw framebuffer.
    pub fn framebuffer(&self) -> &[u8] {
        &self.framebuffer
    }
}
