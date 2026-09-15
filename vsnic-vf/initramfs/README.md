`make build-vf` at the project root cross-compiles the Rust `vsnic-vf` binary as a static AArch64 `/init`.
The same archive boots in both MilvusVisor guests; `/init` reads the guest CPU
`reg` property from the device tree to select tenant 1 or 2.
