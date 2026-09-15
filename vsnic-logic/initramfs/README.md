`make build-logic` at the project root statically links the Rust `vsnic-logic` binary as `/init`.
It configures the two client ports, enables/maps ivshmem BAR2, validates the
host-initialized ABI, and dispatches packets. It never resets a live ring.
