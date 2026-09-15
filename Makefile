SHELL := /bin/sh
.SHELLFLAGS := -eu -c
.DEFAULT_GOAL := help
.DELETE_ON_ERROR:

# variables (user-overridable)
PROJECT_ROOT := $(patsubst %/,%,$(dir $(abspath $(lastword $(MAKEFILE_LIST)))))
BUILD_ROOT ?= $(PROJECT_ROOT)/build
CACHE_DIR ?= $(PROJECT_ROOT)/.cache
override BUILD_ROOT := $(abspath $(BUILD_ROOT))
override CACHE_DIR := $(abspath $(CACHE_DIR))
CARGO_HOME ?= $(BUILD_ROOT)/cargo-home
LINUX_VERSION ?= 6.12.1
ifeq ($(LINUX_VERSION),6.12.1)
LINUX_SHA256 ?= 0193b1d86dd372ec891bae799f6da20deef16fc199f30080a4ea9de8cef0c619
endif
LINUX_SHA256 ?=
LINUX_SOURCE ?= $(CACHE_DIR)/linux-$(LINUX_VERSION)
override LINUX_SOURCE := $(abspath $(LINUX_SOURCE))
JOBS ?= $(shell getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)
export CARGO_BUILD_JOBS ?= $(JOBS)
RUSTUP_TOOLCHAIN ?= nightly-2026-03-25
MILVUS_TOOLCHAIN ?= $(RUSTUP_TOOLCHAIN)
RUST_BUILD_STD ?= auto
AARCH64_CC ?= aarch64-linux-gnu-gcc
AARCH64_CROSS_COMPILE ?= aarch64-linux-gnu-
QEMU_X86_64 ?= qemu-system-x86_64
QEMU_AARCH64 ?= qemu-system-aarch64
QEMU_ACCEL ?= tcg
AARCH64_EFI ?=
NETWORK_MODE ?= socket
CLIENT1_PORT ?= 19001
CLIENT2_PORT ?= 19002

# 0 means unlimited; otherwise the client will send only that many packets and exit.
CLIENT_PACKET_COUNT ?= 0
CLIENT_INTERVAL_MS ?= 1000
SNIC_SHMEM_BYTES ?= 8388608
RUN_DIR ?= $(BUILD_ROOT)/run
override RUN_DIR := $(abspath $(RUN_DIR))
BRIDGE_RUN_DIR ?= $(BUILD_ROOT)/bridge-verify
override BRIDGE_RUN_DIR := $(abspath $(BRIDGE_RUN_DIR))
SHMEM_FILE ?= $(RUN_DIR)/snic.ivshmem
BOOT_TIMEOUT ?= 300
CHECK_TIMEOUT ?= 300
NETWORK_OWNER ?= $(shell id -un)
CLIENT_ID ?= 1

export PROJECT_ROOT BUILD_ROOT CACHE_DIR CARGO_HOME RUSTUP_TOOLCHAIN
export QEMU_X86_64 QEMU_AARCH64 QEMU_ACCEL AARCH64_EFI NETWORK_MODE
export CLIENT1_PORT CLIENT2_PORT SNIC_SHMEM_BYTES RUN_DIR SHMEM_FILE
export CLIENT_PACKET_COUNT CLIENT_INTERVAL_MS
export BOOT_TIMEOUT CHECK_TIMEOUT BRIDGE_RUN_DIR

# Optional workspace-local host headers/libraries (no system installation).
ifneq ($(wildcard $(CACHE_DIR)/host-deps/usr/include),)
export CPATH := $(CACHE_DIR)/host-deps/usr/include$(if $(CPATH),:$(CPATH))
export LIBRARY_PATH := $(CACHE_DIR)/host-deps/usr/lib/x86_64-linux-gnu$(if $(LIBRARY_PATH),:$(LIBRARY_PATH))
endif

