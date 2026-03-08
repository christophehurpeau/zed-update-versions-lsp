# TODO — zed-update-versions

> Implementation progress tracker.  
> Based on [SPEC.md](./SPEC.md) and [SPEC_TECHNICAL.md](./SPEC_TECHNICAL.md).

---

## Phase 1 — MVP (v0.1): npm + Cargo

### Infrastructure

- [x] POC: Confirm `textDocument/inlayHint` works in Zed
- [x] POC: Confirm `textDocument/codeAction` works in Zed
- [x] POC: Confirm `InlayHintLabelPart.command` is NOT supported
- [x] WASM extension skeleton (`extension/`)
- [x] LSP server POC (`lsp-server/src/main.rs`)
- [x] Restructure LSP server into modules:
  - [x] `backend.rs` — `LanguageServer` impl
  - [x] `cache.rs` — `VersionCache` (in-memory, TTL, dedup)
  - [x] `config.rs` — `ConfigManager` (settings, toggles)
  - [x] `semver_utils.rs` — range parsing, comparison, operator preservation
  - [x] `providers/mod.rs` — `Provider` trait + `ProviderRegistry`
  - [x] `providers/npm.rs` — npm registry provider
  - [x] `providers/cargo.rs` — crates.io provider
- [x] `main.rs` — thin entry point delegating to `backend`
- [x] Unit tests for `semver_utils` (15 tests)
- [x] Unit tests for `cache` (5 tests)
- [x] Unit tests for `config` (5 tests)
- [x] Unit tests for npm provider parsing (9 tests)
- [x] Unit tests for Cargo provider parsing (8 tests)
- [x] Unit tests for backend helpers (10 tests)

### LSP Features

- [x] `textDocument/inlayHint` — version hints with status symbols
- [x] `textDocument/codeAction` — update version quick-fix (applies WorkspaceEdit)
- [x] `workspace/didChangeConfiguration` — read `update-versions` settings
- [x] Lazy fetching (no requests when hints disabled)
- [x] Cache with TTL + fetch deduplication

### WASM Extension

- [x] Dev build: binary path via `CARGO_MANIFEST_DIR`
- [ ] Production build: download binary from GitHub Releases

### Known Bugs

- [x] **Inlay hints cancelled**: `inlay_hint` now returns `… fetching` hints immediately from
  cache (or `Loading` state for uncached deps), spawns background HTTP fetches, then calls
  `inlayHint/refresh` — Zed re-requests hints with real statuses once all fetches complete.
- [x] **`cache_ttl_secs` setting ignored**: `Backend::new` now reads `cache_ttl_secs` from
  `Settings::default()` instead of hardcoding 300. Hot-reload of TTL is not supported (requires
  server restart).
- [x] **Provider settings not hot-reloaded**: `ProviderRegistry` is now stored in an `RwLock`;
  `workspace/didChangeConfiguration` rebuilds providers with the new npm registry URL and
  dependency keys before updating `ConfigManager`, so all subsequent requests use the updated
  settings immediately.

---

## Phase 2 — Python & TOML (v0.2)

- [x] `providers/pypi.rs` — `requirements.txt` + `pyproject.toml`
- [x] Unit tests for PyPI provider

## Phase 3 — Web Ecosystem (v0.3)

- [ ] `providers/composer.rs` — `composer.json`
- [ ] `providers/deno.rs` — `deno.json`, `import_map.json`
- [ ] `providers/rubygems.rs` — `Gemfile`

## Phase 4 — Enterprise & JVM (v0.4)

- [ ] `providers/nuget.rs` — `.csproj`, `.fsproj`, `.vbproj`, `Directory.Packages.props`
- [ ] `providers/maven.rs` — `pom.xml`
- [ ] `providers/pub.rs` — `pubspec.yaml`

## Phase 5 — Systems & Infra (v0.5)

- [ ] `providers/dub.rs` — `dub.json`, `dub.sdl`
- [ ] `providers/docker.rs` — `Dockerfile`
- [ ] Private registry auth hardening
- [ ] Dist tag / release channel filtering

---

## CI/CD

- [ ] `ci.yml` — test + WASM build on every push
- [ ] `release-lsp.yml` — cross-compile on `lsp-v*` tag
- [ ] `release-extension.yml` — publish extension on `extension-v*` tag
