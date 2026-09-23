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

    pub fn write(self, register: u8, value: u32) {
        let address = 0x8000_0000
            | (u32::from(self.bus) << 16)
            | (u32::from(self.device) << 11)
            | (u32::from(self.function) << 8)
            | u32::from(register & 0xfc);
        x86_64::instructions::interrupts::without_interrupts(|| unsafe {
            let mut selector = Port::<u32>::new(0xcf8);
            let mut data = Port::<u32>::new(0xcfc);
            selector.write(address);
            data.write(value);
        });
    }

    pub fn enable_bus_master(self) {
        self.write(0x04, self.read(0x04) | 0x0000_0006);
    }

    pub fn memory_bar0(self) -> Option<u64> {
        let bar = self.read(0x10);
        if bar & 1 != 0 {
            return None;
        }
        let low = u64::from(bar & !0xf);
        match (bar >> 1) & 3 {
            0 => Some(low),
            2 => Some(low | (u64::from(self.read(0x14)) << 32)),
            _ => None,
        }
    }
}

pub fn find(vendor: u16, device_id: u16) -> Option<Address> {
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
                if address.read(0) == ((u32::from(device_id) << 16) | u32::from(vendor)) {
                    return Some(address);
                }
            }
        }
    }
    None
}

pub fn find_i225_v() -> Option<Address> {
    find(0x8086, 0x15f3)
}

pub fn find_qemu_e1000() -> Option<Address> {
    find(0x8086, 0x100e)
}
