//! Facts collected from the bootloader and processor, with no guessed hardware.
use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use core::arch::x86_64::__cpuid;
use core::fmt::Write;

use crate::vga_buffer::{self, Color};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy)]
pub struct SystemInfo {
    pub usable_memory: u64,
    vendor: [u8; 12],
    brand: [u8; 48],
    virtualized: bool,
}

impl SystemInfo {
    pub fn new(memory_map: &MemoryMap) -> Self {
        let usable_memory = memory_map
            .iter()
            .filter(|region| region.region_type == MemoryRegionType::Usable)
            .map(|region| region.range.end_addr() - region.range.start_addr())
            .sum();
        // CPUID is available on x86_64; extended leaves are checked first.
        let base = unsafe { __cpuid(0) };
        let mut vendor = [0; 12];
        vendor[0..4].copy_from_slice(&base.ebx.to_le_bytes());
        vendor[4..8].copy_from_slice(&base.edx.to_le_bytes());
        vendor[8..12].copy_from_slice(&base.ecx.to_le_bytes());
        let mut brand = [0; 48];
        if unsafe { __cpuid(0x8000_0000) }.eax >= 0x8000_0004 {
            for index in 0..3 {
                let leaf = unsafe { __cpuid(0x8000_0002 + index as u32) };
                for (word, value) in [leaf.eax, leaf.ebx, leaf.ecx, leaf.edx].iter().enumerate() {
                    let start = index * 16 + word * 4;
                    brand[start..start + 4].copy_from_slice(&value.to_le_bytes());
                }
            }
        }
        Self {
            usable_memory,
            vendor,
            brand,
            virtualized: base.eax >= 1 && unsafe { __cpuid(1) }.ecx & (1 << 31) != 0,
        }
    }

    pub fn cpu_name(&self) -> &str {
        let name = core::str::from_utf8(&self.brand)
            .unwrap_or("")
            .trim_matches('\0')
            .trim();
        if name.is_empty() {
            core::str::from_utf8(&self.vendor).unwrap_or("Unknown x86_64 CPU")
        } else {
            name
        }
    }

    pub fn fastfetch(&self) {
        const LOGO: [&str; 12] = [
            "                    ",
            "     _     _        ",
            "    (v)   (v)       ",
            "     \\_____ /       ",
            "   __/ o o \\__      ",
            "  / _\\_____/ _\\     ",
            "  \\ \\|     |/ /     ",
            "   \\_|_____|_/      ",
            "    / /   \\ \\       ",
            "   /_/     \\_\\      ",
            "                    ",
            "      C R A B       ",
        ];
        vga_buffer::with_writer(|writer| {
            for (index, logo) in LOGO.iter().enumerate() {
                writer.set_color(Color::LightRed, Color::Black);
                write!(writer, "{logo}  ").unwrap();
                writer.set_color(Color::LightCyan, Color::Black);
                let label = match index {
                    0 => "shaolin@crabos",
                    1 => "------------",
                    2 => "OS",
                    3 => "Kernel",
                    4 => "CPU",
                    5 => "Platform",
                    6 => "Uptime",
                    7 => "RAM (boot usable)",
                    8 => "Heap (payload)",
                    9 => "Shell",
                    10 => "Terminal",
                    _ => "",
                };
                write!(writer, "{label}").unwrap();
                if (2..=10).contains(&index) {
                    writer.set_color(Color::LightGray, Color::Black);
                    write!(writer, ": ").unwrap();
                }
                match index {
                    2 => write!(writer, "CrabOS {VERSION} x86_64").unwrap(),
                    3 => write!(writer, "CrabOS-kern").unwrap(),
                    4 => write!(writer, "{:.48}", self.cpu_name()).unwrap(),
                    5 => write!(
                        writer,
                        "{}",
                        if self.virtualized {
                            "Virtual machine"
                        } else {
                            "PC"
                        }
                    )
                    .unwrap(),
                    6 => {
                        let seconds = crate::interrupts::uptime_seconds();
                        write!(
                            writer,
                            "{}h {}m {}s",
                            seconds / 3600,
                            seconds / 60 % 60,
                            seconds % 60
                        )
                        .unwrap();
                    }
                    7 => write!(writer, "{} MiB", self.usable_memory / 1024 / 1024).unwrap(),
                    8 => write!(
                        writer,
                        "{} B / {} KiB",
                        crate::allocator::requested_bytes(),
                        crate::allocator::HEAP_SIZE / 1024
                    )
                    .unwrap(),
                    9 => write!(writer, "crabsh (built-in)").unwrap(),
                    10 => write!(writer, "VGA 80x25 / PS/2").unwrap(),
                    11 => {
                        for color in [
                            Color::Red,
                            Color::Green,
                            Color::Brown,
                            Color::Blue,
                            Color::Magenta,
                            Color::Cyan,
                            Color::LightGray,
                            Color::White,
                        ] {
                            writer.set_color(color, color);
                            write!(writer, "   ").unwrap();
                        }
                    }
                    _ => {}
                }
                writer.set_color(Color::LightGray, Color::Black);
                writeln!(writer).unwrap();
            }
            writeln!(writer).unwrap();
        });
    }
}

#[test_case]
fn cpu_detection_and_empty_memory_map() {
    let info = SystemInfo::new(&MemoryMap::new());
    assert!(!info.cpu_name().is_empty());
    assert_eq!(info.usable_memory, 0);
}
