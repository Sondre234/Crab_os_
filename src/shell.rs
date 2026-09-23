//! A bounded, allocation-free command editor and in-kernel command shell.
//! This is not a POSIX shell: commands run in the kernel, without processes.
use core::fmt::Write;
use pc_keyboard::{DecodedKey, KeyCode};

use crate::system::{SystemInfo, VERSION};
use crate::vga_buffer::{self, Color, Position};
use crate::{print, println};

const MAX_INPUT: usize = 256;
const HISTORY_SIZE: usize = 16;
const MAX_ARGS: usize = 32;
const COMMANDS: &[(&str, &str)] = &[
    ("help", "Show commands and keyboard shortcuts"),
    (
        "fastfetch",
        "Show the CrabOS logo and live system information",
    ),
    ("clear", "Clear the screen and scrollback"),
    (
        "echo",
        "Print arguments (quotes and backslash escapes supported)",
    ),
    ("uname", "Show kernel name; -a for version and architecture"),
    ("uptime", "Show time since kernel initialization"),
    (
        "mem",
        "Show boot memory and current heap allocation payloads",
    ),
    ("history", "Show the last 16 commands"),
    ("whoami", "Show the console user"),
    ("hostname", "Show the system name"),
    (
        "lspci",
        "Find the supported Intel I225-V Ethernet controller",
    ),
    ("net", "Show Ethernet controller and link status"),
];

#[derive(Clone, Copy)]
struct Line {
    bytes: [u8; MAX_INPUT],
    len: usize,
    cursor: usize,
    overflow: bool,
}

impl Line {
    const fn new() -> Self {
        Self {
            bytes: [0; MAX_INPUT],
            len: 0,
            cursor: 0,
            overflow: false,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).expect("editor only accepts ASCII")
    }

    fn insert(&mut self, byte: u8) -> bool {
        if !(byte.is_ascii_graphic() || byte == b' ') {
            return false;
        }
        if self.len == MAX_INPUT {
            self.overflow = true;
            return false;
        }
        self.bytes
            .copy_within(self.cursor..self.len, self.cursor + 1);
        self.bytes[self.cursor] = byte;
        self.cursor += 1;
        self.len += 1;
        true
    }

    fn delete(&mut self) {
        if self.cursor < self.len {
            self.bytes
                .copy_within(self.cursor + 1..self.len, self.cursor);
            self.len -= 1;
            self.overflow = false;
        }
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.delete();
        }
    }

    fn delete_word(&mut self) {
        while self.cursor > 0 && self.bytes[self.cursor - 1] == b' ' {
            self.backspace();
        }
        while self.cursor > 0 && self.bytes[self.cursor - 1] != b' ' {
            self.backspace();
        }
    }
}

struct History {
    lines: [Line; HISTORY_SIZE],
    start: usize,
    len: usize,
    browsing: Option<usize>,
    draft: Line,
}

impl History {
    const fn new() -> Self {
        Self {
            lines: [Line::new(); HISTORY_SIZE],
            start: 0,
            len: 0,
            browsing: None,
            draft: Line::new(),
        }
    }

    fn get(&self, index: usize) -> Line {
        self.lines[(self.start + index) % HISTORY_SIZE]
    }

    fn record(&mut self, line: Line) {
        self.browsing = None;
        if line.as_str().trim().is_empty()
            || (self.len > 0 && self.get(self.len - 1).as_str() == line.as_str())
        {
            return;
        }
        if self.len == HISTORY_SIZE {
            self.lines[self.start] = line;
            self.start = (self.start + 1) % HISTORY_SIZE;
        } else {
            self.lines[(self.start + self.len) % HISTORY_SIZE] = line;
            self.len += 1;
        }
    }

    fn previous(&mut self, current: Line) -> Line {
        if self.len == 0 {
            return current;
        }
        let index = match self.browsing {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = current;
                self.len - 1
            }
        };
        self.browsing = Some(index);
        let mut line = self.get(index);
        line.cursor = line.len;
        line
    }

    fn next(&mut self, current: Line) -> Line {
        match self.browsing {
            Some(index) if index + 1 < self.len => {
                self.browsing = Some(index + 1);
                let mut line = self.get(index + 1);
                line.cursor = line.len;
                line
            }
            Some(_) => {
                self.browsing = None;
                self.draft
            }
            None => current,
        }
    }
}

