pub mod e1000;
pub mod web;

use alloc::vec;
use alloc::vec::Vec;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::socket::{dhcpv4, dns, tcp};
use smoltcp::time::Instant;
use smoltcp::wire::{DnsQueryType, EthernetAddress, IpAddress, IpCidr};
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

fn now() -> Instant {
    Instant::from_millis(crate::interrupts::uptime_millis())
}

pub fn fetch(url: &web::Url<'_>) -> Result<Vec<u8>, &'static str> {
    let mut guard = DEVICE.lock();
    let device = guard.as_mut().ok_or("no initialized Ethernet controller")?;
    if !device.link_up() {
        return Err("Ethernet link is down");
    }
    let mut config = Config::new(EthernetAddress(device.mac()).into());
    config.random_seed = crate::interrupts::uptime_millis() as u64;
    let mut interface = Interface::new(config, device, now());
    let mut sockets = SocketSet::new(vec![]);
    let dhcp_handle = sockets.add(dhcpv4::Socket::new());
    let deadline = now().total_millis() + 5000;
    let dns_server = loop {
        interface.poll(now(), device, &mut sockets);
        if let Some(dhcpv4::Event::Configured(config)) =
            sockets.get_mut::<dhcpv4::Socket>(dhcp_handle).poll()
        {
            interface.update_ip_addrs(|addresses| {
                addresses.clear();
                addresses.push(IpCidr::Ipv4(config.address)).unwrap();
            });
            if let Some(router) = config.router {
                interface
                    .routes_mut()
                    .add_default_ipv4_route(router)
                    .map_err(|_| "DHCP route could not be set")?;
            }
            break config.dns_servers.first().copied();
        }
        if now().total_millis() >= deadline {
            return Err("DHCP timed out");
        }
        x86_64::instructions::hlt();
    };
    let remote = if let Ok(address) = url.host.parse::<IpAddress>() {
        address
    } else {
        let dns_server = dns_server.ok_or("DHCP supplied no DNS server")?;
        let dns_socket = dns::Socket::new(&[dns_server.into()], vec![]);
        let handle = sockets.add(dns_socket);
        let query = sockets
            .get_mut::<dns::Socket>(handle)
            .start_query(interface.context(), url.host, DnsQueryType::A)
            .map_err(|_| "DNS query could not start")?;
        let deadline = now().total_millis() + 5000;
        loop {
            interface.poll(now(), device, &mut sockets);
            match sockets
                .get_mut::<dns::Socket>(handle)
                .get_query_result(query)
            {
                Ok(addresses) => {
                    let address = *addresses.first().ok_or("DNS returned no address")?;
                    sockets.remove(handle);
                    break address;
                }
                Err(dns::GetQueryResultError::Pending) => {}
                Err(_) => return Err("DNS lookup failed"),
            }
            if now().total_millis() >= deadline {
                return Err("DNS lookup timed out");
            }
            x86_64::instructions::hlt();
        }
    };
    let socket = tcp::Socket::new(
        tcp::SocketBuffer::new(vec![0; 4096]),
        tcp::SocketBuffer::new(vec![0; 4096]),
    );
    let handle = sockets.add(socket);
    sockets
        .get_mut::<tcp::Socket>(handle)
        .connect(
            interface.context(),
            (remote, url.port),
            49152 + (now().total_millis() as u16 % 10000),
        )
        .map_err(|_| "TCP connection could not start")?;
    let mut response = Vec::new();
    let mut sent = false;
    let deadline = now().total_millis() + 10_000;
    loop {
        interface.poll(now(), device, &mut sockets);
        let socket = sockets.get_mut::<tcp::Socket>(handle);
        if !sent && socket.may_send() {
            let mut request = Vec::new();
            request.extend_from_slice(b"GET ");
            request.extend_from_slice(url.path.as_bytes());
            request.extend_from_slice(b" HTTP/1.0\r\nHost: ");
            request.extend_from_slice(url.host.as_bytes());
            request.extend_from_slice(
                b"\r\nConnection: close\r\nAccept: text/html,text/plain\r\n\r\n",
            );
            socket
                .send_slice(&request)
                .map_err(|_| "HTTP request could not be sent")?;
            sent = true;
        }
        if socket.can_recv() {
            socket
                .recv(|bytes| {
                    let available = 16_384usize.saturating_sub(response.len());
                    response.extend_from_slice(&bytes[..bytes.len().min(available)]);
                    (bytes.len(), ())
                })
                .map_err(|_| "HTTP response could not be read")?;
        }
        if sent && !socket.may_recv() {
            return Ok(response);
        }
        if response.len() == 16_384 {
            return Ok(response);
        }
        if now().total_millis() >= deadline {
            return Err("HTTP request timed out");
        }
        x86_64::instructions::hlt();
    }
}
