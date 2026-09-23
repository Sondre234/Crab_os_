#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crab_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use bootloader::{BootInfo, entry_point};
use core::panic::PanicInfo;
use crab_os::memory::{self, BootInfoFrameAllocator};
use crab_os::println;
use crab_os::system::SystemInfo;
use crab_os::task::executor::Executor;
use crab_os::task::{Task, keyboard};
use x86_64::VirtAddr;

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static BootInfo) -> ! {
    crab_os::init();
    let phys_mem_offset = VirtAddr::new(boot_info.physical_memory_offset);
    let mut mapper = unsafe { memory::init(phys_mem_offset) };
    let mut frame_allocator = unsafe { BootInfoFrameAllocator::init(&boot_info.memory_map) };
    crab_os::allocator::init_heap(&mut mapper, &mut frame_allocator).expect("heap init failed");

    #[cfg(not(test))]
    if let Err(error) = crab_os::net::init(&mut mapper, &mut frame_allocator, phys_mem_offset) {
        crab_os::serial_println!("Network initialization failed: {}", error);
    }

    #[cfg(test)]
    {
        test_main();
        crab_os::hlt_loop();
    }

    #[cfg(not(test))]
    {
        let info = SystemInfo::new(&boot_info.memory_map);
        let mut executor = Executor::new();
        executor.spawn(Task::new(keyboard::run_shell(info)));
        executor.spawn(Task::new(crab_os::desktop::run_redraw_worker()));
        executor.run();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    #[cfg(test)]
    crab_os::test_panic_handler(info);

    #[cfg(not(test))]
    {
        println!("{}", info);
        crab_os::serial_println!("{}", info);
        crab_os::hlt_loop();
    }
}
