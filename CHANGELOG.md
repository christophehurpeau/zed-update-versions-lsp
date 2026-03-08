# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- release-plz prepends new entries above this line -->

## [0.1.0] - 2026-03-08

### Added

- **Cargo**: inline hints showing the latest available version for each
  dependency in `Cargo.toml`, with severity levels (up-to-date, patch, minor,
  major).
- **npm/pnpm/yarn**: inline hints for dependencies in `package.json`.
- **PyPI**: inline hints for Python packages in `requirements.txt` and
  `pyproject.toml` (both `[project.dependencies]` and
  `[tool.poetry.dependencies]`).
- **Ruby / Bundler**: inline hints for gems in `Gemfile`.
- LSP server distributed as pre-built binaries for macOS (ARM & Intel),
  Linux (x86\_64 & aarch64), and Windows (x86\_64) — downloaded automatically
  by the Zed extension on first use.
- GitHub Actions CI (lint, test, WASM build) and release workflow
  (`release-lsp.yml`) for cross-compiled binary uploads.

[0.1.0]: https://github.com/christophehurpeau/zed-update-versions/releases/tag/lsp-v0.1.0
