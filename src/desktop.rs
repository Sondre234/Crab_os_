//! Small text-mode desktop layout and PS/2 mouse window dragging.
//!
//! The current kernel uses VGA text mode, so windows are character-cell panels.
use spin::Mutex;
use x86_64::instructions::interrupts;

const SCREEN_W: i16 = 80;
const SCREEN_H: i16 = 25;
const WINDOW_W: i16 = 38;
const WINDOW_H: i16 = 20;

#[derive(Clone, Copy)]
struct Window {
    x: i16,
    y: i16,
}

struct Desktop {
    windows: [Window; 2],
    drag: Option<(usize, i16, i16)>,
    packet: [u8; 3],
    packet_len: usize,
    pointer_x: i16,
    pointer_y: i16,
    buttons: u8,
}

static DESKTOP: Mutex<Desktop> = Mutex::new(Desktop {
    windows: [Window { x: 1, y: 2 }, Window { x: 41, y: 2 }],
    drag: None,
    packet: [0; 3],
    packet_len: 0,
    pointer_x: 40,
    pointer_y: 12,
    buttons: 0,
});

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
            let dx = desktop.packet[1] as i8 as i16;
            let dy = desktop.packet[2] as i8 as i16;
            desktop.pointer_x = (desktop.pointer_x + dx).clamp(0, SCREEN_W - 1);
            desktop.pointer_y = (desktop.pointer_y - dy).clamp(0, SCREEN_H - 1);
            let pressed = flags & 1 != 0 && desktop.buttons & 1 == 0;
            if pressed {
                let (x, y) = (desktop.pointer_x, desktop.pointer_y);
                desktop.drag = desktop.windows.iter().enumerate().find_map(|(i, w)| {
                    (x >= w.x && x < w.x + WINDOW_W && y == w.y).then_some((i, x - w.x, y - w.y))
                });
            }
            if flags & 1 == 0 {
                desktop.drag = None;
            }
            if let Some((index, offset_x, offset_y)) = desktop.drag {
                let x = desktop.pointer_x - offset_x;
                let y = desktop.pointer_y - offset_y;
                desktop.windows[index].x = x.clamp(0, SCREEN_W - WINDOW_W);
                desktop.windows[index].y = y.clamp(0, SCREEN_H - WINDOW_H);
            }
            desktop.buttons = flags & 7;
            desktop.packet_len = 0;
            drop(desktop);
            crate::vga_buffer::refresh();
        }
    });
}

/// Current text-cell position of the mouse cursor.
pub fn pointer() -> (i16, i16) {
    interrupts::without_interrupts(|| {
        let desktop = DESKTOP.lock();
        (desktop.pointer_x, desktop.pointer_y)
    })
}

/// Snapshot window locations for the VGA renderer.
pub fn windows() -> [(i16, i16); 2] {
    interrupts::without_interrupts(|| {
        let desktop = DESKTOP.lock();
        [
            (desktop.windows[0].x, desktop.windows[0].y),
            (desktop.windows[1].x, desktop.windows[1].y),
        ]
    })
}
