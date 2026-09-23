//! VGA text console with bounded scrollback and a hardware cursor.
use core::fmt::{self, Write};
use spin::Mutex;
use volatile::Volatile;
use x86_64::instructions::{interrupts, port::Port};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    Blue = 1,
    Green = 2,
    Cyan = 3,
    Red = 4,
    Magenta = 5,
    Brown = 6,
    LightGray = 7,
    DarkGray = 8,
    LightBlue = 9,
    LightGreen = 10,
    LightCyan = 11,
    LightRed = 12,
    Pink = 13,
    Yellow = 14,
    White = 15,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct ScreenChar {
    ascii_character: u8,
    color_code: u8,
}

const BLANK: ScreenChar = ScreenChar {
    ascii_character: b' ',
    color_code: Color::LightGray as u8,
};
pub const BUFFER_HEIGHT: usize = 25;
pub const BUFFER_WIDTH: usize = 80;
pub const SCROLLBACK_LINES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    line: usize,
    column: usize,
}

impl Position {
    fn offset(self, bytes: usize) -> Self {
        let offset = self.column + bytes;
        // Preserve pending wrap: a following newline must advance only once.
        if offset > 0 && offset.is_multiple_of(BUFFER_WIDTH) {
            Self {
                line: self.line + offset / BUFFER_WIDTH - 1,
                column: BUFFER_WIDTH,
            }
        } else {
            Self {
                line: self.line + offset / BUFFER_WIDTH,
                column: offset % BUFFER_WIDTH,
            }
        }
    }
}

pub struct Writer {
    lines: [[ScreenChar; BUFFER_WIDTH]; SCROLLBACK_LINES],
    cursor: Position,
    last_line: usize,
    first_line: usize,
    view_top: usize,
    color: u8,
}

// Const initialization keeps the scrollback buffer off the kernel stack/heap.
pub static WRITER: Mutex<Writer> = Mutex::new(Writer {
    lines: [[BLANK; BUFFER_WIDTH]; SCROLLBACK_LINES],
    cursor: Position { line: 0, column: 0 },
    last_line: 0,
    first_line: 0,
    view_top: 0,
    color: Color::LightGray as u8,
});

impl Writer {
    pub fn clear(&mut self) {
        for line in &mut self.lines {
            line.fill(BLANK);
        }
        self.cursor = Position { line: 0, column: 0 };
        self.last_line = 0;
        self.first_line = 0;
        self.view_top = 0;
        self.color = Color::LightGray as u8;
    }

    pub fn set_color(&mut self, foreground: Color, background: Color) {
        self.color = (background as u8) << 4 | foreground as u8;
    }

    pub fn position(&self) -> Position {
        self.cursor
    }

    fn live_top(&self) -> usize {
        self.last_line.saturating_sub(BUFFER_HEIGHT - 1)
    }

    fn ensure_line(&mut self, line: usize) {
        while self.last_line < line {
            self.last_line += 1;
            self.lines[self.last_line % SCROLLBACK_LINES].fill(BLANK);
        }
        self.first_line = self.last_line.saturating_sub(SCROLLBACK_LINES - 1);
        self.view_top = self.live_top();
    }

    pub fn write_byte(&mut self, byte: u8) {
        self.view_top = self.live_top();
        match byte {
            b'\n' => {
                self.cursor.line += 1;
                self.cursor.column = 0;
                self.ensure_line(self.cursor.line);
            }
            b'\r' => self.cursor.column = 0,
            b'\t' => {
                let spaces = 4 - self.cursor.column % 4;
                for _ in 0..spaces {
                    self.write_byte(b' ');
                }
            }
            byte => {
                if self.cursor.column == BUFFER_WIDTH {
                    self.write_byte(b'\n');
                }
                self.lines[self.cursor.line % SCROLLBACK_LINES][self.cursor.column] = ScreenChar {
                    ascii_character: if byte.is_ascii_graphic() || byte == b' ' {
                        byte
                    } else {
                        0xfe
                    },
                    color_code: self.color,
                };
                self.cursor.column += 1;
            }
        }
    }

    pub fn write_string(&mut self, text: &str) {
        for character in text.chars() {
            self.write_byte(if character.is_ascii() {
                character as u8
            } else {
                0xfe
            });
        }
    }

