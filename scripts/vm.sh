#!/bin/sh
# Implementation helper: invoke through the root Makefile.
# Subshell functions isolate launch/check/stop state and cleanup traps.
set -eu
: "${PROJECT_ROOT:?Use make run, make check, or make stop}"
: "${BUILD_ROOT:?Use the root Makefile}"
SCRIPT_DIR="$PROJECT_ROOT/scripts"

log() { printf '[vsnic] %s\n' "$*"; }
die() { printf '[vsnic] error: %s\n' "$*" >&2; exit 1; }
require_cmd() { command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"; }

find_aarch64_efi()
{
    if [ -n "${AARCH64_EFI:-}" ]; then
        [ -f "$AARCH64_EFI" ] || die "AARCH64_EFI does not exist: $AARCH64_EFI"
        printf '%s\n' "$AARCH64_EFI"
        return
    fi
    for candidate in \
        /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
        /usr/share/AAVMF/AAVMF_CODE.fd
    do
        if [ -f "$candidate" ]; then
            printf '%s\n' "$candidate"
            return
        fi
    done
    die "AArch64 UEFI firmware not found; set AARCH64_EFI"
}

run_client() (
    client_id=${1:-1}
    if [ "$#" -gt 0 ]; then
        shift
    fi
    case "$client_id" in
        1) port=$CLIENT1_PORT; mac=52:54:00:10:01:02 ;;
        2) port=$CLIENT2_PORT; mac=52:54:00:10:02:02 ;;
        *) die "client id must be 1 or 2" ;;
    esac

    require_cmd "$QEMU_X86_64"
    case "$NETWORK_MODE" in
        socket) netdev="socket,id=clientnet,connect=127.0.0.1:$port" ;;
        bridge) netdev="tap,id=clientnet,ifname=vsnic-c$client_id,script=no,downscript=no" ;;
        *) die "NETWORK_MODE must be socket or bridge" ;;
    esac
    exec "$QEMU_X86_64" \
        -machine "q35,accel=$QEMU_ACCEL" \
        -name "${VM_NAME:-vsnic-client$client_id}" \
        -cpu max -m 256M -smp 1 -display none -monitor none \
        -serial "${VM_SERIAL:-mon:stdio}" -no-reboot \
        -kernel "$BUILD_ROOT/client/bzImage" \
        -initrd "$BUILD_ROOT/client/initramfs.cpio" \
        -append "console=ttyS0 panic=-1 quiet client.id=$client_id client.packet_count=$CLIENT_PACKET_COUNT client.interval_ms=$CLIENT_INTERVAL_MS" \
        -netdev "$netdev" \
        -device "virtio-net-pci,netdev=clientnet,mac=$mac" \
        "$@"
)

run_management() (
    require_cmd "$QEMU_X86_64"
    SHMEM_FILE="${SHMEM_FILE:-$BUILD_ROOT/run/snic.ivshmem}"
    [ -f "$SHMEM_FILE" ] || die "shared-memory file does not exist: $SHMEM_FILE"
    case "$NETWORK_MODE" in
        socket)
            net1="socket,id=client1,listen=127.0.0.1:$CLIENT1_PORT"
            net2="socket,id=client2,listen=127.0.0.1:$CLIENT2_PORT" ;;
        bridge)
            net1="tap,id=client1,ifname=vsnic-m1,script=no,downscript=no"
            net2="tap,id=client2,ifname=vsnic-m2,script=no,downscript=no" ;;
        *) die "NETWORK_MODE must be socket or bridge" ;;
    esac

    exec "$QEMU_X86_64" \
        -machine "q35,accel=$QEMU_ACCEL" \
        -name "${VM_NAME:-vsnic-management}" \
        -cpu max -m 384M -smp 1 -display none -monitor none \
        -serial "${VM_SERIAL:-mon:stdio}" -no-reboot \
        -kernel "$BUILD_ROOT/vsnic-logic/bzImage" \
        -initrd "$BUILD_ROOT/vsnic-logic/initramfs.cpio" \
        -append "console=ttyS0 panic=-1 quiet vsnic.role=management" \
        -object "memory-backend-file,id=snicmem,size=$SNIC_SHMEM_BYTES,mem-path=$SHMEM_FILE,share=on" \
        -device "ivshmem-plain,memdev=snicmem,addr=03.0,rombar=0" \
        -netdev "$net1" \
        -device "virtio-net-pci,netdev=client1,mac=52:54:00:10:01:01" \
        -netdev "$net2" \
        -device "virtio-net-pci,netdev=client2,mac=52:54:00:10:02:01" \
        "$@"
)

