# SNICLab

## About

SNICLab is an experimental SmartNIC virtualization platform written primarily in Rust.

The project consists of:

- Client VMs that generate network traffic
- A Management VM that performs packet dispatching
- Virtual SmartNIC (SNIC) VMs that process packets
- Shared-memory communication using ivshmem

A common Rust library (`libvsnic`) provides packet parsing, Linux utilities, shared-memory setup, and lock-free SPSC ring buffers.

### Architecture

```text
Client VM 1 ──┐
              ├─ Management VM ── ivshmem ── SNIC VM 1
Client VM 2 ──┘                     │
                                    └─ SNIC VM 2
```

The Management VM and Client VMs run as separate QEMU instances.
Two SNIC guests run under MilvusVisor on AArch64 and communicate with the Management VM through shared memory.

---

## How To Use

### Requirements

- Linux host
- Rust nightly toolchain
- QEMU
- AArch64 cross-compilation tools

### Build and Run

```sh
make run
```

Run validation tests:

```sh
make check
```

Useful commands:

```sh
make build
make run-only
make stop
make test
make lint
```

For additional options:

```sh
make help
```

### Realtime Logs

SNICLab stores logs for all VMs in a per-run session directory.

To monitor activity in real time:

```sh
session_dir=$(sed -n '1p' build/run/current)

tail -n +1 -F \
    "$session_dir/client1.log" \
    "$session_dir/client2.log" \
    "$session_dir/management.log" \
    "$session_dir/snic1.log" \
    "$session_dir/snic2.log"
```

The Management VM logs packet dispatch decisions, while SNIC VMs print packet summaries, payload information, and debugging output.

Press `Ctrl+C` to stop log monitoring. Running VMs continue execution until:

```sh
make stop
```

All logs are preserved across runs and stored under:

```text
build/run/session.XXXXXX/
```


## ivshmem

SNICLab uses QEMU's **ivshmem** device to share memory between the Management VM and the SNIC VMs.

The shared-memory region contains:

- ABI header
- Management ready flag
- Management-to-SNIC (M2S) queues
- SNIC-to-Management (S2M) queues

Each tenant owns a dedicated pair of lock-free SPSC rings:

```text
Management
    ├─ M2S Queue ──► SNIC
    └─ S2M Queue ◄── SNIC
```

Characteristics:

- Shared memory is initialized once by the host before VM startup.
- Queues use lock-free single-producer/single-consumer rings.
- Frames are dropped when a ring becomes full.
- The ABI is fixed to little-endian format.

The Management VM accesses the ivshmem BAR through PCI mmap, while SNIC guests access the shared region through a fixed guest physical address provided by MilvusVisor.

---

## Status

Current implementation includes:

- Packet dispatching in the Management VM
- Shared-memory transport via ivshmem
- SNIC packet debugging and echo-back functionality
- Automated VM launch and testing workflow

Client packet forwarding and full end-to-end networking features are still under development.
