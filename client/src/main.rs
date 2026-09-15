use libvsnic::linux;
use std::io;
use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

#[derive(Debug, PartialEq)]
struct Config {
    id: u16,
    packet_count: u64,
    interval: Duration,
}

impl Config {
    fn from_cmdline(cmdline: &str) -> io::Result<Self> {
        fn number(cmdline: &str, key: &str, default: Option<u64>) -> io::Result<u64> {
            let value = cmdline
                .split_whitespace()
                .find_map(|arg| arg.strip_prefix(key));
            match value {
                Some(value) => value.parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("invalid {key}{value}"))
                }),
                None => default.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, format!("missing {key}"))
                }),
            }
        }

        let id = number(cmdline, "client.id=", None)?;
        let packet_count = number(cmdline, "client.packet_count=", Some(0))?;
        let interval_ms = number(cmdline, "client.interval_ms=", Some(1000))?;
        if !(1..=2).contains(&id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "client.id must be 1 or 2",
            ));
        }
        if interval_ms == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "client.interval_ms must be greater than zero",
            ));
        }
        Ok(Self {
            id: id as u16,
            packet_count,
            interval: Duration::from_millis(interval_ms),
        })
    }
}

// Zero means continuous transmission. Injectable I/O keeps the pacing and
// termination tests independent of PID 1, privileged NIC setup, and wall time.
fn transmit(
    config: &Config,
    mut send: impl FnMut(u64) -> io::Result<()>,
    mut wait: impl FnMut(Duration),
) -> io::Result<()> {
    let mut sequence = 1u64;
    loop {
        send(sequence)?;
        if config.packet_count != 0 && sequence == config.packet_count {
            return Ok(());
        }
        sequence = sequence
            .checked_add(1)
            .ok_or_else(|| io::Error::other("packet sequence exhausted"))?;
        wait(config.interval);
    }
}

fn run() -> io::Result<()> {
    linux::boot()?;
    let cmdline = std::fs::read_to_string("/proc/cmdline")?;
    let config = Config::from_cmdline(&cmdline)?;
    let id = config.id;
    let mac = format!("52:54:00:10:0{id}:02");
    linux::wait_for(
        || linux::interface_with_mac(&mac).is_ok(),
        30,
        "client NIC timeout",
    )?;
    let interface = linux::interface_with_mac(&mac)?;
    let ip = Ipv4Addr::new(10, 0, id as u8, 2);
    linux::configure_interface(&interface, ip)?;
    let socket = UdpSocket::bind((ip, 40000))?;
    // Default 3-tuple definition: (IP protocol, source port, destination port).
    let port = if id == 1 { 9001 } else { 9000 };
    println!("client{id}: {interface} {ip}/24; UDP 40000 -> {port}");
    println!(
        "client{id}: packet_count={} (0=continuous), interval_ms={}",
        config.packet_count,
        config.interval.as_millis()
    );
    transmit(
        &config,
        |sequence| {
            let payload = format!("VSNIC client={id} sequence={sequence} expected_tenant={id}");
            socket.send_to(
                payload.as_bytes(),
                (Ipv4Addr::new(10, 0, id as u8, 1), port),
            )?;
            println!("client{id}: sent sequence={sequence}");
            Ok(())
        },
        std::thread::sleep,
    )?;
    println!("client{id}: test complete (TX only; no client RX implementation)");
    Ok(())
}
fn main() {
    linux::idle(run());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_continuous_once_per_second() {
        assert_eq!(
            Config::from_cmdline("console=ttyS0 client.id=1 quiet").unwrap(),
            Config {
                id: 1,
                packet_count: 0,
                interval: Duration::from_secs(1),
            }
        );
    }

    #[test]
    fn finite_transmission_stops_at_configured_count() {
        let config =
            Config::from_cmdline("client.id=2 client.packet_count=3 client.interval_ms=250")
                .unwrap();
        let mut sent = Vec::new();
        let mut waits = Vec::new();
        transmit(
            &config,
            |sequence| {
                sent.push(sequence);
                Ok(())
            },
            |duration| waits.push(duration),
        )
        .unwrap();
        assert_eq!(sent, [1, 2, 3]);
        assert_eq!(waits, [Duration::from_millis(250); 2]);
    }

    #[test]
    fn continuous_transmission_passes_five_and_propagates_send_errors() {
        let config = Config::from_cmdline("client.id=1").unwrap();
        let mut sent = Vec::new();
        let mut waits = Vec::new();
        let result = transmit(
            &config,
            |sequence| {
                if sequence == 9 {
                    return Err(io::Error::from(io::ErrorKind::BrokenPipe));
                }
                sent.push(sequence);
                Ok(())
            },
            |duration| waits.push(duration),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(sent, (1..=8).collect::<Vec<_>>());
        assert_eq!(waits, [Duration::from_secs(1); 8]);
    }

    #[test]
    fn rejects_invalid_options() {
        for cmdline in [
            "",
            "client.id=0",
            "client.id=3",
            "client.id=65537",
            "client.id=one",
            "client.id=1 client.packet_count=-1",
            "client.id=1 client.packet_count=",
            "client.id=1 client.packet_count=18446744073709551616",
            "client.id=1 client.interval_ms=0",
            "client.id=1 client.interval_ms=fast",
        ] {
            assert_eq!(
                Config::from_cmdline(cmdline).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{cmdline}"
            );
        }
    }
}
