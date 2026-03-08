# Contributing

Thank you for your interest in contributing to zed-update-versions!

---

## Prerequisites

- [Rust](https://rustup.rs/) (stable toolchain)
- [Zed](https://zed.dev/) (latest stable or preview)

After cloning, run:

```sh
make setup
```

This installs the `wasm32-wasip1` rustup target and writes the git hooks.

---

## Repository layout

```
extension/        WASM extension (Rust → .wasm) — thin shim that locates the LSP binary
lsp-server/       Native LSP server binary (Rust)
```

---

## Development workflow

### 1. Build the LSP server

```sh
make build-lsp
```

This compiles `lsp-server/` in release mode and places the binary at `target/release/update-versions-lsp`.

### 2. Install the binary for the dev extension

```sh
make install-dev
```

Copies the binary to `extension/bin/update-versions-lsp` where the WASM extension looks for it at dev time.

### 3. Install the dev extension in Zed

Open Zed, run the command `zed: install dev extension`, and pick the `extension/` directory.

### 4. Full clean rebuild

```sh
make clean && make install-dev
```

---

## Running tests

```sh
# LSP server unit tests
cargo test -p update-versions-lsp

# Extension (WASM) — build check only (no testable logic)
cargo build -p update-versions --target wasm32-wasip1
```

---

## Linting & formatting

```sh
# Check formatting and clippy for both crates
make lint

# Auto-format both crates
make fmt
```

---

## Submitting changes

1. Fork the repository and create a feature branch.
2. Make sure `cargo test`, `cargo fmt --check`, and `cargo clippy` all pass.
3. Open a pull request against `main` with a clear description of the change.

---

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