run_vf() (
    require_cmd "$QEMU_AARCH64"
    SHMEM_FILE="${SHMEM_FILE:-$BUILD_ROOT/run/snic.ivshmem}"
    RUN_DIR="${RUN_DIR:-$BUILD_ROOT/run}"
    MILVUS_BIN="$PROJECT_ROOT/MilvusVisor/src/bin"
    FIRMWARE=$(find_aarch64_efi)

    [ -f "$SHMEM_FILE" ] || die "shared-memory file does not exist: $SHMEM_FILE"
    [ -f "$MILVUS_BIN/EFI/BOOT/BOOTAA64.EFI" ] || \
        die "MilvusVisor is not built; run make build-vf"
    mkdir -p "$RUN_DIR"

    exec "$QEMU_AARCH64" \
        -machine virt,virtualization=on,iommu=smmuv3,gic-version=3 \
        -name "${VM_NAME:-vsnic-milvusvisor}" \
        -cpu cortex-a72 -m 4G -smp 4 -display none -monitor none \
        -serial "${VM_SERIAL:-mon:stdio}" -no-reboot -no-shutdown \
        -nic none \
        -bios "$FIRMWARE" \
        -drive "file=fat:rw:$MILVUS_BIN,format=raw,media=disk" \
        -object "memory-backend-file,id=snicmem,size=$SNIC_SHMEM_BYTES,mem-path=$SHMEM_FILE,share=on" \
        -device "ivshmem-plain,memdev=snicmem,addr=02.0,rombar=0" \
        -chardev "file,id=console1,path=$RUN_DIR/snic1.log" \
        -device virtio-serial-device,id=vserial1,bus=virtio-mmio-bus.31 \
        -device virtconsole,chardev=console1,bus=vserial1.0,name=guest1-console \
        -chardev "file,id=console2,path=$RUN_DIR/snic2.log" \
        -device virtio-serial-device,id=vserial2,bus=virtio-mmio-bus.23 \
        -device virtconsole,chardev=console2,bus=vserial2.0,name=guest2-console \
        "$@"
)

stop_vms() (
    RUN_DIR="${RUN_DIR:-$BUILD_ROOT/run}"
    if [ -f "$RUN_DIR/current" ]; then RUN_DIR=$(sed -n '1p' "$RUN_DIR/current"); fi
    for name in client1 client2 management milvusvisor; do
        pid_file="$RUN_DIR/$name.pid"
        [ -f "$pid_file" ] || continue
        pid=$(sed -n '1p' "$pid_file")
        expected=$(sed -n '1p' "$RUN_DIR/$name.name")
        case "$pid" in ''|*[!0-9]*) die "invalid PID file $pid_file" ;; esac
        if kill -0 "$pid" 2>/dev/null; then
            if ! tr '\000' '\n' <"/proc/$pid/cmdline" | grep -Fxq -- "$expected"; then
                die "PID $pid no longer belongs to $expected; leaving it untouched"
            fi
            log "stopping $name (PID $pid)"
            kill -TERM "$pid"
            tries=0
            while kill -0 "$pid" 2>/dev/null; do
                state=$(awk '{print $3}' "/proc/$pid/stat" 2>/dev/null || true)
                [ "$state" != Z ] || break
                [ "$tries" -lt 50 ] || die "$name has not stopped; PID file retained"
                sleep 0.1; tries=$((tries + 1))
            done
        fi
        rm -f -- "$pid_file"
    done
    log "VMs stopped; logs and shared-memory snapshots retained in $RUN_DIR"
)