X86_KERNEL := $(BUILD_ROOT)/linux-x86_64-management
ARM_KERNEL := $(BUILD_ROOT)/linux-aarch64-snic
MILVUS_SOURCE := $(PROJECT_ROOT)/MilvusVisor/src
MILVUS_BOOT := $(MILVUS_SOURCE)/bin/EFI/BOOT
CARGO := env -u MAKEFLAGS -u MFLAGS cargo
CARGO_WORKSPACE := --manifest-path "$(PROJECT_ROOT)/Cargo.toml" --locked
VM := sh "$(PROJECT_ROOT)/scripts/vm.sh"
NETWORK := sh "$(PROJECT_ROOT)/scripts/network.sh"
KERNEL_JOBS = $(if $(filter -j%,$(MAKEFLAGS)),,-j$(JOBS))

.PHONY: help all build build-client build-logic build-vf build-milvus build-host \
        kernel-x86 kernel-arm rust-client rust-logic rust-vf linux-source \
        run run-only check stop check-all check-all-only bridge-check bridge-check-only \
        run-client run-logic run-vf shmem network-up network-down test lint fmt-check

help:
	@printf '%s\n' \
	  'make build                   Build all kernels, Rust initramfs, and MilvusVisor' \
	  'make run (or make all)        Build, then start all VMs in readiness order' \
	  'make run-only                Start existing images without building' \
	  'make run CLIENT_INTERVAL_MS=500  Send continuously every 500 ms (default: 1000)' \
	  'make run CLIENT_PACKET_COUNT=5   Send only 5 packets per client (default: 0/unlimited)' \
	  'make check / make stop       Check packet paths / stop this session safely' \
	  'make test / make lint        Rust tests + shell syntax / clippy + format check' \
	  'make check-all               Build, start, check, and automatically stop' \
	  'make check-all-only          Same integration check without building' \
	  'make bridge-check            Build, then check in a private netns (sudo)' \
	  'make build-client|build-logic|build-vf|build-milvus' \
	  'make run-client CLIENT_ID=1|2 / make run-logic / make run-vf' \
	  'make shmem SHMEM_FILE=/absolute/new/file (individual launches only)' \
	  'make network-up / make network-down (sudo; isolated bridge/TAP only)' \
	  'Configuration: make run JOBS=4 NETWORK_MODE=socket QEMU_ACCEL=tcg'

all: run
build: build-host build-client build-logic build-vf

linux-source:
	@if [ ! -f "$(LINUX_SOURCE)/Makefile" ]; then \
	  [ "$(LINUX_SOURCE)" = "$(CACHE_DIR)/linux-$(LINUX_VERSION)" ] || \
	    { echo 'LINUX_SOURCE is not a kernel tree' >&2; exit 1; }; \
	  mkdir -p "$(CACHE_DIR)"; \
	  archive="$(CACHE_DIR)/linux-$(LINUX_VERSION).tar.xz"; \
	  if [ ! -f "$$archive" ]; then \
	    curl --fail --location --retry 3 \
	      "https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-$(LINUX_VERSION).tar.xz" \
	      --output "$$archive.part"; \
	    mv "$$archive.part" "$$archive"; \
	  fi; \
	  if [ -n "$(LINUX_SHA256)" ]; then \
	    printf '%s  %s\n' "$(LINUX_SHA256)" "$$archive" | sha256sum -c -; \
	  fi; \
	  mkdir -p "$(LINUX_SOURCE)"; \
	  tar -xJf "$$archive" --strip-components=1 -C "$(LINUX_SOURCE)"; \
	fi

kernel-x86: KERNEL_OUTPUT := $(X86_KERNEL)
kernel-x86: KERNEL_ARCH := x86
kernel-x86: KERNEL_CROSS :=
kernel-x86: KERNEL_FRAGMENTS := "$(PROJECT_ROOT)/client/kernel/config.fragment" "$(PROJECT_ROOT)/vsnic-logic/kernel/config.fragment"
kernel-x86: KERNEL_TARGET := bzImage
kernel-arm: KERNEL_OUTPUT := $(ARM_KERNEL)
kernel-arm: KERNEL_ARCH := arm64
kernel-arm: KERNEL_CROSS := $(AARCH64_CROSS_COMPILE)
kernel-arm: KERNEL_FRAGMENTS := "$(PROJECT_ROOT)/vsnic-vf/kernel/config.fragment"
kernel-arm: KERNEL_TARGET := Image

