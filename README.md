# zed-update-versions-lsp

A [Zed](https://zed.dev) extension that shows the latest available version for each dependency declared in your project's package manifests — directly inline in the editor, as inlay hints.

[![CI](https://github.com/christophehurpeau/zed-update-versions-lsp/actions/workflows/ci.yml/badge.svg)](https://github.com/christophehurpeau/zed-update-versions-lsp/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

---

## Features

- **Inline version hints** — see at a glance whether a dependency is up to date, or has a patch / minor / major update available.
- **Code actions** — apply a version update with a single `Cmd+.` on the dependency line.
- **Prerelease awareness** — optionally surface prerelease versions (alpha, beta, rc…).
- **In-memory cache** — registry responses are cached with a configurable TTL to keep the editor snappy.
- **Supported ecosystems** (v0.2): npm / Node.js (`package.json`), Rust / Cargo (`Cargo.toml`), Python / PyPI (`requirements.txt`, `pyproject.toml`).

| State | Label |
|-------|-------|
| Up to date | `✔ latest` |
| Patch update available | `↑ 1.2.3 (patch)` |
| Minor update available | `↑ 1.3.0 (minor)` |
| Major update available | `↑ 2.0.0 (major)` |
| Package not found | `✘ not found` |
| Unsupported constraint | `⊘ unsupported` |
| Fetching | `… fetching` |

---

## Installation

### From the Zed Extension Marketplace

1. Open the Extensions panel in Zed (`Cmd+Shift+X` or `zed: extensions`).
2. Search for **Update Versions**.
3. Click **Install**.

### Manual / development install

See [CONTRIBUTING.md](CONTRIBUTING.md) for building from source and installing the dev extension.

---

## Configuration

Settings live under `"update-versions-lsp"` in Zed's `settings.json`:

```jsonc
{
  "lsp": {
    "update-versions-lsp": {
      "initialization_options": {
        // Cache TTL for registry responses, in seconds (default: 300)
        "cacheTtlSecs": 300,
        // Hide prerelease suggestions (default: false)
        "hidePrereleases": false,
        // npm registry URL (default: "https://registry.npmjs.org")
        "npm": {
          "registry": "https://registry.npmjs.org"
        }
      }
    }
  }
}
```

---

## Architecture

The extension is split into two components:

```
WASM extension  (extension/)      — locates the LSP binary, hands it to Zed
LSP server      (lsp-server/)     — parses manifests, fetches registries, returns inlay hints
```

The LSP server is a standalone native Rust binary (`update-versions-lsp`) communicated with over stdio using the Language Server Protocol. See [SPEC_TECHNICAL.md](SPEC_TECHNICAL.md) for the full architecture description.

---

## Acknowledgements

This extension was built with the assistance of [Claude Code](https://claude.ai/code).

---

## License

[MIT](LICENSE) © Christophe Hurpeau
