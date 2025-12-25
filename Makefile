.PHONY: build install clean test run

# Default target
all: build

# Build the release binary
build:
	cargo build --release

# Install the binary (copies to ~/.cargo/bin, skipping rebuild)
install: build
	mkdir -p $(HOME)/.cargo/bin
	cp target/release/msrch $(HOME)/.cargo/bin/

# Clean build artifacts
clean:
	cargo clean

# Run tests
test:
	cargo test

# Run the binary (debug mode)
run:
	cargo run
