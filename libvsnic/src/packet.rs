// Copyright (c) 2026 National Institute of Advanced Industrial Science and Technology (AIST)
// All rights reserved.
//
// This software is released under the MIT License.
// http://opensource.org/licenses/mit-license.php

//! Bounds-checked Ethernet/VLAN/IPv4 TCP/UDP decoding without aligned casts.
use std::net::Ipv4Addr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tuple {
    pub protocol: u8,
    pub source_port: u16,
    pub destination_port: u16,
}
impl Tuple {
    pub fn tenant(self) -> u16 {
        ((self.protocol as u16 ^ self.source_port ^ self.destination_port) & 1) + 1
    }
}
pub struct Parsed<'a> {
    pub tuple: Tuple,
    pub source: Ipv4Addr,
    pub destination: Ipv4Addr,
    pub payload: &'a [u8],
}
fn be16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
pub fn parse(frame: &[u8]) -> Option<Parsed<'_>> {
    let mut kind = be16(frame, 12)?;
    let mut start = 14;
    for _ in 0..2 {
        if kind != 0x8100 && kind != 0x88a8 {
            break;
        }
        kind = be16(frame, start + 2)?;
        start += 4;
    }
    if kind != 0x0800 {
        return None;
    }
    let ip = frame.get(start..)?;
    if *ip.first()? >> 4 != 4 {
        return None;
    }
    let ihl = ((ip[0] & 15) as usize) * 4;
    let total = be16(ip, 2)? as usize;
    if ihl < 20 || total < ihl || total > ip.len() {
        return None;
    }
    // No reassembly: reject both first (MF) and later fragments.
    if be16(ip, 6)? & 0x3fff != 0 {
        return None;
    }
    let protocol = *ip.get(9)?;
    let transport = ip.get(ihl..total)?;
    let payload = match protocol {
        17 => {
            let length = be16(transport, 4)? as usize;
            if length < 8 || length != transport.len() {
                return None;
            }
            transport.get(8..length)?
        }
        6 => {
            let length = ((*transport.get(12)? >> 4) as usize) * 4;
            if length < 20 {
                return None;
            }
            transport.get(length..)?
        }
        _ => return None,
    };
    Some(Parsed {
        tuple: Tuple {
            protocol,
            source_port: be16(transport, 0)?,
            destination_port: be16(transport, 2)?,
        },
        source: Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]),
        destination: Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]),
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn udp() -> Vec<u8> {
        let mut f = vec![0u8; 14 + 20 + 8 + 4];
        f[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
        f[14] = 0x45;
        f[16..18].copy_from_slice(&32u16.to_be_bytes());
        f[23] = 17;
        f[34..36].copy_from_slice(&40000u16.to_be_bytes());
        f[36..38].copy_from_slice(&9001u16.to_be_bytes());
        f[38..40].copy_from_slice(&12u16.to_be_bytes());
        f[42..].copy_from_slice(b"test");
        f
    }
    #[test]
    fn tenants_and_truncated_frames() {
        let mut f = udp();
        assert_eq!(parse(&f).unwrap().tuple.tenant(), 1);
        assert_eq!(parse(&f).unwrap().payload, b"test");
        for n in 0..f.len() {
            assert!(parse(&f[..n]).is_none());
        }
        f[36..38].copy_from_slice(&9000u16.to_be_bytes());
        assert_eq!(parse(&f).unwrap().tuple.tenant(), 2);
        f[20] = 0x20;
        assert!(parse(&f).is_none()); // first fragment with MF
    }
    #[test]
    fn vlan_options_and_invalid_lengths() {
        let mut f = udp();
        f.splice(14..14, [0, 1, 8, 0]);
        f[12..14].copy_from_slice(&0x8100u16.to_be_bytes());
        assert_eq!(parse(&f).unwrap().payload, b"test");
        f[18] = 0x46;
        f.splice(38..38, [0, 0, 0, 0]);
        f[20..22].copy_from_slice(&36u16.to_be_bytes());
        assert!(parse(&f).is_some());
        f[46] = 0xff;
        assert!(parse(&f).is_none());
    }
}
