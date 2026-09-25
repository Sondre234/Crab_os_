//! UEFI entry and handoff: collect firmware facts, exit boot services, install
//! kernel-owned page tables and stack, then enter the kernel.
use core::ffi::c_void;
use core::mem::MaybeUninit;
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::table::cfg::ConfigTableEntry;
use x86_64::structures::paging::OffsetPageTable;
use x86_64::{PhysAddr, VirtAddr};

use crate::framebuffer::{FramebufferInfo, PixelOrder};
use crate::memory::{self, BootInfoFrameAllocator};
use crate::serial_println;

pub type KernelMain = fn(&'static mut BootInfo) -> !;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// Free after boot services exit and never referenced by the kernel.
    Usable,
    /// Firmware boot-services memory. Still holds the firmware page tables and
    /// stack during handoff; reclaimable once nothing refers to it.
    BootServices,
    /// The kernel image and loader allocations such as the memory map.
    Loader,
    AcpiReclaimable,
    AcpiNvs,
    Mmio,
    Reserved,
}

#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub start: u64,
    pub pages: u64,
    pub kind: MemoryKind,
}

impl MemoryRegion {
    pub fn end(&self) -> u64 {
        self.start + self.pages * 4096
    }
}

pub struct BootInfo {
    pub memory_regions: &'static [MemoryRegion],
    pub framebuffer: Option<FramebufferInfo>,
    /// Physical address of the ACPI RSDP, preferring the ACPI 2.0+ table.
    pub rsdp: Option<PhysAddr>,
    /// Physical memory is identity mapped, so this is zero.
    pub physical_memory_offset: VirtAddr,
    pub mapper: OffsetPageTable<'static>,
    pub frame_allocator: BootInfoFrameAllocator,
}

/// Define the UEFI entry point and hand a `&'static mut BootInfo` to `$path`.
#[macro_export]
macro_rules! entry_point {
    ($path:path) => {
        #[unsafe(export_name = "efi_main")]
        extern "efiapi" fn __crab_os_efi_main(
            image: *mut ::core::ffi::c_void,
            system_table: *const ::core::ffi::c_void,
        ) -> ! {
            const KERNEL: $crate::boot::KernelMain = $path;
            unsafe { $crate::boot::start(image, system_table, KERNEL) }
        }
    };
}

const MAX_REGIONS: usize = 1024;
static mut REGIONS: [MemoryRegion; MAX_REGIONS] = [MemoryRegion {
    start: 0,
    pages: 0,
    kind: MemoryKind::Reserved,
}; MAX_REGIONS];
static mut BOOT_INFO: MaybeUninit<BootInfo> = MaybeUninit::uninit();

/// # Safety
/// Must be called exactly once, as the UEFI entry point, with the firmware's
/// image handle and system table.
pub unsafe fn start(image: *mut c_void, system_table: *const c_void, kernel: KernelMain) -> ! {
    let image = unsafe { uefi::Handle::from_ptr(image) }.expect("null image handle");
    unsafe {
        uefi::boot::set_image_handle(image);
        uefi::table::set_system_table(system_table.cast());
    }
    serial_println!("[crab_os] UEFI entry");

    let framebuffer = discover_framebuffer();
    let rsdp = uefi::system::with_config_table(find_rsdp);

    // No boot services, protocols or UEFI console may be used past this point.
    let map = unsafe { uefi::boot::exit_boot_services(None) };
    x86_64::instructions::interrupts::disable();

    let regions = unsafe { &mut *(&raw mut REGIONS) };
    let mut count = 0;
    for descriptor in map.entries() {
        assert!(count < MAX_REGIONS, "UEFI memory map has too many entries");
        regions[count] = MemoryRegion {
            start: descriptor.phys_start,
            pages: descriptor.page_count,
            kind: kind(descriptor.ty),
        };
        count += 1;
    }
    // The map buffer is LOADER_DATA, which is never handed out; leaking it is fine.
    core::mem::forget(map);
    let regions: &'static [MemoryRegion] = &regions[..count];
    serial_println!("[crab_os] boot services exited; {} memory regions", count);

    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(regions) };
    let mut mapper = unsafe { memory::init(regions, framebuffer.as_ref(), &mut frame_allocator) };
    let stack_top = memory::map_kernel_stack(&mut mapper, &mut frame_allocator)
        .expect("kernel stack mapping failed");

    let boot_info = unsafe { &mut *(&raw mut BOOT_INFO) }.write(BootInfo {
        memory_regions: regions,
        framebuffer,
        rsdp,
        physical_memory_offset: VirtAddr::new(0),
        mapper,
        frame_allocator,
    });

    // The firmware stack lives in boot-services memory; leave it for good.
    unsafe {
        core::arch::asm!(
            "mov rsp, {stack}",
            "xor ebp, ebp",
            "call {enter}",
            "ud2",
            stack = in(reg) stack_top.as_u64(),
            enter = sym enter_kernel,
            in("rdi") boot_info as *mut BootInfo,
            in("rsi") kernel as usize,
            options(noreturn),
        )
    }
}

extern "sysv64" fn enter_kernel(boot_info: *mut BootInfo, kernel: usize) -> ! {
    let kernel: KernelMain = unsafe { core::mem::transmute(kernel) };
    kernel(unsafe { &mut *boot_info })
}

fn discover_framebuffer() -> Option<FramebufferInfo> {
    let handle = uefi::boot::get_handle_for_protocol::<GraphicsOutput>().ok()?;
    let mut gop = uefi::boot::open_protocol_exclusive::<GraphicsOutput>(handle).ok()?;
    let mode = gop.current_mode_info();
    // Bitmask and BLT-only modes are unsupported: BLT is a boot service.
    let order = match mode.pixel_format() {
        PixelFormat::Rgb => PixelOrder::Rgb,
        PixelFormat::Bgr => PixelOrder::Bgr,
        _ => return None,
    };
    let (width, height) = mode.resolution();
    let mut buffer = gop.frame_buffer();
    let info = FramebufferInfo {
        address: PhysAddr::new(buffer.as_mut_ptr() as u64),
        size: buffer.size(),
        width,
        height,
        stride: mode.stride(),
        order,
    };
    info.valid().then_some(info)
}

fn find_rsdp(entries: &[ConfigTableEntry]) -> Option<PhysAddr> {
    [ConfigTableEntry::ACPI2_GUID, ConfigTableEntry::ACPI_GUID]
        .iter()
        .find_map(|guid| entries.iter().find(|entry| entry.guid == *guid))
        .map(|entry| PhysAddr::new(entry.address as u64))
}

fn kind(ty: MemoryType) -> MemoryKind {
    match ty {
        MemoryType::CONVENTIONAL => MemoryKind::Usable,
        MemoryType::BOOT_SERVICES_CODE | MemoryType::BOOT_SERVICES_DATA => MemoryKind::BootServices,
        MemoryType::LOADER_CODE | MemoryType::LOADER_DATA => MemoryKind::Loader,
        MemoryType::ACPI_RECLAIM => MemoryKind::AcpiReclaimable,
        MemoryType::ACPI_NON_VOLATILE => MemoryKind::AcpiNvs,
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => MemoryKind::Mmio,
        _ => MemoryKind::Reserved,
    }
}
