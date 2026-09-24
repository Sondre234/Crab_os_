# CrabOS

A small x86_64 Rust kernel, booted directly by UEFI, with a mouse-driven text
desktop that supports
one or two terminal windows. Each runs **crabsh** with separate history and
scrollback.

## Run

The kernel is a UEFI application (`crab_os.efi`) built with the nightly
toolchain for the custom `x86_64-crab_os.json` target. That target is the stock
`x86_64-unknown-uefi` spec without `singlethread`, so atomics stay real atomics
once more cores are brought up. Running needs QEMU and OVMF firmware
(`edk2-ovmf` on Arch; override the paths with `OVMF_CODE` / `OVMF_VARS`).

~~~sh
cargo run --locked
~~~

`scripts/qemu.sh` is the Cargo runner. It copies the image to a temporary ESP
as `EFI/BOOT/BOOTX64.EFI` and boots a q35 machine with OVMF, COM1 on stdio and
QEMU's emulated Intel e1000 with user networking. Extra arguments are passed to
QEMU. Open a terminal and run `browse http://example.com/` to fetch a page.

To boot a physical PC, copy `target/x86_64-crab_os/debug/crab_os.efi` to
`EFI/BOOT/BOOTX64.EFI` on a FAT32 USB stick and boot it with Secure Boot off.
Keyboard and mouse still need PS/2 or firmware USB legacy emulation.

Click the Terminal desktop icon to open crabsh, then type in QEMU's display
window. Click a terminal to focus it, or drag its title bar to move it. Ctrl+Q
opens another terminal; Ctrl+C or the title-bar [x] closes a terminal. The
desktop starts with no windows open. One terminal uses most of the desktop
width; two share it equally. The guest uses a US keyboard layout, regardless
of the host layout. QEMU's usual Ctrl+Alt+G releases captured input.

## Boot

`src/boot.rs` defines `efi_main` through `entry_point!`. It records the GOP
framebuffer and the ACPI RSDP, exits boot services, copies the memory map,
builds kernel-owned page tables that identity map RAM, the low 4 GiB and the
framebuffer (1 GiB pages when the CPU supports them), then switches to a
512 KiB kernel stack with an unmapped guard page before calling the kernel with
a `BootInfo`. The frame allocator only hands out conventional memory above
1 MiB; boot-services memory is left alone for now.

## Console

The console has an 80×25 display, colors, a visible text cursor, wrapped input,
and a 128-line screen/scrollback ring. Input is bounded to 256 ASCII bytes. History
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
| lspci | Detect an Intel I225-V Ethernet controller |
| net | Show the QEMU e1000 MAC address and link state |
| browse http://host/path | Fetch and show a plain HTTP page as text |

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
CPU identity comes from CPUID, boot-usable RAM from the UEFI memory map,
and uptime from a programmed PIT. The heap figure measures live requested
allocation bytes, excluding allocator metadata, size-class padding, and cached
blocks. Boot-usable RAM is not current free RAM.

The guest identity is a console label, not an authentication system. There is
no filesystem, process loader, or userspace. The desktop is an 80×25 cell
interface drawn into the UEFI framebuffer with an 8×8 bitmap font scaled to fit. In QEMU with the e1000 adapter enabled, `browse` obtains
IPv4 configuration through DHCP, resolves names with DNS, and fetches plain
HTTP pages. It displays text from HTML without images, scripts, CSS, forms,
or links. HTTPS is not supported. The physical Intel I225-V is detected but
does not yet have a driver. Porting upstream Fastfetch needs additional runtime and OS
interfaces.

## Verification

~~~sh
cargo test --locked
cargo check --all-targets --locked
cargo clippy --all-targets --locked
cargo fmt --check
~~~

Kernel tests run inside QEMU and cover the editor, parser, history, display,
keyboard decoding, CPU detection, allocation, boot and exception handling,
including a stack overflow hitting the kernel stack guard page.

If the rolling nightly toolchain leaves incompatible cached metadata, run check
and clippy with --target-dir target/console-check to use a separate build cache.

## Source map

- src/boot.rs: UEFI entry, boot-services exit, page-table and stack handoff.
- src/framebuffer.rs: GOP framebuffer and glyph rendering of console cells.
- src/vga_buffer.rs: screen history, wrapping and desktop cell rendering.
- src/desktop.rs: draggable terminal windows and mouse focus.
- src/shell.rs: bounded line editor, history, command parser and built-ins.
- src/system.rs: boot/CPU facts and the fastfetch display.
- src/task/keyboard.rs: interrupt-fed keyboard stream and layout decoding.
- src/task/executor.rs: async task scheduling and sleeping while idle.
- src/interrupts.rs: keyboard IRQs and the PIT uptime clock.
- src/pci.rs: PCI detection and bus-master configuration.
- src/net/e1000.rs: QEMU e1000 transmit and receive rings.
- src/net/mod.rs: DHCP, DNS and TCP page fetching.
- src/net/web.rs: HTTP URL parsing and HTML-to-text rendering.

Console rendering runs with interrupts briefly disabled. Interrupt handlers
queue keyboard bytes and count timer ticks; they do not print over the prompt.
Scrollback and editor storage are bounded so repeated commands do not consume
unbounded heap memory.
