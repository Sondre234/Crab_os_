use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

use crate::{gdt, println};
use core::sync::atomic::{AtomicU64, Ordering};
use pic8259::ChainedPics;
use spin;

pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: spin::Mutex<ChainedPics> =
    spin::Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::Mouse.as_u8()].set_handler_fn(mouse_interrupt_handler);

        idt.page_fault.set_handler_fn(page_fault_handler);

        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

const PIT_FREQUENCY: u64 = 1_193_182;
const PIT_DIVISOR: u16 = 11_932;
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Program PIT channel 0 for approximately 100 timer interrupts per second.
pub fn init_timer() {
    use x86_64::instructions::port::Port;
    unsafe {
        Port::<u8>::new(0x43).write(0x36);
        let mut channel = Port::<u8>::new(0x40);
        channel.write(PIT_DIVISOR as u8);
        channel.write((PIT_DIVISOR >> 8) as u8);
    }
}

/// Enable the auxiliary PS/2 port and unmask IRQ12.
pub fn init_mouse() {
    use x86_64::instructions::port::Port;
    unsafe {
        let mut command = Port::<u8>::new(0x64);
        let mut data = Port::<u8>::new(0x60);
        command.write(0xa8); // enable auxiliary device
        command.write(0x20); // read controller configuration
        while command.read() & 1 == 0 {}
        let mut config = data.read();
        config |= 2; // route auxiliary interrupts
        config &= !0x20; // enable auxiliary clock
        command.write(0x60);
        while command.read() & 2 != 0 {}
        data.write(config);
        command.write(0xd4);
        while command.read() & 2 != 0 {}
        data.write(0xf6); // defaults
        while command.read() & 1 == 0 {}
        let _ = data.read();
        command.write(0xd4);
        while command.read() & 2 != 0 {}
        data.write(0xf4); // stream packets
        while command.read() & 1 == 0 {}
        let _ = data.read();
        PICS.lock().write_masks(0b1111_1000, 0b1110_1111);
    }
}

pub fn uptime_seconds() -> u64 {
    let ticks = TICKS.load(Ordering::Relaxed);
    // Splitting the quotient keeps the intermediate product from overflowing.
    ticks / PIT_FREQUENCY * u64::from(PIT_DIVISOR)
        + ticks % PIT_FREQUENCY * u64::from(PIT_DIVISOR) / PIT_FREQUENCY
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("EXCEPTION: BREAKPOINT\n{:?}", stack_frame);
}
#[test_case]
fn test_breakpoint_exception() {
    x86_64::instructions::interrupts::int3();
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    panic!("EXCEPTION: DOUBLE FAULT\n{:?}", stack_frame);
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
    Mouse = PIC_2_OFFSET + 4,
}

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut port = Port::new(0x60);

    let scancode: u8 = unsafe { port.read() };
    crate::task::keyboard::add_scancode(scancode);

    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;
    let byte: u8 = unsafe { Port::<u8>::new(0x60).read() };
    crate::desktop::mouse_byte(byte);
    unsafe {
        PICS.lock()
            .notify_end_of_interrupt(InterruptIndex::Mouse.as_u8());
    }
}

use crate::hlt_loop;
use x86_64::structures::idt::PageFaultErrorCode;

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    println!("EXCEPTION: PAGE FAULT");

    println!("Accessed adress: {:?}", Cr2::read());
    println!("Error Code: {:?}", error_code);
    println!("{:#?}", stack_frame);
    hlt_loop();
}