kernel-x86 kernel-arm: linux-source
	+$(MAKE) -C "$(LINUX_SOURCE)" O="$(KERNEL_OUTPUT)" ARCH=$(KERNEL_ARCH) CROSS_COMPILE="$(KERNEL_CROSS)" KERNELRELEASE="$(LINUX_VERSION)-vsnic" LOCALVERSION= tinyconfig
	"$(LINUX_SOURCE)/scripts/kconfig/merge_config.sh" -m -O "$(KERNEL_OUTPUT)" "$(KERNEL_OUTPUT)/.config" $(KERNEL_FRAGMENTS)
	+$(MAKE) -C "$(LINUX_SOURCE)" O="$(KERNEL_OUTPUT)" ARCH=$(KERNEL_ARCH) CROSS_COMPILE="$(KERNEL_CROSS)" KERNELRELEASE="$(LINUX_VERSION)-vsnic" LOCALVERSION= olddefconfig
	@for required in CONFIG_BINFMT_ELF CONFIG_BLK_DEV_INITRD CONFIG_DEVTMPFS CONFIG_PROC_FS CONFIG_SYSFS CONFIG_FUTEX CONFIG_VIRTIO CONFIG_VIRTIO_NET; do \
	  grep -qx "$$required=y" "$(KERNEL_OUTPUT)/.config" || { echo "$$required missing after olddefconfig" >&2; exit 1; }; \
	done
	+$(MAKE) -C "$(LINUX_SOURCE)" O="$(KERNEL_OUTPUT)" ARCH=$(KERNEL_ARCH) CROSS_COMPILE="$(KERNEL_CROSS)" KERNELRELEASE="$(LINUX_VERSION)-vsnic" LOCALVERSION= $(KERNEL_JOBS) $(KERNEL_TARGET)

# Auto fallback only uses already-installed rust-src; never installs toolchains.
define rust-build-std
build_std="$(RUST_BUILD_STD)"; \
if [ "$$build_std" = auto ]; then \
  build_std=0; \
  for target in $(1); do \
    [ -d "$$(env -u MAKEFLAGS -u MFLAGS rustc --print target-libdir --target "$$target")" ] || build_std=1; \
  done; \
fi; \
set --; \
case "$$build_std" in \
  1) set -- -Z build-std=$(2) ;; \
  0) ;; \
  *) echo 'RUST_BUILD_STD must be auto, 0, or 1' >&2; exit 1 ;; \
esac;
endef

define build-guest
	@$(call rust-build-std,$(2),std$(comma)panic_abort) \
	CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$(AARCH64_CC)" \
	RUSTFLAGS='-C target-feature=+crt-static' $(CARGO) build $(CARGO_WORKSPACE) \
	  "$$@" --release --target "$(2)" --target-dir "$(BUILD_ROOT)/rust" -p $(1)
	install -D -m 0755 "$(BUILD_ROOT)/rust/$(2)/release/$(1)" "$(BUILD_ROOT)/$(3)/$(4)"
endef
comma := ,

rust-client:
	$(call build-guest,vsnic-client,x86_64-unknown-linux-gnu,client,client-init)
rust-logic:
	$(call build-guest,vsnic-logic,x86_64-unknown-linux-gnu,vsnic-logic,management-init)
rust-vf:
	$(call build-guest,vsnic-vf,aarch64-unknown-linux-gnu,vsnic-vf,snic-init)

# gen_init_cpio creates console/null nodes rootlessly, before Rust/libc startup.
define initramfs
	@mkdir -p "$(BUILD_ROOT)/$(1)/rootfs/dev" "$(BUILD_ROOT)/$(1)/rootfs/proc" \
	  "$(BUILD_ROOT)/$(1)/rootfs/sys" "$(BUILD_ROOT)/$(1)/rootfs/tmp"
	install -m 0755 "$(BUILD_ROOT)/$(1)/$(2)" "$(BUILD_ROOT)/$(1)/rootfs/init"
	@{ printf 'dir /dev 0755 0 0\ndir /proc 0755 0 0\ndir /sys 0755 0 0\ndir /tmp 1777 0 0\n'; \
	  printf 'nod /dev/console 0600 0 0 c 5 1\nnod /dev/null 0666 0 0 c 1 3\n'; \
	  printf 'file /init %s 0755 0 0\n' "$(BUILD_ROOT)/$(1)/$(2)"; \
	} >"$(BUILD_ROOT)/$(1)/rootfs.cpio-list"
	"$(3)/usr/gen_init_cpio" -t 0 "$(BUILD_ROOT)/$(1)/rootfs.cpio-list" >"$(BUILD_ROOT)/$(1)/initramfs.cpio.tmp"
	mv "$(BUILD_ROOT)/$(1)/initramfs.cpio.tmp" "$(BUILD_ROOT)/$(1)/initramfs.cpio"
