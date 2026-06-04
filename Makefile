.PHONY: build build-arm clean run check

TARGET := aarch64-unknown-linux-musl
BINARY := mini-agent

build:
	cargo build --release

build-arm:
	rustup target add $(TARGET) 2>/dev/null || true
	cargo build --release --target $(TARGET)
	@echo "Binary: target/$(TARGET)/release/$(BINARY)"
	@file target/$(TARGET)/release/$(BINARY)

build-minimal:
	rustup target add $(TARGET) 2>/dev/null || true
	cargo build --profile release-minimal --target $(TARGET)
	@echo "Binary: target/$(TARGET)/release-minimal/$(BINARY)"
	@ls -lh target/$(TARGET)/release-minimal/$(BINARY)

clean:
	cargo clean

run:
	cargo run

check:
	cargo check

fmt:
	cargo fmt

fmt-check:
	cargo fmt -- --check
