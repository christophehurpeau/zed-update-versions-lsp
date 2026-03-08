LSP_BIN_NAME   := update-versions-lsp
LSP_RELEASE    := lsp-server/target/release/$(LSP_BIN_NAME)
EXT_BIN_DIR    := extension/bin

.PHONY: setup build-lsp install-dev lint fmt clean

## Install required toolchain and tools (run once after cloning).
setup:
	rustup target add wasm32-wasip1
	cargo install rusty-hook
	cargo test --manifest-path lsp-server/Cargo.toml --no-run
	@echo "Setup complete. Git hooks installed."

## Format and lint both crates.
lint:
	cargo fmt --check --manifest-path lsp-server/Cargo.toml
	cargo clippy --manifest-path lsp-server/Cargo.toml -- -D warnings
	cargo fmt --check --manifest-path extension/Cargo.toml
	cargo clippy --manifest-path extension/Cargo.toml --target wasm32-wasip1 -- -D warnings

## Auto-format both crates.
fmt:
	cargo fmt --manifest-path lsp-server/Cargo.toml
	cargo fmt --manifest-path extension/Cargo.toml

## Build the native LSP server binary (release mode).
build-lsp:
	cargo build --release --manifest-path lsp-server/Cargo.toml

## Build the LSP server and copy the binary into extension/bin/
## so that the dev extension can find it.
install-dev: build-lsp
	mkdir -p $(EXT_BIN_DIR)
	cp $(LSP_RELEASE) $(EXT_BIN_DIR)/$(LSP_BIN_NAME)
	@echo "Binary installed at $(EXT_BIN_DIR)/$(LSP_BIN_NAME)"
	@echo "Now open Zed and run: zed: install dev extension → pick extension/"

## Remove build artefacts.
clean:
	cargo clean --manifest-path lsp-server/Cargo.toml
	rm -rf $(EXT_BIN_DIR)
