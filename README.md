# CrabOS

A small x86_64 Rust kernel with a mouse-driven VGA text desktop that supports
one or two terminal windows. Each runs **crabsh** with separate history and
scrollback.

## Run

The repository uses its existing nightly Rust toolchain, a custom target, the
bootloader 0.9 series, cargo-bootimage, and QEMU. Rust source and LLVM tools must
be available for building the boot image. No dependency changes are needed for
the console.

~~~sh
cargo run --locked
~~~

Type commands in the terminal in QEMU's display window. Click a terminal to
focus it, or drag its title bar to move it. Ctrl+Q opens another terminal;
Ctrl+C closes the focused terminal. One terminal fills the desktop width; two
share it equally. The guest uses a US keyboard layout, regardless of the host
layout. QEMU's usual Ctrl+Alt+G releases captured input.

To build an image without opening a window:

~~~sh
cargo bootimage --locked
~~~

The image is written to
~~~text
target/x86_64-crab_os/debug/bootimage-crab_os.bin
~~~

## Console

The console has an 80×25 display, colors, a hardware cursor, wrapped input, and
a 128-line screen/scrollback ring. Input is bounded to 256 ASCII bytes. History
keeps the last 16 nonempty commands, omitting consecutive duplicates; Down
restores the draft you were typing before browsing history.

Overlong input is rejected on Enter rather than executing a truncated command.
Deleting input makes the remaining bounded line usable again. Keyboard queue
overflow also discards partial input and resets the decoder.

| Keys | Action |
| --- | --- |
| Left / Right | Move within the command |
| Home / End, Ctrl+A / Ctrl+E | Move to start / end |
| Backspace / Delete | Remove before / at cursor |
| Ctrl+D | Delete at cursor |
| Up / Down, Ctrl+P / Ctrl+N | Browse command history |
| Tab | Complete a command; list ambiguous matches |
| PageUp / PageDown | Browse scrollback |
| Ctrl+U / Ctrl+K | Erase to start / end |
| Ctrl+W | Erase the previous word |
| Ctrl+C | Close the focused terminal |
| Ctrl+Q | Open another terminal |
| Ctrl+L | Clear screen/scrollback while preserving the command |

Typing or editing returns to the live display after scrolling. Closing a
terminal keeps its command history for reopening; changing the number of
windows clears displayed output and any unfinished command so text fits the new
width.

## Commands

| Command | Purpose |
| --- | --- |
| fastfetch | Crab logo, kernel version, detected CPU, uptime and memory |
| help | Commands and keyboard shortcuts |
| clear | Clear screen and scrollback |
| echo text | Print arguments |
| uname [-a] | Kernel name, or name/version/architecture |
| uptime | Time since kernel initialization |
| mem | Boot-usable RAM, heap capacity and live allocation payloads |
| history | Recent commands |
| whoami / hostname | Console identity |

Commands accept --help; fastfetch also accepts --version. For example:

~~~text
echo "hello CrabOS"
echo 'a pipe | is literal inside quotes'
echo one\ argument
uname -a
fastfetch
~~~

Single and double quotes group arguments. Backslash escapes the next character
outside single quotes. Unclosed quotes, unfinished escapes, and more than 32
arguments produce an error. Shell expansion, pipes, redirection, and command
chaining are not implemented.

This fastfetch is a CrabOS built-in, not the upstream Fastfetch executable.
CPU identity comes from CPUID, boot-usable RAM from the bootloader memory map,
and uptime from a programmed PIT. The heap figure measures live requested
allocation bytes, excluding allocator metadata, size-class padding, and cached
blocks. Boot-usable RAM is not current free RAM.

The guest identity is a console label, not an authentication system. There is
no filesystem, process loader, userspace, or network stack. The desktop is a
kernel VGA text-mode interface, not a graphical userspace environment. Porting
upstream Fastfetch needs additional runtime and OS interfaces.

## Verification

~~~sh
cargo test --locked
cargo check --all-targets --locked
cargo clippy --all-targets --locked
cargo fmt --check
cargo bootimage --locked
python tests/smoke_console.py --screenshot target/console.ppm
~~~

Kernel tests run inside QEMU and cover the editor, parser, history, display,
keyboard decoding, CPU detection, allocation, boot and exception handling.
The Python smoke test uses only the standard library and drives actual emulated
PS/2 keys, then checks physical VGA memory. It covers boot, editing, quoting,
completion, history, command errors, input bounds, scrolling, and system
commands. Its QEMU uses a temporary disk snapshot and no network, and exits
when the test finishes.

If the rolling nightly toolchain leaves incompatible cached metadata, run check
and clippy with --target-dir target/console-check to use a separate build cache.

## Source map

- src/vga_buffer.rs: screen history, wrapping, rendering and hardware cursor.
- src/desktop.rs: draggable terminal windows and mouse focus.
- src/shell.rs: bounded line editor, history, command parser and built-ins.
- src/system.rs: boot/CPU facts and the fastfetch display.
- src/task/keyboard.rs: interrupt-fed keyboard stream and layout decoding.
- src/task/executor.rs: async task scheduling and sleeping while idle.
- src/interrupts.rs: keyboard IRQs and the PIT uptime clock.

Console rendering runs with interrupts briefly disabled. Interrupt handlers
queue keyboard bytes and count timer ticks; they do not print over the prompt.
Scrollback and editor storage are bounded so repeated commands do not consume
unbounded heap memory.
