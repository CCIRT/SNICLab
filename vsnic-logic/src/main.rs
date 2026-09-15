// Copyright (c) 2026 National Institute of Advanced Industrial Science and Technology (AIST)
// All rights reserved.
//
// This software is released under the MIT License.
// http://opensource.org/licenses/mit-license.php

use libvsnic::{
    linux, packet,
    ring::{self, Consumer, Direction, Producer},
};
use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::fd::AsRawFd;

fn run() -> io::Result<()> {
    linux::boot()?;
    let map = linux::map_ivshmem_pci()?;
    ring::validate(&map)?;
    let mut sockets = Vec::new();
    let mut names = Vec::new();
    // Bound UDP sinks prevent Linux from replying ICMP port-unreachable.
    let mut sinks = Vec::new();
    for id in 1..=2 {
        let mac = format!("52:54:00:10:0{id}:01");
        linux::wait_for(
            || linux::interface_with_mac(&mac).is_ok(),
            30,
            "management NIC timeout",
        )?;
        let name = linux::interface_with_mac(&mac)?;
        let ip = Ipv4Addr::new(10, 0, id, 1);
        linux::configure_interface(&name, ip)?;
        println!("management: {name} {ip}/24");
        sockets.push(linux::PacketSocket::new(&name)?);
        names.push(name);
        let sink = UdpSocket::bind((ip, if id == 1 { 9001 } else { 9000 }))?;
        sink.set_nonblocking(true)?;
        sinks.push(sink);
    }
    // Exactly one Management thread owns all M2S producers and S2M consumers.
    let mut tx: Vec<_> = (1..=2)
        .map(|id| unsafe { Producer::new(&map, id, Direction::M2s) })
        .collect();
    let mut rx: Vec<_> = (1..=2)
        .map(|id| unsafe { Consumer::new(&map, id, Direction::S2m) })
        .collect();
    for ring in &mut rx {
        ring.publish_ready();
    }
    ring::publish_management_ready(&map);
    println!("management: dispatcher ready (protocol, source-port, destination-port)");
    let mut polls: Vec<_> = sockets
        .iter()
        .map(|s| libc::pollfd {
            fd: s.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let mut data = [0u8; ring::DATA_SIZE];
    loop {
        let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as _, 10) };
        if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Err(io::Error::last_os_error());
        }
        for (index, socket) in sockets.iter().enumerate() {
            // Bounded batches prevent one client from starving the other/S2M.
            for _ in 0..64 {
                let Some(length) = socket.receive(&mut data)? else {
                    break;
                };
                if length > data.len() {
                    eprintln!("management: drop oversize len={length}");
                    continue;
                }
                let Some(parsed) = packet::parse(&data[..length]) else {
                    continue;
                };
                let tenant = parsed.tuple.tenant();
                if tx[tenant as usize - 1].push(&data[..length], 0)? {
                    println!(
                        "management: {} proto={} sport={} dport={} len={length} -> tenant {tenant}",
                        names[index],
                        parsed.tuple.protocol,
                        parsed.tuple.source_port,
                        parsed.tuple.destination_port
                    );
                } else {
                    eprintln!("management: drop full tenant={tenant}");
                }
            }
        }
        for (index, consumer) in rx.iter_mut().enumerate() {
            for _ in 0..64 {
                let Some(packet) = consumer.pop()? else {
                    break;
                };
                println!(
                    "management: S2M tenant={} seq={} len={} flags={}; client forwarding stub",
                    index + 1,
                    packet.sequence,
                    packet.data.len(),
                    packet.flags
                );
            }
        }
        for sink in &sinks {
            for _ in 0..64 {
                if sink.recv_from(&mut data).is_err() {
                    break;
                }
            }
        }
    }
}
fn main() {
    linux::idle(run());
}
