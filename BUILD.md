# Build Guide

## Prerequisites

### Native Build (Development)

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env

# Verify
rustc --version  # >= 1.75
```

### Cross-Compile for ARM64 + musl (Production)

```bash
# Install Rust target
rustup target add aarch64-unknown-linux-musl

# Install cross-compiler (Debian/Ubuntu)
sudo apt-get update
sudo apt-get install -y gcc-aarch64-linux-gnu

# For ring (crypto library used by rustls), also install:
sudo apt-get install -y musl-tools
```

## Build Commands

### Development Build

```bash
cargo build
cargo run
```

### Release Build (Native)

```bash
cargo build --release
# Binary: target/release/mini-agent
```

### Release Build for ARM64 + musl (Debian 7 Compatible)

```bash
# Set environment
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc
export AR_aarch64_unknown_linux_musl=aarch64-linux-gnu-ar

# Build
cargo build --release --target aarch64-unknown-linux-musl

# Verify static linking
file target/aarch64-unknown-linux-musl/release/mini-agent
# Expected: ELF 64-bit LSB executable, ARM aarch64, version 1 (SYSV), statically linked

ldd target/aarch64-unknown-linux-musl/release/mini-agent
# Expected: not a dynamic executable
```

### Minimal Size Build

```bash
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_musl=aarch64-linux-gnu-gcc
cargo build --profile release-minimal --target aarch64-unknown-linux-musl

# Strip symbols (already done by profile, but double-check)
strip target/aarch64-unknown-linux-musl/release-minimal/mini-agent

# Check size
ls -lh target/aarch64-unknown-linux-musl/release-minimal/mini-agent
```

## Troubleshooting

### ring build fails

If `ring` fails to compile for the target:

```bash
# Install perl (required by ring build script)
sudo apt-get install perl

# Set explicit compiler env vars
export TARGET_CC=aarch64-linux-gnu-gcc
export TARGET_AR=aarch64-linux-gnu-ar
```

### musl toolchain not found

On some distributions, you may need to install the musl cross-compiler:

```bash
# Alpine-based Docker image (easiest for musl)
docker run --rm -it -v $(pwd):/src alpine:latest
apk add rust cargo musl-dev gcc make
# Then build inside the container
```

### SQLite bundled fails

`rusqlite` with `bundled` feature compiles SQLite from source. If it fails:

```bash
# Ensure you have a working C compiler for the target
export CC=aarch64-linux-gnu-gcc
```

## Deployment

```bash
# Copy to target device
scp target/aarch64-unknown-linux-musl/release/mini-agent user@arm-device:/usr/local/bin/

# On the ARM device (Debian 7):
mkdir -p ~/.mini-agent
# Edit ~/.mini-agent/config.toml
# Run
mini-agent
```