/// Parse quotes and escapes without allocating or passing metacharacters to an
/// external interpreter. Operators such as pipes are deliberately unsupported.
struct Arguments {
    bytes: [u8; MAX_INPUT],
    spans: [(usize, usize); MAX_ARGS],
    len: usize,
}

impl Arguments {
    fn parse(input: &str) -> Result<Self, &'static str> {
        let mut result = Self {
            bytes: [0; MAX_INPUT],
            spans: [(0, 0); MAX_ARGS],
            len: 0,
        };
        if input.len() > MAX_INPUT || !input.is_ascii() {
            return Err("input must be at most 256 ASCII bytes");
        }
        let mut used = 0;
        let mut start = None;
        let mut quote = None;
        let mut escaped = false;
        for byte in input.bytes() {
            if escaped {
                result.bytes[used] = byte;
                used += 1;
                escaped = false;
                continue;
            }
            if byte == b'\\' && quote != Some(b'\'') {
                start.get_or_insert(used);
                escaped = true;
            } else if quote == Some(byte) {
                quote = None;
            } else if quote.is_none() && (byte == b'\'' || byte == b'"') {
                start.get_or_insert(used);
                quote = Some(byte);
            } else if quote.is_none() && byte.is_ascii_whitespace() {
                if let Some(begin) = start.take() {
                    result.push(begin, used)?;
                }
            } else if quote.is_none() && matches!(byte, b'|' | b'>' | b'<' | b';' | b'&') {
                return Err("pipes, redirection and command chaining are not available");
            } else {
                start.get_or_insert(used);
                result.bytes[used] = byte;
                used += 1;
            }
        }
        if escaped {
            return Err("unfinished backslash escape");
        }
        if quote.is_some() {
            return Err("unclosed quote");
        }
        if let Some(begin) = start {
            result.push(begin, used)?;
        }
        Ok(result)
    }

    fn push(&mut self, start: usize, end: usize) -> Result<(), &'static str> {
        if self.len == MAX_ARGS {
            return Err("too many arguments (maximum 32)");
        }
        self.spans[self.len] = (start, end);
        self.len += 1;
        Ok(())
    }

    fn get(&self, index: usize) -> &str {
        assert!(index < self.len);
        let (start, end) = self.spans[index];
        core::str::from_utf8(&self.bytes[start..end]).expect("ASCII arguments")
    }
}

pub struct Shell {
    info: SystemInfo,
    line: Line,
    history: History,
    anchor: Position,
}

impl Shell {
    pub fn new(info: SystemInfo) -> Self {
        vga_buffer::with_writer(|writer| writer.clear());
        info.fastfetch();
        println!("Welcome to CrabOS. Type 'help' to get started.");
        println!("Tab completes commands. Up/Down recalls history. PgUp/PgDn scrolls.\n");
        let anchor = Self::prompt();
        Self {
            info,
            line: Line::new(),
            history: History::new(),
            anchor,
        }
    }

    /// Clear this terminal's old layout and anchor a fresh prompt after resize.
    pub fn reset_for_resize(&mut self) {
        vga_buffer::with_writer(|writer| writer.clear());
        self.line = Line::new();
        self.anchor = Self::prompt();
    }

    fn prompt() -> Position {
        vga_buffer::with_writer(|writer| {
            writer.set_color(Color::LightGreen, Color::Black);
            write!(writer, "shaolin@crabos").unwrap();
            writer.set_color(Color::LightGray, Color::Black);
            write!(writer, ":").unwrap();
            writer.set_color(Color::LightBlue, Color::Black);
            write!(writer, "~").unwrap();
            writer.set_color(Color::LightGray, Color::Black);
            write!(writer, "$ ").unwrap();
            writer.position()
        })
    }

