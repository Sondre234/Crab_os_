//! Small text-mode desktop layout and PS/2 mouse window dragging.
//!
//! The current kernel uses VGA text mode, so windows are character-cell panels.
use core::future::poll_fn;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Poll;
use futures_util::task::AtomicWaker;
use spin::Mutex;
use x86_64::instructions::interrupts;

const SCREEN_W: i16 = 80;
const SCREEN_H: i16 = 25;
const WINDOW_W: i16 = 74;
const WINDOW_H: i16 = 20;
const MOUSE_CELLS_PER_COUNT_X: i16 = 8;
const MOUSE_CELLS_PER_COUNT_Y: i16 = 16;

#[derive(Clone, Copy)]
struct Window {
    x: i16,
    y: i16,
}

struct Desktop {
    windows: [Window; 2],
    open: [bool; 2],
    drag: Option<(usize, i16, i16)>,
    packet: [u8; 3],
    packet_len: usize,
    pointer_x: i16,
    pointer_y: i16,
    motion_x: i16,
    motion_y: i16,
    buttons: u8,
    active: usize,
}

static DESKTOP: Mutex<Desktop> = Mutex::new(Desktop {
    windows: [Window { x: 3, y: 4 }, Window { x: 41, y: 4 }],
    open: [false, false],
    drag: None,
    packet: [0; 3],
    packet_len: 0,
    pointer_x: 72,
    pointer_y: 22,
    motion_x: 0,
    motion_y: 0,
    buttons: 0,
    active: 0,
});
static REDRAW_PENDING: AtomicBool = AtomicBool::new(false);
static REDRAW_WAKER: AtomicWaker = AtomicWaker::new();

/// Accept one byte from the PS/2 mouse packet stream.
pub fn mouse_byte(byte: u8) {
    interrupts::without_interrupts(|| {
        let mut desktop = DESKTOP.lock();
        if desktop.packet_len == 0 && byte & 8 == 0 {
            return;
        }
        let len = desktop.packet_len;
        desktop.packet[len] = byte;
        desktop.packet_len += 1;
        if desktop.packet_len == 3 {
            let flags = desktop.packet[0];
            let dx = desktop.packet[1] as i16 - if flags & 0x10 != 0 { 256 } else { 0 };
            let dy = desktop.packet[2] as i16 - if flags & 0x20 != 0 { 256 } else { 0 };
            desktop.motion_x += dx;
            desktop.motion_y += dy;
            let move_x = desktop.motion_x / MOUSE_CELLS_PER_COUNT_X;
            let move_y = desktop.motion_y / MOUSE_CELLS_PER_COUNT_Y;
            desktop.motion_x %= MOUSE_CELLS_PER_COUNT_X;
            desktop.motion_y %= MOUSE_CELLS_PER_COUNT_Y;
            desktop.pointer_x = (desktop.pointer_x + move_x).clamp(0, SCREEN_W - 1);
            desktop.pointer_y = (desktop.pointer_y - move_y).clamp(0, SCREEN_H - 1);
            let pressed = flags & 1 != 0 && desktop.buttons & 1 == 0;
            if pressed {
                let (x, y) = (desktop.pointer_x, desktop.pointer_y);
                let rects = layout(&desktop);
                desktop.drag = None;
                let hit = [desktop.active, desktop.active ^ 1]
                    .into_iter()
                    .find(|&index| {
                        rects[index].is_some_and(|(wx, wy, width)| {
                            x >= wx && x < wx + width && y >= wy && y < wy + WINDOW_H
                        })
                    });
                if let Some(index) = hit {
                    let (wx, wy, width) = rects[index].unwrap();
                    desktop.active = index;
                    if y == wy && x >= wx + width - 4 && x < wx + width - 1 {
                        crate::task::keyboard::desktop_action(
                            crate::task::keyboard::InputEvent::CloseTerminal(index),
                        );
                    } else if y == wy {
                        desktop.drag = Some((index, x - wx, y - wy));
                    }
                } else if (3..=12).contains(&x) && (1..=3).contains(&y) {
                    crate::task::keyboard::desktop_action(
                        crate::task::keyboard::InputEvent::OpenTerminal,
                    );
                }
            }
            if flags & 1 == 0 {
                desktop.drag = None;
            }
            if let Some((index, offset_x, offset_y)) = desktop.drag {
                let x = desktop.pointer_x - offset_x;
                let y = desktop.pointer_y - offset_y;
                let rect = layout(&desktop)[index];
                let width = rect.map_or(38, |(_, _, width)| width);
                desktop.windows[index].x = x.clamp(0, SCREEN_W - width);
                desktop.windows[index].y = y.clamp(1, SCREEN_H - WINDOW_H - 1);
            }
            desktop.buttons = flags & 7;
            desktop.packet_len = 0;
            let active = desktop.active;
            drop(desktop);
            crate::vga_buffer::set_active_terminal(active);
            REDRAW_PENDING.store(true, Ordering::Release);
            REDRAW_WAKER.wake();
        }
    });
}