    /// Redraw ASCII input, erasing stale characters after deletions.
    /// The shell limits input to 256 bytes, keeping its anchor in the ring.
    pub fn replace_input(&mut self, anchor: Position, text: &str, cursor: usize, old_len: usize) {
        assert!(anchor.line >= self.first_line);
        assert!(text.is_ascii() && cursor <= text.len());
        self.cursor = anchor;
        self.write_string(text);
        for _ in text.len()..old_len {
            self.write_byte(b' ');
        }
        self.cursor = anchor.offset(cursor);
        self.ensure_line(self.cursor.line);
    }

    pub fn scroll_up(&mut self) {
        self.view_top = self
            .view_top
            .saturating_sub(BUFFER_HEIGHT - 1)
            .max(self.first_line);
    }

    pub fn scroll_down(&mut self) {
        self.view_top = (self.view_top + BUFFER_HEIGHT - 1).min(self.live_top());
    }

    fn render(&self) {
        let buffer = 0xb8000 as *mut Volatile<ScreenChar>;
        for row in 0..BUFFER_HEIGHT {
            for col in 0..BUFFER_WIDTH {
                unsafe { (*buffer.add(row * BUFFER_WIDTH + col)).write(BLANK) };
            }
        }
        draw_cell(
            buffer,
            0,
            0,
            b" CrabOS desktop   [1] Terminal  [2] Terminal",
            Color::White,
            Color::Blue,
        );
        draw_cell(
            buffer,
            0,
            24,
            b"PS/2 mouse: drag a title bar",
            Color::LightGray,
            Color::DarkGray,
        );
        let windows = crate::desktop::windows();
        for (index, (x, y)) in windows.into_iter().enumerate() {
            draw_window(buffer, x as usize, y as usize, index);
        }
        // The primary shell is currently the shared kernel console. Show its
        // visible history in the first panel; the second panel is ready for a
        // separate shell session in a later desktop increment.
        let (x, y) = windows[0];
        for row in 0..16 {
            let line = self.view_top + row;
            if line <= self.last_line {
                for col in 0..36 {
                    let cell = self.lines[line % SCROLLBACK_LINES][col];
                    unsafe {
                        (*buffer.add((y as usize + 2 + row) * BUFFER_WIDTH + x as usize + 1 + col))
                            .write(cell)
                    };
                }
            }
        }
        let (pointer_x, pointer_y) = crate::desktop::pointer();
        draw_cell(
            buffer,
            pointer_y as usize,
            pointer_x as usize,
            b"+",
            Color::White,
            Color::Blue,
        );
        let visible = false;
        unsafe {
            let mut index = Port::<u8>::new(0x3d4);
            let mut data = Port::<u8>::new(0x3d5);
            index.write(0x0a);
            data.write(if visible { 14 } else { 0x20 });
            if visible {
                index.write(0x0b);
                data.write(15);
                let position = ((self.cursor.line - self.view_top) * BUFFER_WIDTH
                    + self.cursor.column.min(BUFFER_WIDTH - 1))
                    as u16;
                index.write(0x0f);
                data.write(position as u8);
                index.write(0x0e);
                data.write((position >> 8) as u8);
            }
        }
    }
}

fn draw_cell(
    buffer: *mut Volatile<ScreenChar>,
    row: usize,
    col: usize,
    text: &[u8],
    fg: Color,
    bg: Color,
) {
    for (offset, byte) in text.iter().copied().enumerate() {
        if col + offset >= BUFFER_WIDTH {
            break;
        }
        let cell = ScreenChar {
            ascii_character: byte,
            color_code: (bg as u8) << 4 | fg as u8,
        };
        unsafe { (*buffer.add(row * BUFFER_WIDTH + col + offset)).write(cell) };
    }
}

