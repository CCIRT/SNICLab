`make build-client` at the project root statically links the Rust `vsnic-client` binary as `/init`.
The `newc` archive includes `/dev/console` and `/dev/null` for libc/Rust startup.
The application configures virtio-net and sends deterministic UDP packets continuously,
once per second by default. `CLIENT_PACKET_COUNT=0` means unlimited; a positive
value limits the number of packets. `CLIENT_INTERVAL_MS` controls the interval.
The Makefile passes these settings to `/init` through the kernel command line.
