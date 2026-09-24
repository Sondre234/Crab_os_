#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(crab_os::test_runner)]
#![reexport_test_harness_main = "test_main"]

use core::panic::PanicInfo;
use crab_os::boot::BootInfo;
use crab_os::system::SystemInfo;
use crab_os::task::executor::Executor;
use crab_os::task::{Task, keyboard};
use crab_os::{entry_point, println};

entry_point!(kernel_main);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    crab_os::init(boot_info);
    let BootInfo {
        mapper,
        frame_allocator,
        physical_memory_offset,
        ..
    } = boot_info;
    crab_os::allocator::init_heap(mapper, frame_allocator).expect("heap init failed");

    #[cfg(not(test))]
    if let Err(error) = crab_os::net::init(mapper, frame_allocator, *physical_memory_offset) {
        crab_os::serial_println!("Network initialization failed: {}", error);
    }

    #[cfg(test)]
    {
        test_main();
        crab_os::hlt_loop();
    }

    #[cfg(not(test))]
    {
        let info = SystemInfo::new(boot_info.memory_regions);
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
