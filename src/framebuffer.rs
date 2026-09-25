//! Linear framebuffer from UEFI GOP, drawing the 80x25 console cell grid as
//! 8x16 glyphs scaled by the largest integer factor that fits the screen.
use spin::Mutex;
use x86_64::PhysAddr;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelOrder {
    Rgb,
    Bgr,
}

#[derive(Clone, Copy, Debug)]
pub struct FramebufferInfo {
    pub address: PhysAddr,
    pub size: usize,
    pub width: usize,
    pub height: usize,
    /// Pixels per scanline, at least `width`.
    pub stride: usize,
    pub order: PixelOrder,
}

impl FramebufferInfo {
    pub fn valid(&self) -> bool {
        self.address.as_u64() != 0
            && self.address.as_u64() % 4 == 0
            && self.width != 0
            && self.height != 0
            && self.stride >= self.width
            && self
                .stride
                .checked_mul(self.height)
                .and_then(|pixels| pixels.checked_mul(4))
                .is_some_and(|bytes| bytes <= self.size)
    }
}

const COLUMNS: usize = crate::vga_buffer::BUFFER_WIDTH;
const ROWS: usize = crate::vga_buffer::BUFFER_HEIGHT;
const GLYPH_WIDTH: usize = 8;
const GLYPH_HEIGHT: usize = 16;

/// The classic 16-color text-mode palette as 0xRRGGBB.
const PALETTE: [u32; 16] = [
    0x000000, 0x0000aa, 0x00aa00, 0x00aaaa, 0xaa0000, 0xaa00aa, 0xaa5500, 0xaaaaaa, 0x555555,
    0x5555ff, 0x55ff55, 0x55ffff, 0xff5555, 0xff55ff, 0xffff55, 0xffffff,
];

struct Framebuffer {
    info: FramebufferInfo,
    base: *mut u32,
    scale: usize,
    origin_x: usize,
    origin_y: usize,
}

// The pointer is to identity-mapped MMIO owned exclusively by this module.
unsafe impl Send for Framebuffer {}

static FRAMEBUFFER: Mutex<Option<Framebuffer>> = Mutex::new(None);

/// Take ownership of the framebuffer and clear it. Physical memory must be
/// identity mapped.
pub fn init(info: FramebufferInfo) {
    let scale = (info.width / (COLUMNS * GLYPH_WIDTH))
        .min(info.height / (ROWS * GLYPH_HEIGHT))
        .max(1);
    let framebuffer = Framebuffer {
        info,
        base: info.address.as_u64() as *mut u32,
        scale,
        origin_x: info.width.saturating_sub(COLUMNS * GLYPH_WIDTH * scale) / 2,
        origin_y: info.height.saturating_sub(ROWS * GLYPH_HEIGHT * scale) / 2,
    };
    framebuffer.fill(0, 0, info.width, info.height, 0);
    *FRAMEBUFFER.lock() = Some(framebuffer);
}

pub fn available() -> bool {
    FRAMEBUFFER.lock().is_some()
}

/// Draw one console cell; `color` is a text-mode attribute byte.
pub fn draw_cell(column: usize, row: usize, character: u8, color: u8) {
    if let Some(framebuffer) = FRAMEBUFFER.lock().as_ref() {
        framebuffer.draw_cell(column, row, character, color);
    }
}

impl Framebuffer {
    fn encode(&self, rgb: u32) -> u32 {
        match self.info.order {
            PixelOrder::Bgr => rgb,
            PixelOrder::Rgb => (rgb >> 16 & 0xff) | (rgb & 0xff00) | (rgb & 0xff) << 16,
        }
    }

    fn fill(&self, x: usize, y: usize, width: usize, height: usize, rgb: u32) {
        let pixel = self.encode(rgb);
        for row in y..(y + height).min(self.info.height) {
            for column in x..(x + width).min(self.info.width) {
                unsafe {
                    self.base
                        .add(row * self.info.stride + column)
                        .write_volatile(pixel)
                };
            }
        }
    }

    fn draw_cell(&self, column: usize, row: usize, character: u8, color: u8) {
        if column >= COLUMNS || row >= ROWS {
            return;
        }
        let foreground = self.encode(PALETTE[usize::from(color & 0xf)]);
        let background = self.encode(PALETTE[usize::from(color >> 4)]);
        let glyph = glyph(character);
        let left = self.origin_x + column * GLYPH_WIDTH * self.scale;
        let top = self.origin_y + row * GLYPH_HEIGHT * self.scale;
        for y in 0..GLYPH_HEIGHT * self.scale {
            let pixel_y = top + y;
            if pixel_y >= self.info.height {
                break;
            }
            // 8x8 glyph rows are doubled to the 8x16 cell height.
            let bits = glyph[y / self.scale / 2];
            let line = unsafe { self.base.add(pixel_y * self.info.stride) };
            for x in 0..GLYPH_WIDTH * self.scale {
                let pixel_x = left + x;
                if pixel_x >= self.info.width {
                    break;
                }
                let lit = bits & (1 << (x / self.scale)) != 0;
                let pixel = if lit { foreground } else { background };
                unsafe { line.add(pixel_x).write_volatile(pixel) };
            }
        }
    }
}

/// 8x8 glyph rows, least significant bit leftmost. Code page 437 bytes used by
/// the console outside ASCII get hand-drawn shapes.
fn glyph(character: u8) -> [u8; 8] {
    match character {
        0x20..=0x7e => font8x8::legacy::BASIC_LEGACY[usize::from(character)],
        0xfa => [0, 0, 0, 0x18, 0x18, 0, 0, 0],
        0xfe => [0, 0, 0x3c, 0x3c, 0x3c, 0x3c, 0, 0],
        _ => [0; 8],
    }
}