fn draw_window(buffer: *mut Volatile<ScreenChar>, x: usize, y: usize, index: usize) {
    let title = if index == 0 {
        b" Terminal 1 - crabsh " as &[u8]
    } else {
        b" Terminal 2 - crabsh "
    };
    for col in 0..38 {
        draw_cell(buffer, y, x + col, b" ", Color::White, Color::DarkGray);
        draw_cell(buffer, y + 19, x + col, b" ", Color::White, Color::DarkGray);
    }
    for row in 1..19 {
        draw_cell(buffer, y + row, x, b" ", Color::White, Color::DarkGray);
        draw_cell(buffer, y + row, x + 37, b" ", Color::White, Color::DarkGray);
        for col in 1..37 {
            draw_cell(
                buffer,
                y + row,
                x + col,
                b" ",
                Color::LightGray,
                Color::Black,
            );
        }
    }
    draw_cell(buffer, y, x, title, Color::White, Color::Blue);
    if index == 1 {
        draw_cell(
            buffer,
            y + 2,
            x + 2,
            b"crabsh terminal",
            Color::LightCyan,
            Color::Black,
        );
        draw_cell(
            buffer,
            y + 4,
            x + 2,
            b"Second shell session",
            Color::LightGray,
            Color::Black,
        );
        draw_cell(
            buffer,
            y + 6,
            x + 2,
            b"is coming next.",
            Color::LightGray,
            Color::Black,
        );
        draw_cell(
            buffer,
            y + 17,
            x + 2,
            b"crabsh$ _",
            Color::LightGreen,
            Color::Black,
        );
    }
}

impl fmt::Write for Writer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_string(text);
        Ok(())
    }
}

pub fn with_writer<R>(f: impl FnOnce(&mut Writer) -> R) -> R {
    interrupts::without_interrupts(|| {
        let mut writer = WRITER.lock();
        let result = f(&mut writer);
        writer.render();
        result
    })
}

/// Redraw the current console and desktop without changing its contents.
pub fn refresh() {
    with_writer(|_| {});
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::vga_buffer::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    with_writer(|writer| writer.write_fmt(args).unwrap());
}

#[test_case]
fn output_wraps_and_scrolls() {
    with_writer(|writer| {
        writer.clear();
        for _ in 0..BUFFER_WIDTH {
            writer.write_byte(b'x');
        }
        writer.write_string("\ny");
        assert_eq!(writer.position(), Position { line: 1, column: 1 });
        for i in 0..200 {
            writeln!(writer, "line {i}").unwrap();
        }
        assert_eq!(writer.first_line, writer.last_line + 1 - SCROLLBACK_LINES);
        for _ in 0..10 {
            writer.scroll_up();
        }
        assert_eq!(writer.view_top, writer.first_line);
        writer.scroll_down();
        assert!(writer.view_top > writer.first_line);
        writer.write_byte(b'z');
        assert_eq!(writer.view_top, writer.live_top());
    });
}

#[test_case]
fn editing_erases_wrapped_text_and_preserves_prompt() {
    with_writer(|writer| {
        writer.clear();
        writer.write_string("crab> ");
        let anchor = writer.position();
        let text = [b'a'; 160];
        writer.replace_input(anchor, core::str::from_utf8(&text).unwrap(), 81, 0);
        assert_eq!(writer.position(), Position { line: 1, column: 7 });
        writer.replace_input(anchor, "hi", 1, 160);
        assert_eq!(writer.lines[0][0].ascii_character, b'c');
        assert_eq!(writer.lines[0][6].ascii_character, b'h');
        assert_eq!(writer.lines[0][8].ascii_character, b' ');
        assert_eq!(writer.lines[2][5].ascii_character, b' ');
        assert_eq!(writer.position(), Position { line: 0, column: 7 });
    });
}

#[test_case]
fn rendered_vga_matches_console() {
    with_writer(|writer| {
        writer.clear();
        writer.write_string("CrabOS");
    });
    let character = unsafe { (0xb8000 as *const ScreenChar).read_volatile() };
    assert_eq!(character.ascii_character, b'C');
}

#[test_case]
fn editing_at_bottom_after_scrollback_ring_wrap() {
    with_writer(|writer| {
        writer.clear();
        for _ in 0..SCROLLBACK_LINES + BUFFER_HEIGHT {
            writer.write_string("old output\n");
        }
        writer.write_string("crab> ");
        let anchor = writer.position();
        let text = [b'x'; 256];
        writer.replace_input(anchor, core::str::from_utf8(&text).unwrap(), 256, 0);
        assert!(writer.first_line > 0);
        assert_eq!(writer.cursor.line, writer.last_line);
        writer.scroll_up();
        writer.replace_input(anchor, "echo ok", 7, 256);
        assert_eq!(writer.view_top, writer.live_top());
        assert_eq!(
            writer.lines[anchor.line % SCROLLBACK_LINES][0].ascii_character,
            b'c'
        );
        assert_eq!(
            writer.lines[(anchor.line + 3) % SCROLLBACK_LINES][0].ascii_character,
            b' '
        );
        writer.write_string("\nok\n");
        assert_eq!(writer.cursor.line, anchor.line + 2);
    });
}