    fn redraw(&self, old_len: usize) {
        vga_buffer::with_writer(|writer| {
            writer.replace_input(self.anchor, self.line.as_str(), self.line.cursor, old_len);
        });
    }

    pub fn handle_key(&mut self, key: DecodedKey) {
        let old_len = self.line.len;
        match key {
            DecodedKey::Unicode('\n' | '\r') => {
                self.line.cursor = self.line.len;
                self.redraw(old_len);
                println!();
                if self.line.overflow {
                    println!("crabsh: input exceeds 256 bytes; command discarded.");
                    self.history.browsing = None;
                } else {
                    let line = self.line;
                    self.history.record(line);
                    self.execute(line.as_str());
                }
                self.line = Line::new();
                self.anchor = Self::prompt();
                return;
            }
            DecodedKey::Unicode('\u{3}') => {
                // Ctrl+C: cancel the current input
                self.line.cursor = self.line.len;
                self.redraw(old_len);
                println!("^C");
                self.line = Line::new();
                self.history.browsing = None;
                self.anchor = Self::prompt();
                return;
            }
            DecodedKey::Unicode('\u{c}') => {
                // Ctrl+L: clear, preserving input
                vga_buffer::with_writer(|writer| writer.clear());
                self.anchor = Self::prompt();
                self.redraw(0);
                return;
            }
            DecodedKey::Unicode('\t') => {
                self.complete();
                return;
            }
            DecodedKey::Unicode('\u{8}') => self.line.backspace(),
            DecodedKey::Unicode('\u{7f}' | '\u{4}') => self.line.delete(),
            DecodedKey::Unicode('\u{1}') | DecodedKey::RawKey(KeyCode::Home) => {
                self.line.cursor = 0
            }
            DecodedKey::Unicode('\u{5}') | DecodedKey::RawKey(KeyCode::End) => {
                self.line.cursor = self.line.len
            }
            DecodedKey::Unicode('\u{2}') | DecodedKey::RawKey(KeyCode::ArrowLeft) => {
                self.line.cursor = self.line.cursor.saturating_sub(1);
            }
            DecodedKey::Unicode('\u{6}') | DecodedKey::RawKey(KeyCode::ArrowRight) => {
                self.line.cursor = (self.line.cursor + 1).min(self.line.len);
            }
            DecodedKey::Unicode('\u{10}') | DecodedKey::RawKey(KeyCode::ArrowUp) => {
                self.line = self.history.previous(self.line);
            }
            DecodedKey::Unicode('\u{e}') | DecodedKey::RawKey(KeyCode::ArrowDown) => {
                self.line = self.history.next(self.line);
            }
            DecodedKey::Unicode('\u{15}') => {
                // Ctrl+U
                self.line
                    .bytes
                    .copy_within(self.line.cursor..self.line.len, 0);
                self.line.len -= self.line.cursor;
                self.line.cursor = 0;
                if self.line.len < old_len {
                    self.line.overflow = false;
                }
            }
            DecodedKey::Unicode('\u{b}') => {
                self.line.len = self.line.cursor; // Ctrl+K
                if self.line.len < old_len {
                    self.line.overflow = false;
                }
            }
            DecodedKey::Unicode('\u{17}') => self.line.delete_word(), // Ctrl+W
            DecodedKey::RawKey(KeyCode::Delete) => self.line.delete(),
            DecodedKey::RawKey(KeyCode::PageUp) => {
                vga_buffer::with_writer(|writer| writer.scroll_up());
                return;
            }
            DecodedKey::RawKey(KeyCode::PageDown) => {
                vga_buffer::with_writer(|writer| writer.scroll_down());
                return;
            }
            DecodedKey::Unicode(character) if character.is_ascii() => {
                self.line.insert(character as u8);
            }
            _ => return,
        }
        self.redraw(old_len);
    }

    pub fn input_lost(&mut self) {
        self.line.cursor = self.line.len;
        self.redraw(self.line.len);
        println!("^C");
        println!("crabsh: keyboard input overflow; line discarded.");
        println!("Release modifier keys before typing again.");
        self.line = Line::new();
        self.history.browsing = None;
        self.anchor = Self::prompt();
    }

