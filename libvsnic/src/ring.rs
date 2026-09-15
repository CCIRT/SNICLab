// Copyright (c) 2026 National Institute of Advanced Industrial Science and Technology (AIST)
// All rights reserved.
//
// This software is released under the MIT License.
// http://opensource.org/licenses/mit-license.php

//! SPSC queues: release publishes complete slots, acquire observes ownership.
//!
//! No Rust references to shared packet storage are formed. Only naturally
//! aligned 32-bit atomic loads/stores are used (no RMW/exclusive operations).
//! Byte-wise volatile copies also work with ARM Device memory mappings.
use crate::linux::Mapping;
use std::io;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicU32, Ordering};

#[cfg(not(all(target_endian = "little", target_has_atomic = "32")))]
compile_error!("the transport requires little-endian lock-free 32-bit atomics");

pub const SIZE: usize = 8 * 1024 * 1024;
pub const GPA: u64 = 0x3e00_0000;
pub const HEADER: usize = 4096;
pub const STRIDE: usize = 0x101000;
pub const CAPACITY: u32 = 512;
pub const SLOT_SIZE: usize = 2048;
pub const DATA_SIZE: usize = SLOT_SIZE - 16;
pub const TENANTS: u16 = 2;
pub const VERSION: u32 = 2;
pub const MAGIC: u64 = 0x534e494352494e47;
pub const PROCESSED: u16 = 1;
const CONTROL: usize = 192;
const PRODUCER: usize = 0;
const CONSUMER: usize = 64;
const CONSUMER_READY: usize = 128;
const GLOBAL_READY: usize = 36;
const _: () = assert!(CONTROL + CAPACITY as usize * SLOT_SIZE <= STRIDE);
const _: () = assert!(HEADER + TENANTS as usize * 2 * STRIDE <= SIZE);

#[derive(Clone, Copy)]
pub enum Direction {
    M2s = 0,
    S2m = 1,
}

pub struct Packet {
    pub tenant: u16,
    pub flags: u16,
    pub sequence: u64,
    pub data: Vec<u8>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub fn offset(tenant: u16, direction: Direction) -> usize {
    assert!((1..=TENANTS).contains(&tenant));
    HEADER + ((tenant as usize - 1) * 2 + direction as usize) * STRIDE
}

// Every atomic address is page-base + a multiple of four. Mapping is kept
// alive by the owning endpoint's lifetime; peers follow this same ABI.
unsafe fn atomic<'a>(base: *mut u8, offset: usize) -> &'a AtomicU32 {
    &*base.add(offset).cast::<AtomicU32>()
}
unsafe fn read_bytes(base: *mut u8, out: &mut [u8]) {
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = base.add(i).read_volatile();
    }
}
unsafe fn write_bytes(base: *mut u8, input: &[u8]) {
    for (i, byte) in input.iter().enumerate() {
        base.add(i).write_volatile(*byte);
    }
}

/// Called by the host once, before either QEMU process exists. Never reset a
/// live ring: its other endpoint may still own a slot.
pub fn initialize_file(file: &std::fs::File) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.set_len(SIZE as u64)?;
    let zero = [0u8; 4096];
    for pos in (0..SIZE).step_by(zero.len()) {
        file.write_all_at(&zero, pos as u64)?;
    }
    let mut header = [0u8; 40];
    header[..8].copy_from_slice(&MAGIC.to_le_bytes());
    for (i, value) in [
        VERSION,
        TENANTS as u32,
        CAPACITY,
        SLOT_SIZE as u32,
        STRIDE as u32,
        HEADER as u32,
        SIZE as u32,
        0,
    ]
    .iter()
    .enumerate()
    {
        header[8 + i * 4..12 + i * 4].copy_from_slice(&value.to_le_bytes());
    }
    file.write_all_at(&header, 0)?;
    file.sync_all()
}

