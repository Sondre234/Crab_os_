use x86_64::instructions::port::Port;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Address {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Address {
    pub fn read(self, register: u8) -> u32 {
        let address = 0x8000_0000
            | (u32::from(self.bus) << 16)
            | (u32::from(self.device) << 11)
            | (u32::from(self.function) << 8)
            | u32::from(register & 0xfc);
        x86_64::instructions::interrupts::without_interrupts(|| unsafe {
            let mut selector = Port::<u32>::new(0xcf8);
            let mut data = Port::<u32>::new(0xcfc);
            selector.write(address);
            data.read()
        })
    }
}

pub fn find_i225_v() -> Option<Address> {
    for bus in 0..=u8::MAX {
        for device in 0..32 {
            let first = Address {
                bus,
                device,
                function: 0,
            };
            if first.read(0) & 0xffff == 0xffff {
                continue;
            }
            let functions = if first.read(0x0c) & 0x0080_0000 != 0 {
                8
            } else {
                1
            };
            for function in 0..functions {
                let address = Address {
                    bus,
                    device,
                    function,
                };
                if address.read(0) == 0x15f3_8086 {
                    return Some(address);
                }
            }
        }
    }
    None
}
