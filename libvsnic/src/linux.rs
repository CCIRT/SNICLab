// Copyright (c) 2026 National Institute of Advanced Industrial Science and Technology (AIST)
// All rights reserved.
//
// This software is released under the MIT License.
// http://opensource.org/licenses/mit-license.php

//! Small Linux syscall boundary. All application logic is Rust.
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

fn cvt(result: libc::c_int) -> io::Result<libc::c_int> {
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

pub fn boot() -> io::Result<()> {
    if std::process::id() != 1 {
        return Err(io::Error::other(
            "guest program must run as PID 1 in its VM",
        ));
    }
    for (target, kind) in [("/proc", "proc"), ("/sys", "sysfs"), ("/dev", "devtmpfs")] {
        let target = CString::new(target).unwrap();
        let kind = CString::new(kind).unwrap();
        let result = unsafe {
            libc::mount(
                kind.as_ptr(),
                target.as_ptr(),
                kind.as_ptr(),
                0,
                std::ptr::null(),
            )
        };
        if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EBUSY) {
            return Err(io::Error::last_os_error());
        }
    }
    let console = OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/console")?;
    for fd in 0..=2 {
        cvt(unsafe { libc::dup2(console.as_raw_fd(), fd) })?;
    }
    Ok(())
}

pub fn idle(result: io::Result<()>) -> ! {
    if let Err(error) = result {
        eprintln!("VSNIC_FATAL: {error}");
    }
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

pub fn wait_for(mut condition: impl FnMut() -> bool, seconds: u64, label: &str) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !condition() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, label.to_owned()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

pub fn interface_with_mac(mac: &str) -> io::Result<String> {
    for entry in fs::read_dir("/sys/class/net")? {
        let entry = entry?;
        if fs::read_to_string(entry.path().join("address"))?
            .trim()
            .eq_ignore_ascii_case(mac)
        {
            return Ok(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("interface MAC {mac}"),
    ))
}

pub fn configure_interface(name: &str, ip: Ipv4Addr) -> io::Result<()> {
    if name.len() >= libc::IFNAMSIZ {
        return Err(io::Error::other("interface name too long"));
    }
    let fd =
        unsafe { OwnedFd::from_raw_fd(cvt(libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0))?) };
    let mut request: libc::ifreq = unsafe { std::mem::zeroed() };
    for (dst, src) in request.ifr_name.iter_mut().zip(name.bytes()) {
        *dst = src as libc::c_char;
    }
    for (address, operation) in [
        (ip, libc::SIOCSIFADDR),
        (Ipv4Addr::new(255, 255, 255, 0), libc::SIOCSIFNETMASK),
    ] {
        let mut sin: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        sin.sin_family = libc::AF_INET as _;
        sin.sin_addr.s_addr = u32::from_ne_bytes(address.octets());
        unsafe {
            std::ptr::copy_nonoverlapping(
                (&sin as *const libc::sockaddr_in).cast::<u8>(),
                (&mut request.ifr_ifru.ifru_addr as *mut libc::sockaddr).cast::<u8>(),
                std::mem::size_of_val(&sin),
            );
            cvt(libc::ioctl(fd.as_raw_fd(), operation as _, &request))?;
        }
    }
    unsafe {
        cvt(libc::ioctl(
            fd.as_raw_fd(),
            libc::SIOCGIFFLAGS as _,
            &mut request,
        ))?;
        request.ifr_ifru.ifru_flags |= libc::IFF_UP as libc::c_short;
        cvt(libc::ioctl(
            fd.as_raw_fd(),
            libc::SIOCSIFFLAGS as _,
            &request,
        ))?;
    }
    Ok(())
}

pub struct Mapping {
    address: NonNull<u8>,
    length: usize,
}
impl Mapping {
    pub fn new(file: &File, length: usize, offset: u64) -> io::Result<Self> {
        if length == 0 || offset > i64::MAX as u64 {
            return Err(io::Error::other("invalid mapping"));
        }
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                offset as libc::off_t,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            address: NonNull::new(address.cast()).expect("non-null mmap"),
            length,
        })
    }
    pub fn len(&self) -> usize {
        self.length
    }
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
    pub fn as_ptr(&self) -> *mut u8 {
        self.address.as_ptr()
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.address.as_ptr().cast(), self.length);
        }
    }
}