pub fn validate(map: &Mapping) -> io::Result<()> {
    if map.len() != SIZE {
        return Err(invalid("ivshmem must be exactly 8 MiB"));
    }
    let mut bytes = [0u8; 36];
    unsafe {
        read_bytes(map.as_ptr(), &mut bytes);
    }
    if u64::from_le_bytes(bytes[..8].try_into().unwrap()) != MAGIC {
        return Err(invalid(
            "ivshmem magic mismatch; initialize it before starting VMs",
        ));
    }
    for (i, expected) in [
        VERSION,
        TENANTS as u32,
        CAPACITY,
        SLOT_SIZE as u32,
        STRIDE as u32,
        HEADER as u32,
        SIZE as u32,
    ]
    .iter()
    .enumerate()
    {
        if u32::from_le_bytes(bytes[8 + i * 4..12 + i * 4].try_into().unwrap()) != *expected {
            return Err(invalid("ivshmem ABI mismatch"));
        }
    }
    Ok(())
}

/// Only Management writes this flag; the host initialized it before boot.
pub fn publish_management_ready(map: &Mapping) {
    unsafe {
        atomic(map.as_ptr(), GLOBAL_READY).store(1, Ordering::Release);
    }
}
pub fn management_ready(map: &Mapping) -> bool {
    unsafe { atomic(map.as_ptr(), GLOBAL_READY).load(Ordering::Acquire) == 1 }
}

pub struct Producer<'a> {
    base: *mut u8,
    tenant: u16,
    _map: PhantomData<&'a Mapping>,
}
pub struct Consumer<'a> {
    base: *mut u8,
    tenant: u16,
    _map: PhantomData<&'a Mapping>,
}

impl<'a> Producer<'a> {
    /// # Safety
    /// There must be exactly one producer for this ring across all processes
    /// and VMs. The ABI must already be initialized and remain mapped.
    pub unsafe fn new(map: &'a Mapping, tenant: u16, direction: Direction) -> Self {
        Self {
            base: map.as_ptr().add(offset(tenant, direction)),
            tenant,
            _map: PhantomData,
        }
    }
    pub fn consumer_ready(&self) -> bool {
        unsafe { atomic(self.base, CONSUMER_READY).load(Ordering::Acquire) == 1 }
    }
    /// Returns false on full; never overwrites a slot owned by the consumer.
    pub fn push(&mut self, data: &[u8], flags: u16) -> io::Result<bool> {
        if data.len() > DATA_SIZE {
            return Err(invalid("oversize Ethernet frame"));
        }
        unsafe {
            let head = atomic(self.base, PRODUCER).load(Ordering::Relaxed);
            let tail = atomic(self.base, CONSUMER).load(Ordering::Acquire);
            let used = head.wrapping_sub(tail);
            if used > CAPACITY {
                return Err(invalid("corrupt ring indices"));
            }
            if used == CAPACITY {
                return Ok(false);
            }
            let slot = self
                .base
                .add(CONTROL + (head & (CAPACITY - 1)) as usize * SLOT_SIZE);
            write_bytes(slot, &(data.len() as u32).to_le_bytes());
            write_bytes(slot.add(4), &self.tenant.to_le_bytes());
            write_bytes(slot.add(6), &flags.to_le_bytes());
            write_bytes(slot.add(8), &(head as u64).to_le_bytes());
            write_bytes(slot.add(16), data);
            atomic(self.base, PRODUCER).store(head.wrapping_add(1), Ordering::Release);
        }
        Ok(true)
    }
}

