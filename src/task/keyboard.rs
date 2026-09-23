use crate::shell::Shell;
use crate::system::SystemInfo;
use conquer_once::spin::OnceCell;
use core::pin::Pin;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::task::{Context, Poll};
use crossbeam_queue::ArrayQueue;
use futures_util::Stream;
use futures_util::stream::StreamExt;
use futures_util::task::AtomicWaker;
use pc_keyboard::{
    DecodedKey, HandleControl, KeyCode, Keyboard, KeyboardLayout, Modifiers, ScancodeSet1, layouts,
};

static WAKER: AtomicWaker = AtomicWaker::new();
static SCANCODE_QUEUE: OnceCell<ArrayQueue<u8>> = OnceCell::uninit();
static DROPPED_SCANCODES: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn add_scancode(scancode: u8) {
    if let Ok(queue) = SCANCODE_QUEUE.try_get() {
        if queue.push(scancode).is_err() {
            DROPPED_SCANCODES.fetch_add(1, Ordering::Relaxed);
        }
        WAKER.wake();
    }
}

pub fn dropped_scancodes() -> usize {
    DROPPED_SCANCODES.load(Ordering::Relaxed)
}

pub struct ScancodeStream {
    _private: (),
}

impl ScancodeStream {
    pub fn new() -> Self {
        SCANCODE_QUEUE
            .try_init_once(|| ArrayQueue::new(100))
            .expect("ScancodeStream::new should only be called once");
        ScancodeStream { _private: () }
    }
}

impl Stream for ScancodeStream {
    type Item = u8;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<u8>> {
        let queue = SCANCODE_QUEUE
            .try_get()
            .expect("scancode queue not initialized");

        if let Some(scancode) = queue.pop() {
            return Poll::Ready(Some(scancode));
        }

        WAKER.register(cx.waker());
        match queue.pop() {
            Some(scancode) => {
                WAKER.take();
                Poll::Ready(Some(scancode))
            }
            None => Poll::Pending,
        }
    }
}

/// pc-keyboard 0.7 maps ANSI scancode 0x2b to Oem7, but its US layout
/// only translates Oem5 (the ISO 0x56 key). Accept both for backslash/pipe.
struct ConsoleLayout;

impl KeyboardLayout for ConsoleLayout {
    fn map_keycode(
        &self,
        key: KeyCode,
        modifiers: &Modifiers,
        control: HandleControl,
    ) -> DecodedKey {
        let key = if key == KeyCode::Oem7 {
            KeyCode::Oem5
        } else {
            key
        };
        layouts::Us104Key.map_keycode(key, modifiers, control)
    }
}

fn decoder() -> Keyboard<ConsoleLayout, ScancodeSet1> {
    Keyboard::new(
        ScancodeSet1::new(),
        ConsoleLayout,
        HandleControl::MapLettersToUnicode,
    )
}

pub async fn run_shell(info: SystemInfo) {
    let mut scancodes = ScancodeStream::new();
    let mut keyboard = decoder();
    crate::vga_buffer::set_active_terminal(0);
    crate::vga_buffer::with_writer(|writer| writer.set_columns(36));
    let mut first_shell = Shell::new(info);
    crate::vga_buffer::set_active_terminal(1);
    crate::vga_buffer::with_writer(|writer| writer.set_columns(36));
    let mut second_shell = Shell::new(info);
    crate::vga_buffer::set_active_terminal(0);
    let mut dropped = dropped_scancodes();

    while let Some(scancode) = scancodes.next().await {
        let current_dropped = dropped_scancodes();
        if current_dropped != dropped {
            // A missing break/prefix byte can leave Ctrl/Shift or the decoder
            // latched. Discard the partial line and queued bytes as one unit.
            x86_64::instructions::interrupts::without_interrupts(|| {
                if let Ok(queue) = SCANCODE_QUEUE.try_get() {
                    while queue.pop().is_some() {}
                }
            });
            keyboard = decoder();
            dropped = current_dropped;
            match crate::desktop::active_terminal() {
                0 => first_shell.input_lost(),
                _ => second_shell.input_lost(),
            }
            continue;
        }
        if let Ok(Some(key_event)) = keyboard.add_byte(scancode)
            && let Some(key) = keyboard.process_keyevent(key_event)
        {
            match crate::desktop::active_terminal() {
                0 => first_shell.handle_key(key),
                _ => second_shell.handle_key(key),
            }
        }
    }
}

#[test_case]
fn ansi_backslash_pipe_and_control_keys_decode() {
    let mut keyboard = decoder();
    let mut decode = |scancode| {
        keyboard
            .add_byte(scancode)
            .unwrap()
            .and_then(|event| keyboard.process_keyevent(event))
    };
    assert_eq!(decode(0x2b), Some(DecodedKey::Unicode('\\')));
    decode(0xab);
    decode(0x2a); // left Shift down
    assert_eq!(decode(0x2b), Some(DecodedKey::Unicode('|')));
    decode(0xab);
    decode(0xaa); // Shift up
    decode(0x1d); // left Ctrl down
    assert_eq!(decode(0x2e), Some(DecodedKey::Unicode('\u{3}')));
    decode(0xae);
    decode(0x9d); // Ctrl up
    assert_eq!(decode(0x2e), Some(DecodedKey::Unicode('c')));
}
