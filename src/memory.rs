use core::arch::x86_64::__cpuid;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::mapper::{MapToError, TranslateResult};
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size1GiB,
    Size2MiB, Size4KiB, Translate,
};
use x86_64::{PhysAddr, VirtAddr};

use crate::boot::{MemoryKind, MemoryRegion};
use crate::framebuffer::FramebufferInfo;

const GIB: u64 = 1 << 30;
/// Frames below 1 MiB stay free: page 0 would alias null, and low memory is
/// needed later for the application-processor startup trampoline.
const LOW_MEMORY_END: u64 = 0x10_0000;

pub const KERNEL_STACK_START: u64 = 0x_7777_0000_0000;
pub const KERNEL_STACK_PAGES: u64 = 128; // 512 KiB

pub struct BootInfoFrameAllocator {
    regions: &'static [MemoryRegion],
    next: usize,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// `regions` must describe physical memory accurately, and no other
    /// allocator may hand out the same `Usable` frames.
    pub unsafe fn init(regions: &'static [MemoryRegion]) -> Self {
        BootInfoFrameAllocator { regions, next: 0 }
    }

    fn usable_frames(&self) -> impl Iterator<Item = PhysFrame> {
        self.regions
            .iter()
            .filter(|region| region.kind == MemoryKind::Usable)
            .flat_map(|region| (region.start.max(LOW_MEMORY_END)..region.end()).step_by(4096))
            .map(|address| PhysFrame::containing_address(PhysAddr::new(address)))
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let frame = self.usable_frames().nth(self.next);
        self.next += 1;
        frame
    }
}

pub struct EmptyFrameAllocator;

unsafe impl FrameAllocator<Size4KiB> for EmptyFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        None
    }
}

/// Replace the firmware page tables with kernel-owned ones that identity map
/// all RAM, the low 4 GiB (local APIC, IO-APIC, PCI) and the framebuffer.
///
/// # Safety
/// Must run once, after boot services exit, while the firmware identity map is
/// still active, and with `regions` describing the machine.
pub unsafe fn init(
    regions: &[MemoryRegion],
    framebuffer: Option<&FramebufferInfo>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> OffsetPageTable<'static> {
    let level_4_frame = frame_allocator
        .allocate_frame()
        .expect("no frame for the level 4 page table");
    let level_4_table = unsafe { &mut *(level_4_frame.start_address().as_u64() as *mut PageTable) };
    level_4_table.zero();
    let mut mapper = unsafe { OffsetPageTable::new(level_4_table, VirtAddr::new(0)) };

    // Caching is left write-back; the firmware's MTRRs keep MMIO uncached.
    let ram_end = regions
        .iter()
        .filter(|region| !matches!(region.kind, MemoryKind::Mmio | MemoryKind::Reserved))
        .map(MemoryRegion::end)
        .max()
        .unwrap_or(0);
    identity_map(&mut mapper, frame_allocator, 0, ram_end.max(4 * GIB));
    if let Some(framebuffer) = framebuffer {
        let start = framebuffer.address.as_u64();
        identity_map(
            &mut mapper,
            frame_allocator,
            start,
            start + framebuffer.size as u64,
        );
    }

    let (_, flags) = Cr3::read();
    unsafe { Cr3::write(level_4_frame, flags) };
    mapper
}

fn identity_map(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    start: u64,
    end: u64,
) {
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    let gib_pages = unsafe { __cpuid(0x8000_0000).eax >= 0x8000_0001 }
        && unsafe { __cpuid(0x8000_0001).edx } & (1 << 26) != 0;
    let step = if gib_pages { GIB } else { 2 << 20 };
    let mut address = start & !(step - 1);
    while address < end {
        // Ranges may overlap (framebuffer below 4 GiB); keep the first mapping.
        let page = VirtAddr::new(address);
        let frame = PhysAddr::new(address);
        if gib_pages {
            let page = Page::<Size1GiB>::containing_address(page);
            let frame = PhysFrame::containing_address(frame);
            match unsafe { mapper.map_to(page, frame, flags, frame_allocator) } {
                Ok(flush) => flush.ignore(),
                Err(MapToError::PageAlreadyMapped(_)) => {}
                Err(error) => panic!("identity mapping failed at {address:#x}: {error:?}"),
            }
        } else {
            let page = Page::<Size2MiB>::containing_address(page);
            let frame = PhysFrame::containing_address(frame);
            match unsafe { mapper.map_to(page, frame, flags, frame_allocator) } {
                Ok(flush) => flush.ignore(),
                Err(MapToError::PageAlreadyMapped(_)) => {}
                Err(error) => panic!("identity mapping failed at {address:#x}: {error:?}"),
            }
        }
        address += step;
    }
}

/// Map the kernel stack with an unmapped guard page below it, so an overflow
/// faults instead of silently corrupting memory. Returns the stack top.
pub fn map_kernel_stack(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<VirtAddr, MapToError<Size4KiB>> {
    let guard = VirtAddr::new(KERNEL_STACK_START);
    let bottom = guard + 4096u64;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
    for index in 0..KERNEL_STACK_PAGES {
        let page = Page::containing_address(bottom + index * 4096);
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
    }
    Ok(bottom + KERNEL_STACK_PAGES * 4096)
}

/// # Safety
/// `physical_memory_offset` must be where all physical memory is mapped.
pub unsafe fn translate_addr(addr: VirtAddr, physical_memory_offset: VirtAddr) -> Option<PhysAddr> {
    let (level_4_frame, _) = Cr3::read();
    let virt = physical_memory_offset + level_4_frame.start_address().as_u64();
    let table = unsafe { &mut *virt.as_mut_ptr::<PageTable>() };
    let mapper = unsafe { OffsetPageTable::new(table, physical_memory_offset) };
    match mapper.translate(addr) {
        TranslateResult::Mapped { frame, offset, .. } => Some(frame.start_address() + offset),
        _ => None,
    }
}

pub fn map_mmio(
    physical_start: PhysAddr,
    length: usize,
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<VirtAddr, MapToError<Size4KiB>> {
    use x86_64::structures::paging::PageTableFlags as Flags;

    let physical_page = physical_start.align_down(4096u64);
    let offset = physical_start.as_u64() - physical_page.as_u64();
    let page_count = (offset as usize + length).div_ceil(4096);
    let virtual_start = VirtAddr::new(0x_5555_0000_0000);
    for index in 0..page_count {
        let page = Page::containing_address(virtual_start + index as u64 * 4096);
        let frame = PhysFrame::containing_address(physical_page + index as u64 * 4096);
        let flags = Flags::PRESENT | Flags::WRITABLE | Flags::NO_CACHE | Flags::WRITE_THROUGH;
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
    }
    Ok(virtual_start + offset)
}