check_vms() (
    RUN_DIR="${RUN_DIR:-$BUILD_ROOT/run}"
    if [ -f "$RUN_DIR/current" ]; then RUN_DIR=$(sed -n '1p' "$RUN_DIR/current"); fi
    timeout_seconds="${CHECK_TIMEOUT:-300}"
    elapsed=0

    log "waiting up to $timeout_seconds seconds for both tenant packet paths"
    while [ "$elapsed" -lt "$timeout_seconds" ]
    do
        management_ok=0
        snic1_ok=0
        snic2_ok=0

        if [ -f "$RUN_DIR/management.log" ] && \
            grep -q -- 'dport=9001.*tenant 1' "$RUN_DIR/management.log" && \
            grep -q -- 'dport=9000.*tenant 2' "$RUN_DIR/management.log"; then
            management_ok=1
        fi
        if [ -f "$RUN_DIR/snic1.log" ] && grep -q -- 'client=1 sequence=5 expected_tenant=1' "$RUN_DIR/snic1.log" && \
            grep -q 'S2M tenant=1 seq=4 ' "$RUN_DIR/management.log"; then
            snic1_ok=1
        fi
        if [ -f "$RUN_DIR/snic2.log" ] && grep -q -- 'client=2 sequence=5 expected_tenant=2' "$RUN_DIR/snic2.log" && \
            grep -q 'S2M tenant=2 seq=4 ' "$RUN_DIR/management.log"; then
            snic2_ok=1
        fi
        if [ "$management_ok" -eq 1 ] && [ "$snic1_ok" -eq 1 ] && \
            [ "$snic2_ok" -eq 1 ]; then
            if grep -q 'expected_tenant=2' "$RUN_DIR/snic1.log" || grep -q 'expected_tenant=1' "$RUN_DIR/snic2.log"; then
                die "cross-tenant packet leak in debugger logs"
            fi
            log "PASS: first 5 packets on Client 1 -> SNIC 1 and Client 2 -> SNIC 2 (including S2M)"
            exit 0
        fi
        sleep 1
        elapsed=$((elapsed + 1))
    done

    printf '[vsnic] FAIL: packet-path timeout\n' >&2
    for log_file in management.log snic1.log snic2.log client1.log client2.log milvusvisor.log
    do
        if [ -f "$RUN_DIR/$log_file" ]; then
            printf '\n==> %s <==\n' "$RUN_DIR/$log_file" >&2
            tail -n 25 "$RUN_DIR/$log_file" >&2
        fi
    done
    exit 1
)