    fn complete(&mut self) {
        if self.line.cursor != self.line.len || self.line.as_str().contains(' ') {
            return;
        }
        let prefix = self.line;
        let mut matches = COMMANDS
            .iter()
            .filter(|(name, _)| name.starts_with(prefix.as_str()));
        let Some((first, _)) = matches.next() else {
            return;
        };
        let mut common_len = first.len();
        let mut count = 1;
        for (name, _) in matches {
            count += 1;
            common_len = first
                .bytes()
                .zip(name.bytes())
                .take(common_len)
                .take_while(|(a, b)| a == b)
                .count();
        }
        for byte in first.bytes().take(common_len).skip(prefix.len) {
            self.line.insert(byte);
        }
        if count == 1 {
            self.line.insert(b' ');
        } else if common_len == prefix.len {
            self.line.cursor = self.line.len;
            self.redraw(prefix.len);
            println!();
            for (name, _) in COMMANDS
                .iter()
                .filter(|(name, _)| name.starts_with(prefix.as_str()))
            {
                print!("{name}  ");
            }
            println!();
            self.anchor = Self::prompt();
        }
        self.redraw(prefix.len);
    }

    fn execute(&mut self, input: &str) {
        let args = match Arguments::parse(input) {
            Ok(args) => args,
            Err(error) => {
                println!("crabsh: {error}");
                return;
            }
        };
        if args.len == 0 {
            return;
        }
        let command = args.get(0);
        if args.len == 2
            && args.get(1) == "--help"
            && let Some((name, description)) = COMMANDS.iter().find(|(name, _)| *name == command)
        {
            println!("{name}: {description}");
            return;
        }
        match command {
            "echo" => {
                for index in 1..args.len {
                    if index > 1 {
                        print!(" ");
                    }
                    print!("{}", args.get(index));
                }
                println!();
            }
            "uname" => {
                if args.len == 1 {
                    println!("CrabOS");
                } else if args.len == 2 && args.get(1) == "-a" {
                    println!("CrabOS crabos {VERSION} x86_64");
                } else {
                    println!("usage: uname [-a]");
                }
            }
            "fastfetch" if args.len == 2 && args.get(1) == "--version" => {
                println!("CrabOS native fastfetch {VERSION} (not the upstream program)");
            }
            _ if !COMMANDS.iter().any(|(name, _)| *name == command) => {
                println!("crabsh: {command}: command not found. Try 'help'.");
            }
            _ if args.len > 1 => println!("usage: {command} (no arguments)"),
            "help" => {
                for (name, description) in COMMANDS {
                    println!("  {name:<10} {description}");
                }
                println!("\nEditing: Left/Right, Home/End, Backspace/Delete, Tab completion");
                println!("History: Up/Down or Ctrl+P/N; scrollback: PageUp/PageDown");
                println!("Ctrl+A/E: start/end; Ctrl+U/K: erase to start/end; Ctrl+W: word");
                println!("Ctrl+C: close terminal; Ctrl+Q: open terminal");
                println!("Ctrl+L: clear and redraw; Ctrl+D: delete at cursor");
                println!("US keyboard, ASCII input, 256 bytes/line, 16 history entries.");
                println!("Built-in commands only; no filesystem or external programs yet.");
            }
            "fastfetch" => self.info.fastfetch(),
            "clear" => vga_buffer::with_writer(|writer| writer.clear()),
            "uptime" => {
                let seconds = crate::interrupts::uptime_seconds();
                println!(
                    "up {}h {}m {}s",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                );
            }
            "mem" => {
                println!(
                    "Boot-usable physical RAM: {} KiB",
                    self.info.usable_memory / 1024
                );
                println!(
                    "Kernel heap capacity:     {} KiB",
                    crate::allocator::HEAP_SIZE / 1024
                );
                println!(
                    "Live allocation payload:  {} bytes",
                    crate::allocator::requested_bytes()
                );
                println!("Payload excludes allocator metadata, rounding and cached blocks.");
                println!("Boot-usable RAM is not a measurement of current free memory.");
            }
            "history" => {
                for index in 0..self.history.len {
                    println!("{:>3}  {}", index + 1, self.history.get(index).as_str());
                }
            }
            "whoami" => println!("shaolin"),
            "hostname" => println!("crabos"),
            "lspci" => match crate::pci::find_i225_v() {
                Some(address) => println!(
                    "{:02x}:{:02x}.{} Intel I225-V Ethernet [8086:15f3]",
                    address.bus, address.device, address.function
                ),
                None => println!("Intel I225-V Ethernet [8086:15f3] not found"),
            },
            "net" => match crate::net::status() {
                Some((mac, link)) => println!(
                    "QEMU e1000 {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} link {}",
                    mac[0],
                    mac[1],
                    mac[2],
                    mac[3],
                    mac[4],
                    mac[5],
                    if link { "up" } else { "down" }
                ),
                None => println!("No initialized Ethernet controller"),
            },
            _ => unreachable!(),
        }
    }
}