pub fn map_ivshmem_pci() -> io::Result<Mapping> {
    for device in fs::read_dir("/sys/bus/pci/devices")? {
        let device = device?.path();
        if fs::read_to_string(device.join("vendor"))?.trim() != "0x1af4"
            || fs::read_to_string(device.join("device"))?.trim() != "0x1110"
        {
            continue;
        }
        // There is no bound driver: enable Memory Space decoding explicitly.
        fs::write(device.join("enable"), b"1")?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(device.join("resource2"))?;
        if file.metadata()?.len() != crate::ring::SIZE as u64 {
            return Err(io::Error::other("unexpected ivshmem BAR2 size"));
        }
        println!("management: ivshmem BAR2 {}", device.display());
        return Mapping::new(&file, crate::ring::SIZE, 0);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "ivshmem PCI 1af4:1110",
    ))
}

pub fn map_ivshmem_gpa() -> io::Result<Mapping> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_SYNC)
        .open("/dev/mem")?;
    Mapping::new(&file, crate::ring::SIZE, crate::ring::GPA)
}

pub fn tenant_from_dtb() -> io::Result<u16> {
    for entry in fs::read_dir("/sys/firmware/devicetree/base/cpus")? {
        let path = entry?.path();
        if fs::read(path.join("device_type")).unwrap_or_default() != b"cpu\0" {
            continue;
        }
        let reg = fs::read(path.join("reg"))?;
        if reg.len() != 4 && reg.len() != 8 {
            continue;
        }
        let n = u32::from_be_bytes(reg[reg.len() - 4..].try_into().unwrap());
        if (1..=crate::ring::TENANTS as u32).contains(&n) {
            return Ok(n as u16);
        }
    }
    Err(io::Error::other(
        "DTB CPU reg is not a configured SNIC tenant",
    ))
}

pub struct PacketSocket(OwnedFd);
impl AsRawFd for PacketSocket {
    fn as_raw_fd(&self) -> i32 {
        self.0.as_raw_fd()
    }
}
impl PacketSocket {
    pub fn new(name: &str) -> io::Result<Self> {
        let name = CString::new(name).map_err(io::Error::other)?;
        let fd = unsafe {
            OwnedFd::from_raw_fd(cvt(libc::socket(
                libc::AF_PACKET,
                libc::SOCK_RAW | libc::SOCK_NONBLOCK,
                (libc::ETH_P_ALL as u16).to_be() as i32,
            ))?)
        };
        let mut address: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        address.sll_family = libc::AF_PACKET as _;
        address.sll_protocol = (libc::ETH_P_ALL as u16).to_be();
        address.sll_ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) } as i32;
        if address.sll_ifindex == 0 {
            return Err(io::Error::last_os_error());
        }
        let ignore: i32 = 1;
        unsafe {
            cvt(libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_PACKET,
                23,
                (&ignore as *const i32).cast(),
                4,
            ))?;
            cvt(libc::bind(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_ll).cast(),
                std::mem::size_of_val(&address) as _,
            ))?;
        }
        Ok(Self(fd))
    }
    pub fn receive(&self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        let result = unsafe {
            libc::recv(
                self.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                libc::MSG_TRUNC,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock
                || error.kind() == io::ErrorKind::Interrupted
            {
                return Ok(None);
            }
            return Err(error);
        }
        // MSG_TRUNC reports the original length so a jumbo frame cannot be
        // silently dispatched as a valid but truncated Ethernet frame.
        Ok(Some(result as usize))
    }
}

pub fn create_shmem(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    crate::ring::initialize_file(&file)
}