start_vms() (
    RUN_DIR="${RUN_DIR:-$BUILD_ROOT/run}"
    mkdir -p "$RUN_DIR"
    RUN_DIR=$(CDPATH= cd -- "$RUN_DIR" && pwd)
    export RUN_DIR
    require_cmd flock
    [ "$SNIC_SHMEM_BYTES" = 8388608 ] || die "ivshmem ABI requires exactly 8388608 bytes"
    exec 9>"$RUN_DIR/launcher.lock"
    flock -n 9 || die "another launcher is using $RUN_DIR"

    for artifact in "$BUILD_ROOT/bin/vsnic-shmem" \
        "$BUILD_ROOT/client/bzImage" "$BUILD_ROOT/client/initramfs.cpio" \
        "$BUILD_ROOT/vsnic-logic/bzImage" "$BUILD_ROOT/vsnic-logic/initramfs.cpio" \
        "$PROJECT_ROOT/MilvusVisor/src/bin/EFI/BOOT/BOOTAA64.EFI" \
        "$PROJECT_ROOT/MilvusVisor/src/bin/EFI/BOOT/hypervisor_kernel" \
        "$PROJECT_ROOT/MilvusVisor/src/bin/EFI/BOOT/Image" \
        "$PROJECT_ROOT/MilvusVisor/src/bin/EFI/BOOT/initramfs"
    do
        [ -s "$artifact" ] || die "missing artifact: $artifact (run make build)"
    done
    if [ -f "$RUN_DIR/current" ]; then
        previous=$(sed -n '1p' "$RUN_DIR/current")
        for pid_file in "$previous"/*.pid; do
            [ -f "$pid_file" ] || continue
            pid=$(sed -n '1p' "$pid_file")
            case "$pid" in ''|*[!0-9]*) die "invalid PID file: $pid_file" ;; esac
            if kill -0 "$pid" 2>/dev/null; then die "PID $pid still exists; run make stop"; fi
        done
    fi

    # New session directories retain previous logs and prevent stale READY/PASS.
    SESSION_DIR=$(mktemp -d "$RUN_DIR/session.XXXXXX")
    printf '%s\n' "$SESSION_DIR" >"$RUN_DIR/current"
    export SHMEM_FILE="$SESSION_DIR/snic.ivshmem"
    "$BUILD_ROOT/bin/vsnic-shmem" "$SHMEM_FILE"
    export RUN_DIR="$SESSION_DIR"
    complete=0
    cleanup() {
        if [ "$complete" -ne 1 ]; then stop_vms; fi
    }
    trap cleanup EXIT
    trap 'exit 130' INT TERM

    launch() {
        vm=$1
        shift
        VM_NAME="vsnic-$(basename "$SESSION_DIR")-$vm"
        printf '%s\n' "$VM_NAME" >"$SESSION_DIR/$vm.name"
        log "starting $vm"
        VM_NAME="$VM_NAME" VM_SERIAL="file:$SESSION_DIR/$vm.log" \
            "$@" -daemonize -pidfile "$SESSION_DIR/$vm.pid" \
            9>&- >"$SESSION_DIR/$vm.launch.log" 2>&1 || {
                tail -n 20 "$SESSION_DIR/$vm.launch.log" >&2
                die "$vm failed to start"
            }
    }
    wait_log() {
        vm=$1; pattern=$2; elapsed=0
        while ! grep -q "$pattern" "$SESSION_DIR/$vm.log" 2>/dev/null; do
            if grep -Eq 'VSNIC_FATAL|Kernel panic|panicked at' "$SESSION_DIR"/*.log 2>/dev/null; then
                die "guest failure; inspect $SESSION_DIR"
            fi
            [ "$elapsed" -lt "${BOOT_TIMEOUT:-300}" ] || die "$vm boot timeout ($SESSION_DIR)"
            sleep 1; elapsed=$((elapsed + 1))
        done
    }
    launch management sh "$SCRIPT_DIR/vm.sh" management
    wait_log management 'dispatcher ready'
    launch milvusvisor sh "$SCRIPT_DIR/vm.sh" vf
    wait_log snic1 'packet debugger ready'
    wait_log snic2 'packet debugger ready'
    launch client1 sh "$SCRIPT_DIR/vm.sh" client 1
    launch client2 sh "$SCRIPT_DIR/vm.sh" client 2
    complete=1
    log "all VMs ready; logs: $SESSION_DIR"
    log "make check checks both packet paths and the return stub"
)

check_all_vms() (
    start_vms
    trap stop_vms EXIT
    trap 'exit 130' INT TERM
    check_vms
)

action=${1:-}
[ "$#" -gt 0 ] && shift
case "$action" in
    start) start_vms "$@" ;;
    check) check_vms "$@" ;;
    stop) stop_vms "$@" ;;
    check-all) check_all_vms "$@" ;;
    client) run_client "$@" ;;
    management) run_management "$@" ;;
    vf) run_vf "$@" ;;
    *) die "internal usage: vm.sh start|check|stop|check-all|client|management|vf" ;;
esac