impl<'a> Consumer<'a> {
    /// # Safety
    /// Exactly one consumer may exist for this ring across all processes and
    /// VMs. The producer obeys the SPSC protocol; storage remains mapped.
    pub unsafe fn new(map: &'a Mapping, tenant: u16, direction: Direction) -> Self {
        Self {
            base: map.as_ptr().add(offset(tenant, direction)),
            tenant,
            _map: PhantomData,
        }
    }
    pub fn publish_ready(&mut self) {
        unsafe {
            atomic(self.base, CONSUMER_READY).store(1, Ordering::Release);
        }
    }
    pub fn pop(&mut self) -> io::Result<Option<Packet>> {
        unsafe {
            let tail = atomic(self.base, CONSUMER).load(Ordering::Relaxed);
            let head = atomic(self.base, PRODUCER).load(Ordering::Acquire);
            if head == tail {
                return Ok(None);
            }
            if head.wrapping_sub(tail) > CAPACITY {
                return Err(invalid("corrupt ring indices"));
            }
            let slot = self
                .base
                .add(CONTROL + (tail & (CAPACITY - 1)) as usize * SLOT_SIZE);
            let mut header = [0u8; 16];
            read_bytes(slot, &mut header);
            let length = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
            let tenant = u16::from_le_bytes(header[4..6].try_into().unwrap());
            let flags = u16::from_le_bytes(header[6..8].try_into().unwrap());
            let sequence = u64::from_le_bytes(header[8..].try_into().unwrap());
            if length > DATA_SIZE || tenant != self.tenant || sequence != tail as u64 {
                // Consume the malformed slot without reading outside its boundary.
                atomic(self.base, CONSUMER).store(tail.wrapping_add(1), Ordering::Release);
                return Err(invalid("invalid packet length, tenant, or sequence"));
            }
            let mut data = vec![0u8; length];
            read_bytes(slot.add(16), &mut data);
            atomic(self.base, CONSUMER).store(tail.wrapping_add(1), Ordering::Release);
            Ok(Some(Packet {
                tenant,
                flags,
                sequence,
                data,
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::fd::FromRawFd;
    fn shared_file() -> File {
        let fd = unsafe { libc::memfd_create(c"vsnic-ring-test".as_ptr(), 0) };
        assert!(fd >= 0);
        let file = unsafe { File::from_raw_fd(fd) };
        initialize_file(&file).unwrap();
        file
    }
    #[test]
    fn full_empty_and_u32_wrap() {
        let file = shared_file();
        let map = Mapping::new(&file, SIZE, 0).unwrap();
        validate(&map).unwrap();
        let base = unsafe { map.as_ptr().add(offset(1, Direction::M2s)) };
        unsafe {
            atomic(base, PRODUCER).store(u32::MAX - 10, Ordering::Relaxed);
            atomic(base, CONSUMER).store(u32::MAX - 10, Ordering::Relaxed);
        }
        let mut tx = unsafe { Producer::new(&map, 1, Direction::M2s) };
        let mut rx = unsafe { Consumer::new(&map, 1, Direction::M2s) };
        assert!(rx.pop().unwrap().is_none());
        for n in 0..CAPACITY {
            assert!(tx.push(&n.to_le_bytes(), 0).unwrap());
        }
        assert!(!tx.push(b"overflow", 0).unwrap());
        for n in 0..CAPACITY {
            assert_eq!(rx.pop().unwrap().unwrap().data, n.to_le_bytes());
        }
        assert!(rx.pop().unwrap().is_none());
        assert!(tx.push(b"after wrap", 0).unwrap());
        assert_eq!(rx.pop().unwrap().unwrap().data, b"after wrap");
    }
    #[test]
    fn concurrent_distinct_mappings_preserve_order_and_bytes() {
        let file = shared_file();
        let other = file.try_clone().unwrap();
        let reader = std::thread::spawn(move || {
            let map = Mapping::new(&other, SIZE, 0).unwrap();
            let mut rx = unsafe { Consumer::new(&map, 2, Direction::M2s) };
            for expected in 0..25_000u32 {
                let packet = loop {
                    if let Some(packet) = rx.pop().unwrap() {
                        break packet;
                    }
                    std::thread::yield_now();
                };
                assert_eq!(packet.data, expected.to_le_bytes().repeat(127));
            }
        });
        let map = Mapping::new(&file, SIZE, 0).unwrap();
        let mut tx = unsafe { Producer::new(&map, 2, Direction::M2s) };
        for n in 0..25_000u32 {
            let bytes = n.to_le_bytes().repeat(127);
            while !tx.push(&bytes, 0).unwrap() {
                std::thread::yield_now();
            }
        }
        reader.join().unwrap();
    }
    #[test]
    fn malformed_slot_and_tenant_separation() {
        let file = shared_file();
        let map = Mapping::new(&file, SIZE, 0).unwrap();
        let mut tx = unsafe { Producer::new(&map, 1, Direction::M2s) };
        let mut rx = unsafe { Consumer::new(&map, 1, Direction::M2s) };
        let mut other = unsafe { Consumer::new(&map, 2, Direction::M2s) };
        assert!(tx.push(b"data", 0).unwrap());
        assert!(other.pop().unwrap().is_none());
        unsafe {
            write_bytes(
                map.as_ptr().add(offset(1, Direction::M2s) + CONTROL),
                &u32::MAX.to_le_bytes(),
            );
        }
        assert!(rx.pop().is_err());
        assert!(rx.pop().unwrap().is_none());
        assert!(tx.push(&vec![0; DATA_SIZE + 1], 0).is_err());
    }
}
