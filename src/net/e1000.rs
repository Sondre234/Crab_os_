use alloc::vec;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use x86_64::VirtAddr;

use crate::memory;

const RING_SIZE: usize = 8;
const PACKET_SIZE: usize = 2048;

#[repr(C)]
#[derive(Clone, Copy)]
struct RxDescriptor {
    address: u64,
    length: u16,
    checksum: u16,
    status: u8,
    errors: u8,
    special: u16,
}

impl RxDescriptor {
    const EMPTY: Self = Self {
        address: 0,
        length: 0,
        checksum: 0,
        status: 0,
        errors: 0,
        special: 0,
    };
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TxDescriptor {
    address: u64,
    length: u16,
    checksum_offset: u8,
    command: u8,
    status: u8,
    checksum_start: u8,
    special: u16,
}

impl TxDescriptor {
    const EMPTY: Self = Self {
        address: 0,
        length: 0,
        checksum_offset: 0,
        command: 0,
        status: 1,
        checksum_start: 0,
        special: 0,
    };
}

#[repr(C, align(4096))]
struct Packet([u8; PACKET_SIZE]);

#[repr(C, align(4096))]
struct DmaState {
    rx: [RxDescriptor; RING_SIZE],
    tx: [TxDescriptor; RING_SIZE],
    rx_packets: [Packet; RING_SIZE],
    tx_packets: [Packet; RING_SIZE],
}

struct SharedDma(UnsafeCell<DmaState>);

unsafe impl Sync for SharedDma {}

static DMA: SharedDma = SharedDma(UnsafeCell::new(DmaState {
    rx: [RxDescriptor::EMPTY; RING_SIZE],
    tx: [TxDescriptor::EMPTY; RING_SIZE],
    rx_packets: [const { Packet([0; PACKET_SIZE]) }; RING_SIZE],
    tx_packets: [const { Packet([0; PACKET_SIZE]) }; RING_SIZE],
}));

pub struct E1000 {
    registers: *mut u32,
    dma: *mut DmaState,
    rx_next: usize,
    tx_next: usize,
    mac: [u8; 6],
}

unsafe impl Send for E1000 {}

impl E1000 {
    fn read(&self, register: usize) -> u32 {
        unsafe { read_volatile(self.registers.add(register / 4)) }
    }

    fn write(&self, register: usize, value: u32) {
        unsafe { write_volatile(self.registers.add(register / 4), value) }
    }

    fn physical(pointer: *const u8, physical_memory_offset: VirtAddr) -> Result<u64, &'static str> {
        unsafe { memory::translate_addr(VirtAddr::from_ptr(pointer), physical_memory_offset) }
            .map(|address| address.as_u64())
            .ok_or("DMA address is not mapped")
    }

    pub fn new(
        registers: VirtAddr,
        physical_memory_offset: VirtAddr,
    ) -> Result<Self, &'static str> {
        let mut device = Self {
            registers: registers.as_mut_ptr(),
            dma: DMA.0.get(),
            rx_next: 0,
            tx_next: 0,
            mac: [0; 6],
        };
        device.write(0xd8, u32::MAX);
        device.write(0x0000, device.read(0x0000) | (1 << 26));
        for _ in 0..100_000 {
            if device.read(0x0000) & (1 << 26) == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        if device.read(0x0000) & (1 << 26) != 0 {
            return Err("e1000 reset timed out");
        }
        device.write(0xd8, u32::MAX);
        let low = device.read(0x5400);
        let high = device.read(0x5404);
        if high & (1 << 31) == 0 {
            return Err("e1000 MAC address is invalid");
        }
        device.mac[..4].copy_from_slice(&low.to_le_bytes());
        device.mac[4..].copy_from_slice(&high.to_le_bytes()[..2]);
        device.write(0x0000, device.read(0x0000) | (1 << 6));
        let dma = unsafe { &mut *device.dma };
        for index in 0..RING_SIZE {
            dma.rx[index] = RxDescriptor::EMPTY;
            dma.rx[index].address =
                Self::physical(dma.rx_packets[index].0.as_ptr(), physical_memory_offset)?;
            dma.tx[index] = TxDescriptor::EMPTY;
            dma.tx[index].address =
                Self::physical(dma.tx_packets[index].0.as_ptr(), physical_memory_offset)?;
        }
        let rx_address = Self::physical(dma.rx.as_ptr().cast(), physical_memory_offset)?;
        let tx_address = Self::physical(dma.tx.as_ptr().cast(), physical_memory_offset)?;
        device.write(0x2800, rx_address as u32);
        device.write(0x2804, (rx_address >> 32) as u32);
        device.write(0x2808, (RING_SIZE * size_of::<RxDescriptor>()) as u32);
        device.write(0x2810, 0);
        device.write(0x2818, (RING_SIZE - 1) as u32);
        device.write(0x3800, tx_address as u32);
        device.write(0x3804, (tx_address >> 32) as u32);
        device.write(0x3808, (RING_SIZE * size_of::<TxDescriptor>()) as u32);
        device.write(0x3810, 0);
        device.write(0x3818, 0);
        device.write(0x0410, 0);
        device.write(0x0400, 2 | (1 << 3) | (15 << 4) | (64 << 12));
        device.write(0x0100, 2 | (1 << 15) | (1 << 26));
        Ok(device)
    }

    pub fn mac(&self) -> [u8; 6] {
        self.mac
    }

    pub fn link_up(&self) -> bool {
        self.read(0x0008) & 2 != 0
    }

    fn receive_packet(&mut self) -> Option<Vec<u8>> {
        let dma = unsafe { &mut *self.dma };
        let index = self.rx_next;
        let status = unsafe { read_volatile(&dma.rx[index].status) };
        if status & 1 == 0 {
            return None;
        }
        fence(Ordering::Acquire);
        let length = dma.rx[index].length as usize;
        let packet = if status & 2 != 0 && dma.rx[index].errors == 0 && length <= PACKET_SIZE {
            Some(dma.rx_packets[index].0[..length].to_vec())
        } else {
            None
        };
        dma.rx[index].status = 0;
        fence(Ordering::Release);
        self.write(0x2818, index as u32);
        self.rx_next = (index + 1) % RING_SIZE;
        packet
    }

    fn send_packet(&mut self, packet: &[u8]) {
        if packet.len() > PACKET_SIZE {
            return;
        }
        let dma = unsafe { &mut *self.dma };
        let index = self.tx_next;
        if unsafe { read_volatile(&dma.tx[index].status) } & 1 == 0 {
            return;
        }
        dma.tx_packets[index].0[..packet.len()].copy_from_slice(packet);
        dma.tx[index].length = packet.len() as u16;
        dma.tx[index].command = 1 | 2 | 8;
        dma.tx[index].status = 0;
        fence(Ordering::Release);
        self.tx_next = (index + 1) % RING_SIZE;
        self.write(0x3818, self.tx_next as u32);
    }
}

pub struct ReceiveToken(Vec<u8>);

impl RxToken for ReceiveToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

pub struct TransmitToken<'a>(&'a mut E1000);

impl TxToken for TransmitToken<'_> {
    fn consume<R, F>(self, length: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut packet = vec![0; length];
        let result = f(&mut packet);
        self.0.send_packet(&packet);
        result
    }
}

impl Device for E1000 {
    type RxToken<'a> = ReceiveToken;
    type TxToken<'a> = TransmitToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let packet = self.receive_packet()?;
        Some((ReceiveToken(packet), TransmitToken(self)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TransmitToken(self))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ethernet;
        capabilities.max_transmission_unit = 1514;
        capabilities
    }
}