/// Repaint requests are serviced in task context so IRQ12 stays short.
pub async fn run_redraw_worker() {
    loop {
        poll_fn(|context| {
            REDRAW_WAKER.register(context.waker());
            if REDRAW_PENDING.swap(false, Ordering::AcqRel) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        crate::vga_buffer::refresh();
    }
}

pub fn active_terminal() -> usize {
    interrupts::without_interrupts(|| DESKTOP.lock().active)
}

pub fn is_open(index: usize) -> bool {
    interrupts::without_interrupts(|| DESKTOP.lock().open[index.min(1)])
}

pub fn close_active() {
    close_terminal(active_terminal());
}

pub fn close_terminal(index: usize) {
    interrupts::without_interrupts(|| {
        let mut desktop = DESKTOP.lock();
        let closed = index.min(1);
        desktop.drag = None;
        desktop.open[closed] = false;
        if !desktop.open[closed ^ 1] {
            desktop.active = closed;
        } else {
            desktop.active = closed ^ 1;
        }
    });
    sync_terminal_widths();
    crate::vga_buffer::set_active_terminal(active_terminal());
    crate::vga_buffer::refresh();
}

pub fn open_terminal() -> Option<usize> {
    let opened = interrupts::without_interrupts(|| {
        let mut desktop = DESKTOP.lock();
        let index = desktop.open.iter().position(|open| !open)?;
        if desktop.open.iter().any(|open| *open) {
            desktop.windows = [Window { x: 1, y: 4 }, Window { x: 41, y: 4 }];
        } else {
            desktop.windows[index] = Window { x: 3, y: 4 };
        }
        desktop.drag = None;
        desktop.open[index] = true;
        desktop.active = index;
        Some(index)
    });
    if let Some(index) = opened {
        sync_terminal_widths();
        crate::vga_buffer::set_active_terminal(index);
        crate::vga_buffer::refresh();
    }
    opened
}

/// Current text-cell position of the mouse cursor.
pub fn pointer() -> (i16, i16) {
    interrupts::without_interrupts(|| {
        let desktop = DESKTOP.lock();
        (desktop.pointer_x, desktop.pointer_y)
    })
}

/// Snapshot window locations for the VGA renderer.
pub fn windows() -> [Option<(i16, i16, i16)>; 2] {
    interrupts::without_interrupts(|| {
        let desktop = DESKTOP.lock();
        layout(&desktop)
    })
}

fn layout(desktop: &Desktop) -> [Option<(i16, i16, i16)>; 2] {
    let count = desktop.open.iter().filter(|open| **open).count();
    let width = if count == 1 { WINDOW_W } else { 38 };
    let mut rects = [None; 2];
    if count == 1 {
        let index = desktop.open.iter().position(|open| *open).unwrap();
        rects[index] = Some((
            desktop.windows[index].x.clamp(0, SCREEN_W - width),
            desktop.windows[index].y,
            width,
        ));
    } else if count == 2 {
        for (index, window) in desktop.windows.iter().enumerate() {
            rects[index] = Some((window.x, window.y, width));
        }
    }
    rects
}

fn sync_terminal_widths() {
    let rects = windows();
    for (index, rect) in rects.into_iter().enumerate() {
        crate::vga_buffer::set_terminal_columns(
            index,
            rect.map_or(76, |(_, _, width)| (width - 2) as usize),
        );
    }
}
