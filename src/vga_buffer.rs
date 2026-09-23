//! VGA text console with bounded scrollback and a hardware cursor.
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicUsize, Ordering};
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

#[derive(Clone, Copy, PartialEq, Eq)]
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
const SCREEN_CELLS: usize = BUFFER_HEIGHT * BUFFER_WIDTH;
pub const SCROLLBACK_LINES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    line: usize,
    column: usize,
}

impl Position {
    fn offset(self, bytes: usize, width: usize) -> Self {
        let offset = self.column + bytes;
        // Preserve pending wrap: a following newline must advance only once.
        if offset > 0 && offset.is_multiple_of(width) {
            Self {
                line: self.line + offset / width - 1,
                column: width,
            }
        } else {
            Self {
                line: self.line + offset / width,
                column: offset % width,
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
    columns: usize,
}

impl Writer {
    const fn new() -> Self {
        Self {
            lines: [[BLANK; BUFFER_WIDTH]; SCROLLBACK_LINES],
            cursor: Position { line: 0, column: 0 },
            last_line: 0,
            first_line: 0,
            view_top: 0,
            color: Color::LightGray as u8,
            columns: BUFFER_WIDTH,
        }
    }
}

// Const initialization keeps both scrollback rings out of the kernel stack.
static TERMINALS: [Mutex<Writer>; 2] = [Mutex::new(Writer::new()), Mutex::new(Writer::new())];
static ACTIVE_TERMINAL: AtomicUsize = AtomicUsize::new(0);
static FRAME: Mutex<[ScreenChar; SCREEN_CELLS]> = Mutex::new([BLANK; SCREEN_CELLS]);
static PREVIOUS_FRAME: Mutex<FrameCache> = Mutex::new(FrameCache {
    cells: [BLANK; SCREEN_CELLS],
    valid: false,
});

struct FrameCache {
    cells: [ScreenChar; SCREEN_CELLS],
    valid: bool,
}

pub fn set_active_terminal(index: usize) {
    ACTIVE_TERMINAL.store(index.min(1), Ordering::Relaxed);
}

pub fn set_terminal_columns(index: usize, columns: usize) {
    interrupts::without_interrupts(|| {
        TERMINALS[index.min(1)].lock().set_columns(columns);
    });
}

pub fn active_terminal() -> usize {
    ACTIVE_TERMINAL.load(Ordering::Relaxed)
}

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

    pub fn set_columns(&mut self, columns: usize) {
        assert!((1..=BUFFER_WIDTH).contains(&columns));
        self.columns = columns;
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
                if self.cursor.column == self.columns {
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
        self.cursor = anchor.offset(cursor, self.columns);
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
}

fn draw_cell(
    frame: &mut [ScreenChar; SCREEN_CELLS],
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
        frame[row * BUFFER_WIDTH + col + offset] = cell;
    }
}

fn draw_window(
    frame: &mut [ScreenChar; SCREEN_CELLS],
    x: usize,
    y: usize,
    width: usize,
    index: usize,
) {
    let title = if index == 0 {
        b" Terminal 1 - crabsh " as &[u8]
    } else {
        b" Terminal 2 - crabsh "
    };
    for col in 0..width {
        draw_cell(frame, y, x + col, b" ", Color::White, Color::DarkGray);
        draw_cell(frame, y + 19, x + col, b" ", Color::White, Color::DarkGray);
    }
    for row in 1..19 {
        draw_cell(frame, y + row, x, b" ", Color::White, Color::DarkGray);
        draw_cell(
            frame,
            y + row,
            x + width - 1,
            b" ",
            Color::White,
            Color::DarkGray,
        );
        for col in 1..width - 1 {
            draw_cell(
                frame,
                y + row,
                x + col,
                b" ",
                Color::LightGray,
                Color::Black,
            );
        }
    }
    let title_bg = if crate::desktop::active_terminal() == index {
        Color::Blue
    } else {
        Color::DarkGray
    };
    draw_cell(frame, y, x, title, Color::White, title_bg);
}

impl fmt::Write for Writer {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.write_string(text);
        Ok(())
    }
}

pub fn with_writer<R>(f: impl FnOnce(&mut Writer) -> R) -> R {
    let result = interrupts::without_interrupts(|| {
        let mut writer = TERMINALS[active_terminal()].lock();
        f(&mut writer)
    });
    render_desktop();
    result
}

fn render_desktop() {
    interrupts::without_interrupts(render_desktop_inner);
}

fn render_desktop_inner() {
    let buffer = 0xb8000 as *mut Volatile<ScreenChar>;
    let mut frame = FRAME.lock();
    frame.fill(BLANK);
    draw_cell(
        &mut frame,
        0,
        0,
        b" CrabOS desktop   Ctrl+Q: open   Ctrl+C: close",
        Color::White,
        Color::Blue,
    );
    draw_cell(
        &mut frame,
        0,
        24,
        b"Mouse: drag title bar; click terminal to focus",
        Color::LightGray,
        Color::DarkGray,
    );
    let windows = crate::desktop::windows();
    let active = crate::desktop::active_terminal();
    for index in [1 - active, active] {
        let Some((x, y, width)) = windows[index] else {
            continue;
        };
        draw_window(&mut frame, x as usize, y as usize, width as usize, index);
        let terminal = TERMINALS[index].lock();
        for row in 0..16 {
            let line = terminal.view_top + row;
            if line <= terminal.last_line {
                for col in 0..(width as usize - 2) {
                    let cell = terminal.lines[line % SCROLLBACK_LINES][col];
                    let screen_index = (y as usize + 2 + row) * BUFFER_WIDTH + x as usize + 1 + col;
                    frame[screen_index] = cell;
                }
            }
        }
    }
    let (pointer_x, pointer_y) = crate::desktop::pointer();
    draw_cell(
        &mut frame,
        pointer_y as usize,
        pointer_x as usize,
        b"+",
        Color::White,
        Color::Blue,
    );
    let mut previous = PREVIOUS_FRAME.lock();
    for index in 0..SCREEN_CELLS {
        if !previous.valid || previous.cells[index] != frame[index] {
            unsafe { (*buffer.add(index)).write(frame[index]) };
            previous.cells[index] = frame[index];
        }
    }
    previous.valid = true;
    unsafe {
        let mut index = Port::<u8>::new(0x3d4);
        let mut data = Port::<u8>::new(0x3d5);
        index.write(0x0a);
        data.write(0x20);
    }
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
