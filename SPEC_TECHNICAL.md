# zed-update-versions — Technical Specification

> Implementation blueprint for the Zed editor extension.  
> Read alongside [SPEC.md](./SPEC.md) for the feature description.

---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Component A — WASM Extension (Rust)](#2-component-a--wasm-extension-rust)
3. [Component B — LSP Server (Rust)](#3-component-b--lsp-server-rust)
4. [File Type & Language Registration](#4-file-type--language-registration)
5. [LSP Protocol Details](#5-lsp-protocol-details)
6. [Version Resolution per Provider](#6-version-resolution-per-provider)
7. [Semver Handling](#7-semver-handling)
8. [Settings & Configuration](#8-settings--configuration)
9. [Show / Hide Toggle](#9-show--hide-toggle)
10. [One-Click Updates](#10-one-click-updates)
11. [Caching Strategy](#11-caching-strategy)
12. [Authentication](#12-authentication)
13. [Logging](#13-logging)
14. [Build & Distribution](#14-build--distribution)
15. [Feasibility Deviations from Feature Spec](#15-feasibility-deviations-from-feature-spec)
16. [Phasing Recommendation](#16-phasing-recommendation)
17. [Repository Structure](#17-repository-structure)

---

## 1. Architecture Overview

The extension is composed of two tightly-coupled components:

```
┌────────────────────────────────────────────────────────────┐
│  Zed editor process                                        │
│                                                            │
│  ┌──────────────────────┐   JSON-RPC (stdio)               │
│  │  WASM extension      │◄──────────────────────────────┐  │
│  │  (Rust → .wasm)      │   starts, configures          │  │
│  └────────────┬─────────┘                               │  │
│               │ launches                                │  │
│               ▼                                         │  │
│  ┌──────────────────────┐                               │  │
│  │  LSP server binary   │   HTTP ──► registries         │  │
│  │  (Rust — native)     │◄──────────────────────────────┘  │
│  └──────────────────────┘                                  │
└────────────────────────────────────────────────────────────┘
```

| Layer | Responsibility |
|-------|----------------|
| **WASM extension** | Extension lifecycle; downloads and locates the native LSP server binary for the current platform; provides the launch command to Zed; bridges workspace configuration from Zed settings to the LSP server via `workspace/didChangeConfiguration`. |
| **LSP server** | Parses manifest files, resolves version strings, fetches latest versions from public/private registries over HTTP, returns `textDocument/inlayHint` responses, and provides `textDocument/codeAction` fix-ups for version updates. |

The LSP server is a **standalone native Rust binary** (`update-versions-lsp`), cross-compiled for each target platform and distributed via GitHub Releases. The WASM extension downloads the appropriate binary on first use (§2.3, §14).

---

## 2. Component A — WASM Extension (Rust)

### 2.1 Cargo.toml

```toml
[package]
name = "update-versions"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
zed_extension_api = "0.7"
```

### 2.2 extension.toml

> **Verified format** — validated against Zed's schema during POC.

```toml
id = "update-versions"
name = "Update Versions"
version = "0.1.0"
schema_version = 1
authors = ["..."]
description = "Inline version hints for package manifest files."
repository = "https://github.com/christophehurpeau/zed-update-versions"
themes = []
icon_themes = []
languages = []
capabilities = []

[lib]
kind = "Rust"
version = "0.7.0"

[language_servers.update-versions-lsp]
name = "Update Versions LSP"
languages = [
  "JSON",          # package.json, composer.json, dub.json
  "TOML",          # Cargo.toml, pyproject.toml
  "Plain Text",    # requirements.txt (fallback)
  "XML",           # pom.xml, *.csproj, *.fsproj, *.vbproj
  "YAML",          # pubspec.yaml, deno.json (YAML-like)
  "Dockerfile",
  "SDL",           # dub.sdl
]
```

**Schema notes learned from POC:**
- `[lib]` section with `kind = "Rust"` and `version` matching `zed_extension_api` is **required** — Zed will refuse to compile the extension without it.
- `capabilities` must be a **top-level array**, not a `[capabilities]` table. The `download_file` string value is not accepted by the v0.7 schema; use `capabilities = []` and rely on `zed::download_file` / `zed::make_file_executable` API calls in Rust code instead.
- `themes`, `icon_themes`, and `languages` empty arrays are required at the top level.

> **Note:** The language server attaches to files by inspecting `rootUri` and `textDocument/didOpen`  
> `uri` on the server side, selecting the appropriate provider per filename. Attaching to broad  
> language IDs (JSON, TOML, etc.) is intentional and safe because the LSP server returns empty  
> results for files that are not known manifest formats.

### 2.3 src/lib.rs

**Dev build** (POC / local development) — binary path is baked in at compile time via `CARGO_MANIFEST_DIR` because the WASM sandbox blocks arbitrary `std::fs` calls:

```rust
use zed_extension_api::{self as zed, Os, LanguageServerId, Worktree, Result};

struct UpdateVersionsExtension {
    cached_binary_path: Option<String>,
}

impl zed::Extension for UpdateVersionsExtension {
    fn new() -> Self {
        Self { cached_binary_path: None }
    }

    fn language_server_command(
        &mut self,
        _server_id: &LanguageServerId,
        _worktree: &Worktree,
    ) -> Result<zed::Command> {
        Ok(zed::Command {
            command: self.find_binary()?,
            args: vec![],
            env: vec![],
        })
    }
}

impl UpdateVersionsExtension {
    fn find_binary(&mut self) -> Result<String> {
        if let Some(ref path) = self.cached_binary_path {
            return Ok(path.clone());
        }
        let (os, _arch) = zed::current_platform();
        let binary_name = match os {
            Os::Windows => "update-versions-lsp.exe",
            _ => "update-versions-lsp",
        };
        // Absolute path baked in at compile time — WASM sandbox blocks fs checks.
        let path = format!("{}/bin/{binary_name}", env!("CARGO_MANIFEST_DIR"));
        self.cached_binary_path = Some(path.clone());
        Ok(path)
    }
}

zed::register_extension!(UpdateVersionsExtension);
```

**Production build** — replaces `find_binary` with a download-on-first-use approach:

```rust
    fn find_binary(&mut self) -> Result<String> {
        if let Some(ref path) = self.cached_binary_path {
            return Ok(path.clone());
        }
        let (os, arch) = zed::current_platform();
        let target = match (os, arch) {
            (Os::Mac,     zed::Architecture::Aarch64) => "aarch64-apple-darwin",
            (Os::Mac,     zed::Architecture::X8664)   => "x86_64-apple-darwin",
            (Os::Linux,   zed::Architecture::Aarch64) => "aarch64-unknown-linux-gnu",
            (Os::Linux,   zed::Architecture::X8664)   => "x86_64-unknown-linux-gnu",
            (Os::Windows, zed::Architecture::X8664)   => "x86_64-pc-windows-msvc",
            _ => return Err("Unsupported platform".to_string()),
        };
        let binary_file = if matches!(os, Os::Windows) {
            "update-versions-lsp.exe"
        } else {
            "update-versions-lsp"
        };
        let version = env!("CARGO_PKG_VERSION");
        let url = format!(
            "https://github.com/christophehurpeau/zed-update-versions\
             /releases/download/lsp-v{version}/update-versions-lsp-{target}.tar.gz"
        );
        let output_path = format!("bin/{binary_file}");
        zed::download_file(&url, &output_path, zed::DownloadedFileType::GzipTar)
            .map_err(|e| format!("Failed to download LSP server: {e}"))?;
        zed::make_file_executable(&output_path)?;
        self.cached_binary_path = Some(output_path.clone());
        Ok(output_path)
    }
```

> **Note:** `std::fs::metadata` cannot be used in the WASM sandbox to check whether the binary
> already exists on disk. The path is returned directly; Zed will report a clear error if the
> binary is absent.

The WASM component is intentionally thin. All logic lives in the LSP server.

---

## 3. Component B — LSP Server (Rust)

### 3.1 Technology Choices

| Concern | Choice | Rationale |
|---------|--------|-----------|
| Language | Rust (native binary) | No runtime required; maximum performance; cross-compiles to all target platforms |
| Protocol | `tower-lsp` crate | Mature async LSP server framework built on Tower middleware |
| HTTP | `reqwest` crate (async) | TLS via OS cert store, connection pooling, gzip |
| Semver | `semver` crate (v1) | Handles `^`, `~`, `>=`/`<` ranges — compatible with Cargo and npm syntax |
| TOML parsing | `toml` crate | Official Rust parser; exact Cargo semantics |
| XML parsing | `quick-xml` crate | For `.csproj`, `pom.xml` (Phase 4+) |
| YAML parsing | `serde_yaml` crate | For `pubspec.yaml`, `deno.json` (Phase 4+) |
| Async runtime | `tokio` | Required by `tower-lsp` and `reqwest` |

The LSP server is a **standalone native binary** distributed via GitHub Releases. The WASM extension downloads the correct build for the current platform at first use (§2.3, §14).

### 3.2 Server Entry Point (`src/main.rs`)

```rust
use tower_lsp::{LspService, Server};

mod backend;
mod cache;
mod config;
mod semver_utils;
mod providers;

#[tokio::main]
async fn main() {
    let stdin  = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| backend::Backend::new(client));
    Server::new(stdin, stdout, socket).serve(service).await;
}
```

### 3.3 Backend (`src/backend.rs`)

```rust
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use crate::{cache::VersionCache, config::ConfigManager, providers::ProviderRegistry};

pub struct Backend {
    client:    Client,
    cache:     Arc<VersionCache>,
    config:    Arc<RwLock<ConfigManager>>,
    registry:  Arc<ProviderRegistry>,
    documents: Arc<RwLock<HashMap<Url, String>>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                inlay_hint_provider: Some(OneOf::Right(
                    InlayHintServerCapabilities::Options(InlayHintOptions {
                        resolve_provider: Some(true),
                        ..Default::default()
                    }),
                )),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                // No executeCommand entries exposed; code actions apply edits directly.
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn shutdown(&self) -> Result<()> { Ok(()) }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.documents.write().await
            .insert(params.text_document.uri, params.text_document.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents.write().await
                .insert(params.text_document.uri, change.text);
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        self.config.write().await.update(&params.settings);
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri  = &params.text_document.uri;
        let docs = self.documents.read().await;
        let content = match docs.get(uri) { Some(c) => c.clone(), None => return Ok(None) };
        let config = self.config.read().await;
        let hints = self.registry.get_inlay_hints(uri, &content, &config, &self.cache).await;
        Ok(Some(hints))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri  = &params.text_document.uri;
        let docs = self.documents.read().await;
        let content = match docs.get(uri) { Some(c) => c.clone(), None => return Ok(None) };
        let config = self.config.read().await;
        let actions = self.registry
            .get_code_actions(uri, &content, params.range, &config, &self.cache)
            .await;
        Ok(Some(actions.into_iter().map(CodeActionOrCommand::CodeAction).collect()))
    }
}
```

### 3.4 Provider Architecture

```
lsp-server/src/
  main.rs                  ← entry point
  backend.rs               ← LanguageServer impl (tower-lsp)
  config.rs                ← ConfigManager (settings, toggles)
  cache.rs                 ← VersionCache (tokio::RwLock, per-process)
  semver_utils.rs          ← range comparisons, operator preservation
  providers/
    mod.rs                 ← ProviderRegistry + Provider trait
    npm.rs                 ← package.json              (Phase 1)
    cargo.rs               ← Cargo.toml                (Phase 1)
    pypi.rs                ← requirements.txt, pyproject.toml  (Phase 2+)
    composer.rs            ← composer.json             (Phase 3+)
    rubygems.rs            ← Gemfile                   (Phase 3+)
    deno.rs                ← deno.json, import_map.json (Phase 3+)
    nuget.rs               ← *.csproj etc.             (Phase 4+)
    maven.rs               ← pom.xml                   (Phase 4+)
    pub_dev.rs             ← pubspec.yaml              (Phase 4+)
    dub.rs                 ← dub.json, dub.sdl         (Phase 5+)
    docker.rs              ← Dockerfile                (Phase 5+)
```

### 3.5 Provider Trait

```rust
use async_trait::async_trait;
use tower_lsp::lsp_types::*;
use crate::{cache::VersionCache, config::ConfigManager};

pub struct ParsedDependency {
    pub name:            String,
    pub current_version: String,  // raw string from file (may include range operators)
    pub line:            u32,     // 0-based line number
    pub version_range:   Range,   // character range of the version string
}

pub struct VersionResult {
    pub stable:     Option<String>,
    pub prerelease: Option<String>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// File path substring patterns matched against the document URI.
    fn file_patterns(&self) -> &[&'static str];

    /// Parse the document content and return all dependency entries.
    fn parse_dependencies(&self, content: &str) -> Vec<ParsedDependency>;

    /// Fetch the latest stable (and optionally prerelease) version for a package.
    async fn fetch_latest_version(
        &self,
        name: &str,
        config: &ConfigManager,
        cache: &VersionCache,
    ) -> VersionResult;
}
```

---

## 4. File Type & Language Registration

Zed selects a language server based on the language of the open document, which is  
determined by file extension / name. No new language grammars need to be defined.

| File | Zed Language (existing) |
|------|------------------------|
| `package.json`, `composer.json`, `dub.json` | `JSON` |
| `Cargo.toml`, `pyproject.toml` | `TOML` |
| `requirements.txt` | `Plain Text` |
| `pubspec.yaml`, `deno.json` | `YAML` |
| `pom.xml`, `*.csproj`, `*.fsproj`, `*.vbproj`, `Directory.Packages.props` | `XML` |
| `import_map.json` | `JSON` |
| `Dockerfile` | `Dockerfile` |
| `dub.sdl` | `Plain Text` (or `SDL` if grammar available) |
| `Gemfile` | `Ruby` |

The LSP server's `ProviderRegistry` re-filters on the URI path:

```rust
fn get_provider<'a>(&'a self, uri: &str) -> Option<&'a dyn Provider> {
    self.providers.iter()
        .find(|p| p.file_patterns().iter().any(|pat| uri.contains(pat)))
        .map(|p| p.as_ref())
}
```

If no provider matches (e.g. a normal `.json` file), the server returns empty arrays and incurs no cost.

---

## 5. LSP Protocol Details

### 5.1 Inlay Hints (`textDocument/inlayHint`)

Each dependency line produces one `InlayHint`:

```typescript
{
  position: { line: dep.lineNumber, character: veryLargeNumber }, // end-of-line
  label: [
    {
      value: labelText,          // e.g. "✔ 4.2.1" or "↑ 5.0.0"
      // command field: not used — Zed does not wire up InlayHint commands.
    }
  ],
  kind: 1,        // InlayHintKind.Type — rendered after the line
  paddingLeft: true,
  tooltip: fullTooltipText,  // e.g. "Latest: 5.0.0 (major update)"
}
```

Label text mapping:

| State | Label text |
|-------|-----------|
| Up to date | `✔ {version}` |
| Patch update | `↑ {version}` |
| Minor update | `↑ {version}` |
| Major update | `↑ {version}` |
| Version not found | `✘ not found` (tooltip lists available higher versions as candidates) |
| Not found | `✘ not found` |
| Unsupported syntax | `⊘ unsupported` |
| Loading | `… fetching` (optimistic; replaced on resolution) |

> **Note on colours:** Zed applies a single theme colour to all inlay hints. Differentiation between  
> update severity is by symbol prefix and tooltip text only. See §15.3.

### 5.2 Code Actions (`textDocument/codeAction`)

When the cursor is on a dependency line, the server returns:

```typescript
// "Stable" update action
{
  title: "Update to 4.3.0 (minor)",
  kind: "quickfix",
  // Code actions include a WorkspaceEdit that directly applies the replacement.
  edit: { /* WorkspaceEdit replacing the versionRange with newVersionString */ }
}

// "Prerelease" update action
// Shown when: a prerelease exists that is strictly newer than the current constraint,
// AND (hidePrereleases is false OR the current version is itself a prerelease).
{
  title: "Update to 5.0.0-rc.1 (prerelease)",
  kind: "quickfix",
  edit: { /* WorkspaceEdit replacing the versionRange with the prerelease */ }
}
```

### 5.3 Toggle Commands (Execute Command)

| Command | Effect |
|---------|--------|
| (no executeCommand) | Code actions apply `WorkspaceEdit` directly; no `executeCommand` is exposed |

> **Note:** Zed does not surface LSP `workspace/executeCommand` entries in its command palette. Code actions apply `WorkspaceEdit` directly; no `executeCommand` is exposed for updates. Toggling hints on/off uses Zed's native **`editor: toggle inlay hints`** command, which is available in the command palette and bindable to a key.

There is no session-level prerelease toggle. Prerelease visibility is controlled by the `hidePrereleases` setting (see §8).

### 5.4 Workspace Configuration (`workspace/didChangeConfiguration`)

The LSP server listens for configuration changes at the `update-versions` key:

```json
{
  "update-versions": {
    "hidePrereleases": false,
    "logLevel": "error",
    "npm": { "registry": "https://registry.npmjs.org", "dependencyKeys": [...] },
    "cargo": {},
    "pypi": {},
    "composer": {},
    "pub": { "apiUrl": "https://pub.dev" },
    "nuget": { "sources": ["https://api.nuget.org/v3/index.json"] },
    "maven": {},
    "dub": {},
    "docker": {},
    "deno": {},
    "rubygems": {}
  }
}
```

---

## 6. Version Resolution per Provider

### 6.1 npm (`package.json`)

**Parsing:**  
Use `JSON.parse`. Walk the configured `dependencyKeys` (default: `["dependencies", "devDependencies", "peerDependencies", "optionalDependencies", "pnpm.overrides"]`).  
Skip entries whose value starts with `file:`, `link:`, `workspace:`, `github:`, or does not match a version range pattern → mark as `Unsupported`.

**Registry API:**
```
GET {registry}/{packageName}
Accept: application/vnd.npm.install-v1+json
```
Inexpensive "packument" endpoint. Extract `dist-tags.latest` for the stable version, and optionally the highest prerelease from `versions` keys.

For scoped packages (`@scope/name`), percent-encode the `/` → `%2F` in the URL.

**Range operators preserved on update:**  
`^1.0.0` → `^2.0.0`, `~1.0.0` → `~2.0.1`, `>=1.0.0` → `>=2.0.0`, bare `1.0.0` → `2.0.0`.

**Authentication:** Read `~/.npmrc` or project `.npmrc` for `_authToken` per registry host. Pass as `Authorization: Bearer {token}`.

---

### 6.2 Cargo (`Cargo.toml`)

**Parsing:**  
Use a TOML parser. Scan `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`, `[workspace.dependencies]`.  
Values may be:
- Plain string: `"1.0"` → version string  
- Table with `version` key: `{ version = "1.0", features = [...] }` → extract `version`  
- Table without `version` but with `path`/`git` → mark as `Unsupported`

**Registry API (crates.io):**
```
GET https://crates.io/api/v1/crates/{name}
User-Agent: update-versions/{version} (zed-extension)
```
Rate limit: 1 request/second without auth token. Implement a simple per-host request queue with 1 req/s throttle.  
Extract `crate.newest_version` (stable) and `crate.max_version` (includes pre-releases).

For private registries (alternate `registries` in `~/.cargo/config.toml`): read the config file and use the configured `api` URL.

---

### 6.3 PyPI (`requirements.txt`, `pyproject.toml`)

**requirements.txt Parsing:**  
Line-by-line. Skip comment lines (`#`), blank lines, options (`-r`, `-c`, `-e`, `--`).  
Supported specifiers: `==`, `>=`, `~=`, `!=`, `<`, `>`.  
Unsupported: VCS requirements (`git+https://...`), editable installs.  
Extract package name and version constraint range.

**pyproject.toml Parsing:**  
PEP 621: `[project].dependencies` array.  
Poetry: `[tool.poetry.dependencies]` table.  
Both are parsed to extract `name` + version specifier.

**Registry API:**
```
GET https://pypi.org/pypi/{name}/json
```
Extract `info.version` for stable. Prerelease: scan `releases` keys for highest version matching prerelease syntax (`.devN`, `aN`, `bN`, `rcN`).

---

### 6.4 Composer (`composer.json`)

**Parsing:**  
JSON. Walk `require` and `require-dev` maps.  
Skip `php`, `ext-*`, `lib-*` pseudo-packages.  
Version constraints use Composer syntax (`^`, `~`, `>=`, `||`, `*`, `dev-main`). Unsupported: `dev-*` constraints.

**Registry API:**
```
GET https://packagist.org/packages/{vendor}/{package}.json
```
Latest stable: `package.versions` — find the highest non-dev, non-prerelease tag.

---

### 6.5 Pub / Flutter (`pubspec.yaml`)

**Parsing:**  
YAML. Walk `dependencies` and `dev_dependencies` maps.  
Values may be a version constraint string, or a table with `version:` key (for git/path/sdk dependencies → Unsupported).

**Registry API:**
```
GET {pubApiUrl}/packages/{name}
```
Default `pubApiUrl`: `https://pub.dev`. Extract `latest.version` for stable. Prerelease versions come from `versions[].version` list.

---

### 6.6 NuGet (`*.csproj`, `*.fsproj`, `*.vbproj`, `Directory.Packages.props`)

**Parsing:**  
XML. Find `<PackageReference Include="Name" Version="x.y.z" />` elements.  
Also handle `<PackageVersion Include="Name" Version="..." />` (Central Package Management).

**Registry API (NuGet v3):**
```
GET {source}/flatcontainer/{id-lowercase}/index.json
```
Default source: `https://api.nuget.org/v3/index.json` (ServiceIndex).  
First, fetch the ServiceIndex to derive the `PackageBaseAddress` URL.  
Then fetch the flat container index to get the version list.  
Stable = highest non-prerelease. Prerelease = highest overall.

Multiple sources: try each source in order; take the first successful response.

---

### 6.7 Maven (`pom.xml`)

**Parsing:**  
XML. Find `<dependency>` elements with `<groupId>`, `<artifactId>`, `<version>`.  
Skip `<version>` that reference properties (`${...}`) → mark as `Unsupported`.  
Also check `<parent>` and `<plugins>/<plugin>` structures.

**Registry API (Maven Central):**
```
GET https://search.maven.org/solrsearch/select?q=g:{groupId}+AND+a:{artifactId}&core=gav&rows=20&wt=json
```
Parse response to find highest stable version. For prereleases, include entries with `-SNAPSHOT`, `-alpha`, `-beta`, `-RC` in the version string.

---

### 6.8 Dub (`dub.json`, `dub.sdl`)

**dub.json Parsing:** JSON. Walk `dependencies` object.

**dub.sdl Parsing:** Line-based. Match:
```
dependency "name" version="~>x.y.z"
```

**Registry API:**
```
GET https://code.dlang.org/api/packages/{name}/latest
GET https://code.dlang.org/api/packages/search?query={name}
```

---

### 6.9 Docker (`Dockerfile`)

**Parsing:**  
Regex-based line scanner. Match `FROM` instructions:
```
FROM image:tag [AS alias]
FROM image@digest          → Unsupported (digest pinning)
```
Multi-stage builds are supported — each `FROM` line is an independent entry.

**Registry API:**

*Docker Hub official images:*
```
GET https://hub.docker.com/v2/repositories/library/{image}/tags?page_size=100&ordering=last_updated
```

*Docker Hub user/org images:*
```
GET https://hub.docker.com/v2/repositories/{user}/{image}/tags?page_size=100&ordering=last_updated
```

*Generic OCI registry (ghcr.io, etc.):*  
Use the OCI Distribution Spec:
```
GET https://{registry}/v2/{name}/tags/list
```
with anonymous/token auth as required.

**Latest tag strategy:** `latest` is a tag, not a version. The server compares all semver-parseable tags to find the highest stable version. Tags that are not semver (e.g. `bookworm-slim`) are surfaced as `Unsupported`.

---

### 6.10 Deno (`deno.json`, `import_map.json`)

**Parsing:**  
JSON. Walk `imports` map. Values are URL strings such as:
- `https://deno.land/x/{module}@{version}/...` → extract module + version
- `npm:{package}@{version}` → extract npm package + version
- `jsr:@{scope}/{package}@{version}` → JSR package + version
- Bare `/` paths or `./` → Unsupported

**Registry APIs:**

*deno.land/x:*
```
GET https://apiland.deno.dev/v2/modules/{module}
```

*npm (via Deno npm specifiers):* → same as npm provider

*JSR:*
```
GET https://jsr.io/@{scope}/{package}/meta.json
```

---

### 6.11 Ruby Gems (`Gemfile`)

**Parsing:**  
Line-by-line regex. Match:
```ruby
gem 'name', '~> x.y'
gem 'name', '>= x.y', '< z'
gem 'name'                   # no version constraint → fetch latest
gem 'name', git: '...'       # → Unsupported
gem 'name', path: '...'      # → Unsupported
```

**Registry API:**
```
GET https://rubygems.org/api/v1/gems/{name}.json
```
Extract `version` (stable) and `version` from gems with prerelease version strings.

---

## 7. Semver Handling

The server uses the [`semver`](https://crates.io/crates/semver) Rust crate as the canonical semver evaluator for all ecosystems (adapting syntax where necessary).

### 7.1 Range Evaluation

For each dependency:
1. Normalize the version constraint to a `semver`-compatible range string.
2. Parse into `semver::VersionReq`; extract the **base version** (minimum version installed by a fresh lockfile) from the constraint string, e.g. `~3.8.0` → `3.8.0`.
3. Iterate every known stable version. For each one:
   - If it satisfies `VersionReq`, record it as `in_range` (used in the tooltip to show the highest in-range version).
   - **Independently**, if it is greater than the base version, classify it as a `patch`, `minor`, or `major` candidate.
4. If any candidate was found → state = `UpdateAvailable`; the hint shows the most significant candidate.
5. If no candidate was found → state = `UpToDate`.

The key design decision: `in_range` membership does **not** suppress the update classification. A constraint like `~3.8.0` installs `3.8.0` in a fresh lockfile; if `3.8.1` exists it is a patch update regardless of being within the range, because the lockfile will not advance to it unless explicitly refreshed.

### 7.2 Operator Preservation on Update

When applying an update, the new version string should respect the original operator:

| Input | New version | Output |
|-------|-------------|--------|
| `^1.2.0` | `2.0.0` | `^2.0.0` |
| `~1.2.0` | `1.2.5` | `~1.2.5` |
| `>=1.0.0 <2.0.0` | `2.0.0` | `>=2.0.0 <3.0.0` (naive increment of upper bound) |
| `1.2.0` (bare) | `2.0.0` | `2.0.0` |
| `~>1.2` (Ruby/Gemfile) | `1.3.0` | `~>1.3` |

Rules:
- Single-operator ranges: replace the version component, keep the operator.
- Range pairs like `>=x <y`: replace both bounds proportionally.
- Constraints too complex to parse safely: present the new bare version and add a tooltip warning "operator not preserved".

---

## 8. Settings & Configuration

### 8.1 Zed Settings Key

Users configure the extension in Zed's `settings.json` under the `lsp` key:

```json
{
  "lsp": {
    "update-versions-lsp": {
      "settings": {
        "hidePrereleases": false,
        "logLevel": "error",
        "npm": {
          "registry": "https://registry.npmjs.org",
          "scopeRegistries": {
            "@mycompany": "https://npm.mycompany.com"
          },
          "dependencyKeys": [
            "dependencies",
            "devDependencies",
            "peerDependencies",
            "optionalDependencies",
            "pnpm.overrides"
          ]
        },
        "pub": {
          "apiUrl": "https://pub.dev"
        },
        "nuget": {
          "sources": ["https://api.nuget.org/v3/index.json"]
        }
      }
    }
  }
}
```

### 8.2 ConfigManager

```rust
pub struct ConfigManager {
  hide_prereleases: AtomicBool,  // default: settings.hide_prereleases
  pub settings: tokio::sync::RwLock<Settings>,
}

impl ConfigManager {
  pub fn hide_prereleases(&self) -> bool { /* Ordering::Relaxed load */ }

  pub async fn update_settings(&self, new_settings: Settings) {
    // Called on workspace/didChangeConfiguration.
  }
}
```

There is no `hints_enabled` flag. Hint visibility is controlled entirely by Zed's own inlay hints setting via the built-in `editor: toggle inlay hints` command (`editor::ToggleInlayHints`).

---

## 9. Show / Hide Toggle

### 9.1 Default Behaviour

The LSP server always returns hints when the editor requests them. Hint visibility is controlled by Zed itself.

### 9.2 Toggling

Use Zed's built-in **`editor: toggle inlay hints`** command (`editor::ToggleInlayHints`). It is available in the command palette (`Cmd+Shift+P`) and can be bound to a key:

```json
{ "bindings": { "ctrl-shift-v": "editor::ToggleInlayHints" } }
```

This toggles all inlay hints globally (all LSP servers), which is consistent with Zed's design. There is no per-extension toggle — confirmed API limitation of `zed_extension_api` v0.7 (see §15.1).

---

## 10. One-Click Updates

### 10.1 Via InlayHintLabelPart.command — NOT SUPPORTED in Zed

LSP 3.17 allows each `InlayHintLabelPart` to carry a `command`. Zed renders inlay hints but does **not** wire up `InlayHintLabelPart.command` — clicking hints has no effect as of March 2026 (confirmed by POC). This field should not be populated.

### 10.2 Via Code Actions (primary interaction model)

Code actions are the **confirmed and sole update mechanism**. The update is available as a **code action** on the dependency line. Users invoke it via:
- `editor: Toggle Code Actions` (default keybinding `cmd-.`)
- The lightbulb icon that appears when the cursor rests on the line

The code action payload is identical to the hint command; the update is applied via `workspace/applyEdit`.

### 10.3 Version String Replacement Logic

```rust
fn build_replacement_text(original: &str, new_version: &str) -> String {
    // Find where the version number starts and preserve everything before it (operators)
    let operator_end = original
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(0);
    let operator = &original[..operator_end];
    format!("{operator}{new_version}")
}
```

---

## 11. Caching Strategy

### 11.1 In-Memory Cache

```rust
// CacheKey format: "{provider}:{package_name}:{show_prereleases}"
pub type CacheKey = String;

pub struct VersionCache {
    store: tokio::sync::RwLock<HashMap<CacheKey, CacheEntry>>,
}

impl VersionCache {
    pub async fn get(&self, key: &str) -> Option<VersionResult> { ... }
    pub async fn set(&self, key: CacheKey, result: VersionResult, ttl: Duration) { ... }
    pub async fn invalidate(&self, key: &str) { ... }
}
```

- TTL: **5 minutes** per entry (configurable).  
- Cache is in-process; it is discarded when the LSP server exits (when Zed closes).  
- There is no disk persistence. Re-fetches happen on next server start.

### 11.2 Fetch Deduplication

Multiple concurrent requests for the same package are deduplicated via a pending-promise map:

```rust
// Inflight deduplication via broadcast channel
inflight: tokio::sync::Mutex<HashMap<CacheKey, tokio::sync::broadcast::Sender<VersionResult>>>,

pub async fn resolve<F, Fut>(&self, key: &str, fetcher: F) -> VersionResult
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = VersionResult>,
{
    if let Some(cached) = self.get(key).await { return cached; }
    let mut inflight = self.inflight.lock().await;
    if let Some(tx) = inflight.get(key) {
        let mut rx = tx.subscribe();
        drop(inflight);
        return rx.recv().await.unwrap_or_default();
    }
    let (tx, _) = tokio::sync::broadcast::channel(1);
    inflight.insert(key.to_string(), tx.clone());
    drop(inflight);
    let result = fetcher().await;
    self.set(key.to_string(), result.clone(), Duration::from_secs(300)).await;
    tx.send(result.clone()).ok();
    self.inflight.lock().await.remove(key);
    result
}
```

### 11.3 Lazy Fetching

Registry requests are only made when the editor requests inlay hints (`textDocument/inlayHint`). If the user hides hints via Zed's `editor: toggle inlay hints`, Zed stops sending hint requests and no registry calls are made.

---

## 12. Authentication

The LSP server reads credential files from the filesystem using Rust's `std::fs`. The server process inherits the Zed user's environment variables and home directory.

| Registry | Credential source |
|----------|------------------|
| npm / private | `~/.npmrc`, project `.npmrc` — parse `_authToken` per registry host |
| Cargo | `~/.cargo/credentials.toml` — parse `token` per registry name |
| NuGet | `~/.nuget/NuGet/NuGet.Config`, project `nuget.config` — parse `<add key="UserName">` / `<add key="Password">` (or `ClearTextPassword`) |
| Docker private registries | `~/.docker/config.json` — parse `auths[host].auth` (base64 Basic) |
| All others | No credential support in v1 |

No credentials are logged or transmitted beyond the intended registry endpoint. Credential parsing is intentionally read-only.

---

## 13. Logging

The LSP server writes structured JSON log lines to **stderr**, which Zed captures in its output/log panel.

```rust
fn log(level: &str, msg: &str) {
    if should_log(level) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        eprintln!(r#"{{"ts":{ts},"level":"{level}","msg":"{msg}"}}");
    }
}
```

Log level controlled by `settings.logLevel` (default `"error"`).  
In `debug` mode, every registry request, response status, and cache hit/miss is logged.

---

## 14. Build & Distribution

### 14.1 Development Build

**LSP server (native binary):**
```bash
cd lsp-server
cargo build --release
# binary: lsp-server/target/release/update-versions-lsp
```

**WASM extension:**
```bash
cargo build --target wasm32-wasip1 --release
```

### 14.2 Cross-Compilation Targets

| Platform | Rust target triple |
|----------|-------------------|
| macOS Apple Silicon | `aarch64-apple-darwin` |
| macOS Intel | `x86_64-apple-darwin` |
| Linux x86_64 | `x86_64-unknown-linux-gnu` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` |
| Windows x86_64 | `x86_64-pc-windows-msvc` |

### 14.3 Extension Layout (published)

```
update-versions/
  extension.toml
  Cargo.toml
  Cargo.lock
  src/
    lib.rs
```

No bundled binary is shipped with the extension. The WASM wrapper downloads the correct binary from GitHub Releases at first use (see §2.3).

### 14.4 Zed Extension Publishing

```bash
# in zed-industries/extensions repo:
git submodule add https://github.com/christophehurpeau/zed-update-versions extensions/update-versions
```

The `extension.toml` schema version must match the current Zed API (`schema_version = 1`).

### 14.5 CI/CD (GitHub Actions)

Two separate workflows:

**`ci.yml`** — runs on every push/PR:
1. `cargo test` in `lsp-server/` — unit tests for providers, semver utils, caching
2. `cargo build --target wasm32-wasip1 --release` — validates WASM compilation

**`release-lsp.yml`** — triggered by a `lsp-v*` tag:

```yaml
strategy:
  matrix:
    include:
      - { os: macos-latest,   target: aarch64-apple-darwin }
      - { os: macos-latest,   target: x86_64-apple-darwin }
      - { os: ubuntu-latest,  target: x86_64-unknown-linux-gnu }
      - { os: ubuntu-latest,  target: aarch64-unknown-linux-gnu, cross: true }
      - { os: windows-latest, target: x86_64-pc-windows-msvc }
steps:
  - uses: dtolnay/rust-toolchain@stable
    with: { targets: "${{ matrix.target }}" }
  - name: Build
    run: |
      cd lsp-server
      cargo build --release --target ${{ matrix.target }}
  - name: Package
    run: |
      tar czf update-versions-lsp-${{ matrix.target }}.tar.gz \
        -C lsp-server/target/${{ matrix.target }}/release \
        update-versions-lsp
  - uses: softprops/action-gh-release@v1
    with:
      files: "update-versions-lsp-${{ matrix.target }}.tar.gz"
```

**`release-extension.yml`** — triggered by an `extension-v*` tag:
- Opens PR to `zed-industries/extensions` via [`huacnlee/zed-extension-action`](https://github.com/huacnlee/zed-extension-action)

---

## 15. Feasibility Deviations from Feature Spec

### 15.1 Toolbar Buttons — NOT FEASIBLE

**Spec says:** A "V" icon button and a tag icon button in the editor tab bar.

**Reality:** The Zed extension API (`zed_extension_api` v0.7) provides no mechanism to inject custom UI elements of any kind (toolbar, tab bar, status bar). Additionally, Zed does **not** surface LSP `workspace/executeCommand` entries in the command palette — only native Zed actions appear there.

**Adopted approach:**
- **Hint toggle:** Removed from the extension entirely. Users use Zed's native **`editor: toggle inlay hints`** (`editor::ToggleInlayHints`) command, which is palette-accessible and keybindable. The LSP server always serves hints when asked.
- **Prerelease toggle:** Exposed as a persistent `Command`-type code action (appears in `Cmd+.` menu on any supported file). Session state is managed in-process by `ConfigManager`.

### 15.2 Clickable Inline Hints — NOT SUPPORTED (confirmed by POC)

**Spec says:** Clicking on an annotation applies the update.

**Reality:** Confirmed via POC (March 2026) — Zed renders inlay hints but does **not** wire up `InlayHintLabelPart.command` to `workspace/executeCommand`. Clicking hint text has no effect.

**Adopted approach:** `textDocument/codeAction` is the sole update interaction. Users place the cursor on a dependency line and press `cmd+.` (or click the lightbulb) to see the available update actions. Each action applies a `WorkspaceEdit` replacing the version number while preserving the operator prefix. The hint label is read-only and informational only.

### 15.3 Per-Hint Colors — NOT FEASIBLE via LSP

**Spec says:** Green / orange / red / grey per version state; user-configurable.

**Reality:** Zed renders all inlay hints with the theme's single `hints` colour. LSP does not define a per-hint foreground colour extension. There is no extension point available.

**Alternative:** 
- Status is differentiated by prefix symbol: `✔` (up to date), `↑` (update available), `✘` (not found), `⊘` (unsupported), `…` (loading)
- Tooltip text provides the full description
- Users who want coloured output can configure their Zed theme's `hint` colour globally

### 15.4 Dist Tags / Release Channels — Partial

**Spec says:** Show named dist channels (`latest`, `next`, `beta`) alongside the version.

**Feasibility:** The npm packument (`{registry}/{package}`) includes `dist-tags`. The server can include the tag name in the hint label or tooltip. Example: `↑ 5.0.0-beta.1 [next]`.

Full channel filtering (tag whitelist per scope) is implementable in v1 but adds configuration surface area. Recommended for v1.1.

### 15.5 `requirements.txt` — No Language Association by Default

Zed does not associate `requirements.txt` with a particular language by default (it falls through to "Plain Text"). The LSP server will still receive `textDocument/didOpen` for this file if "Plain Text" is in the server's language list. However, users may need to configure `file_types`:

```json
{
  "file_types": {
    "Plain Text": ["requirements.txt", "requirements*.txt"]
  }
}
```

The extension should document this in its README.

### 15.6 Glob File Patterns (`*.csproj`) — Requires `file_types` Config

Zed's `extension.toml` `languages` array references existing language names, not glob patterns. For `.csproj`, `.fsproj`, `.vbproj` files, Zed already recognises these as XML. The LSP server matches on the URI filename suffix.

### 15.7 Maven Local Repository — Deferred

**Spec says:** Support for local `.m2/repository` resolution.

Reading the local `.m2/repository` is straightforward via `std::fs`. However, parsing Maven metadata XML from the local cache is complex. Deferred to v1.1 in favour of Maven Central HTTP API.

---

## 16. Phasing Recommendation

### Phase 1 — MVP (v0.1)

- Infrastructure: native Rust LSP server, cache, toggle commands, code actions, hint rendering
- npm (`package.json`)
- Cargo (`Cargo.toml`)
- ✅ **POC complete:** `textDocument/inlayHint` confirmed working; `textDocument/codeAction` confirmed working; `InlayHintLabelPart.command` confirmed **not** supported — code actions are the sole update path (see §15.2)

### Phase 2 — Python & TOML (v0.2)

- PyPI (`requirements.txt`, `pyproject.toml`)

### Phase 3 — Web Ecosystem (v0.3)

- Composer (`composer.json`)
- Deno (`deno.json`, `import_map.json`)
- Ruby Gems (`Gemfile`)

### Phase 4 — Enterprise & JVM (v0.4)

- NuGet (`.csproj`, `.fsproj`, `.vbproj`, `Directory.Packages.props`)
- Maven (`pom.xml`)
- Pub / Flutter (`pubspec.yaml`)

### Phase 5 — Systems & Infra (v0.5)

- Dub (`dub.json`, `dub.sdl`)
- Docker (`Dockerfile`)
- Private registry authentication hardening
- Dist tag / release channel filtering

---

## 17. Repository Structure

> The flat layout originally described was revised during the POC. The WASM extension
> and the LSP server are kept as separate crate roots in their own subdirectories.

```
zed-update-versions/
├── extension/                         ← WASM extension crate
│   ├── Cargo.toml                     ← crate-type = ["cdylib"]
│   ├── extension.toml                 ← Zed extension manifest
│   ├── src/
│   │   └── lib.rs
│   └── bin/                           ← gitignored; populated by `make install-dev`
│       └── update-versions-lsp
│
├── lsp-server/                        ← Native LSP server (separate Rust project)
│   ├── Cargo.toml
│   ├── Cargo.lock
│   └── src/
│       ├── main.rs                    ← entry point (POC: all in one file)
│       ├── backend.rs                 ← LanguageServer impl  (Phase 1)
│       ├── cache.rs                   ← VersionCache          (Phase 1)
│       ├── config.rs                  ← ConfigManager         (Phase 1)
│       ├── semver_utils.rs            ← range comparisons     (Phase 1)
│       └── providers/
│           ├── mod.rs
│           ├── npm.rs                 ← Phase 1
│           ├── cargo.rs               ← Phase 1
│           ├── pypi.rs                ← Phase 2
│           ├── composer.rs            ← Phase 3
│           ├── rubygems.rs            ← Phase 3
│           ├── deno.rs                ← Phase 3
│           ├── nuget.rs               ← Phase 4
│           ├── maven.rs               ← Phase 4
│           ├── pub_dev.rs             ← Phase 4
│           ├── dub.rs                 ← Phase 5
│           └── docker.rs              ← Phase 5
│
├── tests/
│   └── fixtures/
│       ├── package.json
│       └── Cargo.toml
│
├── Makefile                           ← `make install-dev` builds LSP + copies to extension/bin/
├── .gitignore
├── .github/
│   └── workflows/
│       ├── ci.yml
│       ├── release-lsp.yml
│       └── release-extension.yml
│
├── LICENSE
├── README.md
├── SPEC.md
└── SPEC_TECHNICAL.md
```

---

## Appendix A — LSP Capabilities Checklist

| LSP Feature | Used For | Zed Support |
|-------------|----------|-------------|
| `textDocument/inlayHint` | Version annotations | ✅ Confirmed |
| `inlayHint/refresh` | Re-render after toggle | ✅ Confirmed (sent by server) |
| `InlayHintLabelPart.command` | Click-to-update | ❌ Not supported (confirmed by POC) |
| `textDocument/codeAction` | Update via code action | ✅ Confirmed |
| `workspace/applyEdit` | Apply version replacement | ✅ Confirmed |
| `workspace/executeCommand` | Toggle commands | ✅ Confirmed |
| `workspace/didChangeConfiguration` | Settings update | ✅ Confirmed |
| `textDocument/hover` | Extended tooltip | ✅ Confirmed (optional) |

## Appendix B — Security Considerations

- **No credentials stored in extension settings.** Authentication tokens are read  
  from standard tooling config files (`~/.npmrc`, etc.) using the same trust model  
  as the package manager itself.
- **HTTPS only** for all registry requests. Plain HTTP registries must be explicitly  
  opted in via the `npm.registry` / `nuget.sources` settings.
- **No telemetry.** The extension makes no requests except to the configured registries.
- **Input validation.** Package names extracted from manifest files are validated  
  against each registry's naming rules before being interpolated into URLs, preventing  
  path traversal or injection in HTTP requests.
- **Rate limiting.** Per-host request throttling (especially for crates.io) is enforced  
  to comply with registry terms of service.
