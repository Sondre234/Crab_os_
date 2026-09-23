pub mod e1000;

use spin::Mutex;
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

use crate::pci;

static DEVICE: Mutex<Option<e1000::E1000>> = Mutex::new(None);

pub fn init(
    mapper: &mut OffsetPageTable,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
    physical_memory_offset: VirtAddr,
) -> Result<(), &'static str> {
    let Some(address) = pci::find_qemu_e1000() else {
        return Ok(());
    };
    let bar = address
        .memory_bar0()
        .filter(|bar| *bar != 0)
        .ok_or("e1000 BAR0 is missing")?;
    let registers = crate::memory::map_mmio(PhysAddr::new(bar), 0x20_000, mapper, frame_allocator)
        .map_err(|_| "e1000 MMIO mapping failed")?;
    address.enable_bus_master();
    *DEVICE.lock() = Some(e1000::E1000::new(registers, physical_memory_offset)?);
    Ok(())
}

pub fn status() -> Option<([u8; 6], bool)> {
    DEVICE
        .lock()
        .as_ref()
        .map(|device| (device.mac(), device.link_up()))
}
