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
use std::time::Duration;

fn run() -> io::Result<()> {
    linux::boot()?;
    let tenant = linux::tenant_from_dtb()?;
    println!("snic{tenant}: booted on MilvusVisor");
    let map = linux::map_ivshmem_gpa()?;
    ring::validate(&map)?;
    linux::wait_for(
        || ring::management_ready(&map),
        120,
        "Management readiness timeout",
    )?;
    // This guest's only thread owns this tenant's M2S consumer/S2M producer.
    let mut rx = unsafe { Consumer::new(&map, tenant, Direction::M2s) };
    let mut tx = unsafe { Producer::new(&map, tenant, Direction::S2m) };
    rx.publish_ready();
    println!("snic{tenant}: packet debugger ready");
    loop {
        let Some(received) = rx.pop()? else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        println!(
            "snic{tenant}: RX seq={} len={}",
            received.sequence,
            received.data.len()
        );
        if let Some(parsed) = packet::parse(&received.data) {
            println!(
                "  IPv4 {}:{} -> {}:{} proto={} payload={:?}",
                parsed.source,
                parsed.tuple.source_port,
                parsed.destination,
                parsed.tuple.destination_port,
                parsed.tuple.protocol,
                String::from_utf8_lossy(parsed.payload)
            );
        }
        print!("  hex:");
        for byte in received.data.iter().take(64) {
            print!(" {byte:02x}");
        }
        println!();
        if !tx.push(&received.data, ring::PROCESSED)? {
            eprintln!("snic{tenant}: S2M full");
        }
    }
}
fn main() {
    linux::idle(run());
}