#[test_case]
fn editor_insertion_deletion_and_capacity() {
    let mut line = Line::new();
    for byte in b"echo ac" {
        assert!(line.insert(*byte));
    }
    line.cursor -= 1;
    line.insert(b'b');
    assert_eq!(line.as_str(), "echo abc");
    line.backspace();
    line.delete();
    assert_eq!(line.as_str(), "echo a");
    line.delete_word();
    assert_eq!(line.as_str(), "echo ");
    line.cursor = 0;
    line.backspace();
    assert_eq!(line.cursor, 0);
    line.cursor = line.len;
    while line.len < MAX_INPUT {
        assert!(line.insert(b'x'));
    }
    assert!(!line.insert(b'x'));
    assert!(line.overflow);
    assert!(!line.insert(0xff));
    line.backspace();
    assert!(!line.overflow);
}

#[test_case]
fn history_eviction_duplicates_and_draft_restore() {
    let mut history = History::new();
    for byte in b'a'..=b'z' {
        let mut line = Line::new();
        line.insert(byte);
        history.record(line);
        history.record(line);
    }
    assert_eq!(history.len, HISTORY_SIZE);
    assert_eq!(history.get(0).as_str(), "k");
    let mut draft = Line::new();
    draft.insert(b'?');
    assert_eq!(history.previous(draft).as_str(), "z");
    assert_eq!(history.previous(draft).as_str(), "y");
    assert_eq!(history.next(draft).as_str(), "z");
    assert_eq!(history.next(draft).as_str(), "?");
    assert!(history.browsing.is_none());
    let mut full_draft = Line::new();
    for _ in 0..=MAX_INPUT {
        full_draft.insert(b'x');
    }
    assert!(full_draft.overflow);
    assert!(!history.previous(full_draft).overflow);
    assert!(history.next(Line::new()).overflow);
}

#[test_case]
fn arguments_preserve_quotes_and_reject_incomplete_input() {
    let args = Arguments::parse(r#" echo "two words" '' a\ b 'x\y' "#).unwrap();
    assert_eq!(args.len, 5);
    assert_eq!(args.get(1), "two words");
    assert_eq!(args.get(2), "");
    assert_eq!(args.get(3), "a b");
    assert_eq!(args.get(4), r"x\y");
    assert!(Arguments::parse("echo 'unfinished").is_err());
    assert!(Arguments::parse("echo \\").is_err());
    assert!(Arguments::parse("echo hi | cat").is_err());
    assert_eq!(Arguments::parse("   ").unwrap().len, 0);
    assert_eq!(Arguments::parse("echo '|'").unwrap().get(1), "|");
    let mut many = [b' '; 66];
    for index in (0..many.len()).step_by(2) {
        many[index] = b'x';
    }
    let many = core::str::from_utf8(&many).unwrap();
    assert_eq!(Arguments::parse(&many[..64]).unwrap().len, MAX_ARGS);
    assert!(Arguments::parse(many).is_err());
}
