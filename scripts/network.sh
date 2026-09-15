#!/bin/sh
# Complex privileged lifecycle stays here; build/configuration belongs to Make.
set -eu

network_up() (
    [ "$(id -u)" -eq 0 ] || { echo 'Use make network-up NETWORK_OWNER=user.' >&2; exit 1; }
    owner=${1:-${SUDO_USER:-root}}
    id "$owner" >/dev/null
    for dev in vsnic-br1 vsnic-br2 vsnic-c1 vsnic-c2 vsnic-m1 vsnic-m2; do
        if ip link show dev "$dev" >/dev/null 2>&1; then
            echo "Refusing to overwrite existing interface $dev" >&2; exit 1
        fi
    done
    created=''
    cleanup() { for dev in $created; do ip link delete dev "$dev" || true; done; }
    trap cleanup EXIT
    for id in 1 2; do
        bridge=vsnic-br$id
        ip link add name "$bridge" type bridge stp_state 0 forward_delay 0
        created="$bridge $created"
        ip link set dev "$bridge" alias vsnic-prototype
        for side in c m; do
            tap=vsnic-$side$id
            ip tuntap add dev "$tap" mode tap user "$owner"
            created="$tap $created"
            ip link set dev "$tap" alias vsnic-prototype
            ip link set dev "$tap" master "$bridge"
            ip link set dev "$tap" up
        done
        ip link set dev "$bridge" up
    done
    trap - EXIT
    echo "Created isolated vsnic-br1/vsnic-br2 with TAPs owned by $owner"
)

network_down() (
    [ "$(id -u)" -eq 0 ] || { echo 'Use make network-down, after make stop.' >&2; exit 1; }
    for dev in vsnic-c1 vsnic-c2 vsnic-m1 vsnic-m2 vsnic-br1 vsnic-br2; do
        [ -e "/sys/class/net/$dev" ] || continue
        [ "$(sed -n '1p' "/sys/class/net/$dev/ifalias")" = vsnic-prototype ] || {
            echo "Refusing to delete unowned interface $dev" >&2; exit 1
        }
    done
    for dev in vsnic-c1 vsnic-c2 vsnic-m1 vsnic-m2 vsnic-br1 vsnic-br2; do
        [ -e "/sys/class/net/$dev" ] || continue
        ip link delete dev "$dev"
    done
    echo 'Removed vsnic TAPs/bridges; make network-up can recreate them.'
)

bridge_check() (
    : "${PROJECT_ROOT:?Use make bridge-check}"
    # sudo drops the environment: explicitly carry only runtime configuration.
    exec sudo -n unshare --net env \
        PROJECT_ROOT="$PROJECT_ROOT" BUILD_ROOT="$BUILD_ROOT" \
        RUN_DIR="$BRIDGE_RUN_DIR" NETWORK_MODE=bridge \
        QEMU_X86_64="$QEMU_X86_64" QEMU_AARCH64="$QEMU_AARCH64" \
        QEMU_ACCEL="$QEMU_ACCEL" AARCH64_EFI="$AARCH64_EFI" \
        CLIENT1_PORT="$CLIENT1_PORT" CLIENT2_PORT="$CLIENT2_PORT" \
        CLIENT_PACKET_COUNT="$CLIENT_PACKET_COUNT" CLIENT_INTERVAL_MS="$CLIENT_INTERVAL_MS" \
        SNIC_SHMEM_BYTES="$SNIC_SHMEM_BYTES" \
        BOOT_TIMEOUT="$BOOT_TIMEOUT" CHECK_TIMEOUT="$CHECK_TIMEOUT" \
        sh "$PROJECT_ROOT/scripts/network.sh" check-inside
)

bridge_check_inside() (
    [ "$(readlink /proc/self/ns/net)" != "$(readlink /proc/1/ns/net)" ] || {
        echo 'Refusing to run outside a private network namespace' >&2; exit 1
    }
    : "${PROJECT_ROOT:?Use make bridge-check}"
    network_up "${SUDO_USER:-root}"
    trap network_down EXIT
    trap 'exit 130' INT TERM
    sh "$PROJECT_ROOT/scripts/vm.sh" check-all
)

action=${1:-}
[ "$#" -gt 0 ] && shift
case "$action" in
    up) network_up "$@" ;;
    down) network_down "$@" ;;
    check) bridge_check ;;
    check-inside) bridge_check_inside ;;
    *) echo 'internal usage: network.sh up|down|check' >&2; exit 1 ;;
esac