endef

build-client: kernel-x86 rust-client
	$(call initramfs,client,client-init,$(X86_KERNEL))
	install -m 0644 "$(X86_KERNEL)/arch/x86/boot/bzImage" "$(BUILD_ROOT)/client/bzImage"

build-logic: kernel-x86 rust-logic
	$(call initramfs,vsnic-logic,management-init,$(X86_KERNEL))
	install -m 0644 "$(X86_KERNEL)/arch/x86/boot/bzImage" "$(BUILD_ROOT)/vsnic-logic/bzImage"

build-vf: kernel-arm rust-vf build-milvus
	$(call initramfs,vsnic-vf,snic-init,$(ARM_KERNEL))
	install -m 0644 "$(ARM_KERNEL)/arch/arm64/boot/Image" "$(BUILD_ROOT)/vsnic-vf/Image"
	install -m 0644 "$(BUILD_ROOT)/vsnic-vf/Image" "$(MILVUS_BOOT)/Image"
	install -m 0644 "$(BUILD_ROOT)/vsnic-vf/initramfs.cpio" "$(MILVUS_BOOT)/initramfs"

build-host:
	$(CARGO) build $(CARGO_WORKSPACE) --release --target-dir "$(BUILD_ROOT)/rust-host" -p libvsnic --bin vsnic-shmem
	install -D -m 0755 "$(BUILD_ROOT)/rust-host/release/vsnic-shmem" "$(BUILD_ROOT)/bin/vsnic-shmem"

build-milvus:
	@export RUSTUP_TOOLCHAIN="$(MILVUS_TOOLCHAIN)" CARGO_TARGET_DIR="$(BUILD_ROOT)/milvus-target"; \
	$(call rust-build-std,aarch64-unknown-uefi aarch64-unknown-none-softfloat,core) \
	(cd "$(MILVUS_SOURCE)/hypervisor_bootloader" && $(CARGO) build "$$@" --release --no-default-features); \
	(cd "$(MILVUS_SOURCE)/hypervisor_kernel" && $(CARGO) build "$$@" --release --no-default-features --features virtio_net)
	install -D -m 0644 "$(BUILD_ROOT)/milvus-target/aarch64-unknown-uefi/release/hypervisor_bootloader.efi" "$(MILVUS_BOOT)/BOOTAA64.EFI"
	install -D -m 0644 "$(BUILD_ROOT)/milvus-target/aarch64-unknown-none-softfloat/release/hypervisor_kernel" "$(MILVUS_BOOT)/hypervisor_kernel"

run: build
	@$(VM) start
run-only:
	@$(VM) start
check:
	@$(VM) check
stop:
	@$(VM) stop
check-all: build
	@$(VM) check-all
check-all-only:
	@$(VM) check-all
bridge-check: build
	@$(NETWORK) check
bridge-check-only:
	@$(NETWORK) check

shmem: build-host
	"$(BUILD_ROOT)/bin/vsnic-shmem" "$(SHMEM_FILE)"
run-client:
	@$(VM) client "$(CLIENT_ID)"
run-logic:
	@$(VM) management
run-vf:
	@$(VM) vf
network-up:
	@sudo $(NETWORK) up "$(NETWORK_OWNER)"
network-down:
	@sudo $(NETWORK) down

test:
	$(CARGO) test $(CARGO_WORKSPACE) --workspace
	@for script in "$(PROJECT_ROOT)"/scripts/*.sh; do sh -n "$$script"; done
fmt-check:
	$(CARGO) fmt --manifest-path "$(PROJECT_ROOT)/Cargo.toml" --all -- --check
lint: fmt-check
	$(CARGO) clippy $(CARGO_WORKSPACE) --workspace --all-targets -- -D warnings
