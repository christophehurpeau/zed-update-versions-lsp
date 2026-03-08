# zed-update-versions — Feature Specification

> A Zed editor extension that shows the latest available version for each dependency declared in a project's package manifest, directly inline in the editor.

---

## Overview

zed-update-versions makes dependency management effortless by surfacing version information right where you declare your dependencies. Instead of switching to a terminal or browser to check whether a package is up to date, the editor shows you the current latest version alongside each entry — and lets you update it with a single click.

The core philosophy is **transparent awareness**: version hints are shown automatically whenever a supported manifest is open, leveraging Zed's built-in inlay hint toggle to hide them when not needed.

---

## 1. Supported Package Ecosystems

The extension activates automatically when one of the following manifest files is open in the editor:

| Ecosystem | File(s) |
|-----------|---------|
| **npm / Node.js** | `package.json` |
| **Rust (Cargo)** | `Cargo.toml` |
| **Python (PyPI)** | `requirements.txt`, `pyproject.toml` |
| **PHP (Composer)** | `composer.json` |
| **Dart / Flutter** | `pubspec.yaml` |
| **.NET / NuGet** | `*.csproj`, `*.fsproj`, `*.vbproj`, `Directory.Packages.props` |
| **Java (Maven)** | `pom.xml` |
| **D lang (Dub)** | `dub.json`, `dub.sdl` |
| **Docker** | `Dockerfile` (base image tags) |
| **Deno** | `deno.json`, `import_map.json` |
| **Ruby (Gems)** | `Gemfile` |

Each ecosystem is treated as an independent provider. Only providers relevant to the open file are active.

---

## 2. Core Feature: Inline Version Hints

When activated, the extension inserts a small **inline annotation** (gutter decoration or virtual text) next to each dependency line. The annotation communicates the relationship between the version currently declared and the latest version available in the registry.

### 2.1 Version States

Each package line is annotated with one of the following states:

| State | Meaning | Example label |
|-------|---------|---------------|
| **Up to date** | The declared version satisfies the latest release | `✔ 4.2.1` |
| **Patch update available** | A newer patch version exists (e.g. bug fixes) | `↑ 4.2.3` |
| **Minor update available** | A newer minor version exists (new features, backwards-compatible) | `↑ 4.3.0` |
| **Major update available** | A newer major version exists (potentially breaking changes) | `↑ 5.0.0` |
| **Version not found** | The declared version constraint does not match any version in the registry; higher versions may still be available as code-action suggestions | `✘ not found` |
| **Not found** | Package could not be found in the registry | `✘ Not found` |
| **Unsupported** | Version constraint syntax is not recognised (e.g. git URLs) | `⊘ Unsupported` |
| **Loading** | Fetch in progress | `… fetching` |

Colours for each state are configurable to ensure good contrast with any editor theme.

### 2.2 Version Range Awareness

The extension understands semantic version range operators (`^`, `~`, `>=`, `*`, etc.) and interprets them correctly.

Update classification is based on the **minimum version** the constraint installs (i.e. the version that would be recorded in a fresh lockfile), not on whether the latest version satisfies the range. This matters because a lockfile pin is only updated when it is explicitly refreshed — the declared range alone does not guarantee an up-to-date install.

For example, `~3.8.0` allows `3.8.x`, but a lockfile created from it will record `3.8.0`. If `3.8.1` is released, the extension reports a **patch update** even though `3.8.1` is technically within the declared range.

Only when no version newer than the constraint's minimum exists is the dependency shown as **up to date**.

---

## 3. Show / Hide Toggle

Version hints are **shown by default** whenever a supported manifest file is open.

### Controls
- Use Zed's built-in **"editor: toggle inlay hints"** command (Command Palette, `Cmd+Shift+P`) to show or hide all inlay hints globally. This is a standard Zed action and can be bound to any key in `keymap.json`.
- There is no per-extension toggle: enabling/disabling hints applies to all LSP servers uniformly, which is consistent with Zed's design.

---

## 4. Prerelease Versions

Prerelease versions (alpha, beta, rc, next, etc.) are surfaced automatically, with smart defaults:

- A **prerelease update code action** (`Cmd+.` on a dependency line) is shown whenever a prerelease version is available that is strictly newer than the currently declared version.
- If the currently declared version is itself a prerelease (e.g. `^1.0.0-alpha.1`), the prerelease update action is always shown, and the tooltip also surfaces the latest prerelease.
 - A setting (`hidePrereleases`) can be set to `true` to suppress the prerelease code action — **except** when the current version is already a prerelease, in which case the action is always shown regardless of the setting.
 - There is no session toggle; prerelease visibility is controlled entirely by the `hidePrereleases` setting.

---

## 5. One-Click Version Updates

Every inline annotation is **clickable**. Clicking on a version suggestion replaces the version string in the file with the suggested version.

- The replacement preserves the original version range operator where sensible. For example, if you had `^1.2.0` and the suggestion is `2.0.0`, the result would be `^2.0.0`.
- No terminal command or manual editing is required. The file is modified in place.

---

## 6. Dist Tag / Release Channel Support (npm and similar)

For ecosystems that support named release channels (e.g. npm dist tags like `latest`, `next`, `beta`, `legacy`), the extension can surface these alongside the version number.

- A configurable **tag filter** lets users whitelist only the channels they care about (e.g. only show `next` and `beta` tags, suppress everything else).
- Tags are shown in addition to the resolved version number so you can see both the label and the exact version it points to.

---

## 7. Provider-Specific Configuration

Each ecosystem provider has its own configurable options. Common examples:

### npm
- Custom registry URL (for private registries or mirrors).
- Scoped package registry overrides (e.g. `@mycompany/*` resolves against an internal registry).
- GitHub package URL support (e.g. `github:owner/repo#semver:x.x.x`).
- Support for module aliasing (e.g. `"@npm:some-package"`).
- Configurable list of dependency property keys to scan (e.g. `dependencies`, `devDependencies`, `peerDependencies`, `pnpm.overrides`).

### .NET / NuGet
- Custom NuGet source URLs (v3 compatible endpoints).
- Multiple sources supported; if a package is not found in one source it falls back to the next.
- Tag filter for noisy pre-release channels.
- Support for Central Package Versioning (`Directory.Packages.props`).

### Dart / Flutter (Pub)
- Configurable Pub API URL (for self-hosted registries or mirrors).

### Maven
- Support for local repository resolution (`.m2/repository`).

---

## 8. Authentication / Authorization

For private registries that require credentials, the extension reads authentication tokens from the relevant tooling configuration files (e.g. `.npmrc`, `nuget.config`). No passwords are stored or managed by the extension itself.

A documentation page explains how to configure credentials for each supported provider.

---

## 9. Diagnostic / Status Colours

The inline annotations use distinct colours to help you quickly scan a manifest and identify what needs attention:

- **Green** — dependency is up to date or satisfied.
- **Orange** — an update is available (minor or patch).
- **Red** — package is missing from the registry, or the declared version could not be resolved.
- **Grey / muted** — unsupported version syntax, loading state, or feature is disabled.

All colours are overridable via settings to accommodate accessibility needs and custom themes.

---

## 10. Logging and Diagnostics

The extension writes structured log output to a dedicated output channel, making it possible to diagnose resolution failures without affecting the editing experience.

- Log level is configurable (`error`, `warn`, `info`, `debug`).
- Debug mode produces verbose output useful when reporting issues or investigating unexpected behavior with a particular registry or package.

---

## 11. Performance Considerations

- Version data is **fetched lazily**: hints are only loaded when the editor requests them (on file open or after edits).
- Results are **cached** per session to avoid redundant network requests when switching between files.
- Fetches are performed in the background without blocking the editor.

---

## 12. Settings Summary

| Setting | Description | Default |
|---------|-------------|---------||
| `hidePrereleases` | Suppress prerelease update suggestions (ignored when current version is a prerelease) | `false` |
| `logLevel` | Verbosity of the output log (`error` / `info` / `debug`) | `error` |
| `npm.registry` | Custom npm registry URL | *(npm default)* |
| `npm.dependencyKeys` | JSON keys to scan for dependencies | `["dependencies", "devDependencies", ...]` |
| `dotnet.sources` | List of NuGet source endpoints | *(dotnet CLI config)* |
| `pub.apiUrl` | Pub registry API URL | *(pub.dev default)* |
| `[provider].dependencyKeys` | Keys to scan in that provider's manifest | *(provider defaults)* |

---

## 13. Out of Scope (for v1)

The following features are explicitly **not** planned for the initial version to keep scope manageable:

- Automatic bulk updates ("update all packages").
- Vulnerability / security advisory information.
- Changelog diffing between current and latest version.
- License information display.
- Lockfile awareness (e.g. comparing installed vs. declared version).

These may be considered for future iterations based on user feedback.

---

## Appendix: Glossary

| Term | Meaning |
|------|---------|
| **inline hint** | A small annotation displayed inside the editor, attached to a specific line, without modifying the file. |
| **SemVer** | Semantic Versioning — a versioning convention using `MAJOR.MINOR.PATCH` numbers. |
| **Dist tag** | A named alias pointing to a specific version in a registry (npm concept, e.g. `latest`, `next`). |
| **Prerelease** | A version that is not yet considered stable, often suffixed with `-alpha`, `-beta`, `-rc`, etc. |
| **Provider** | The module within the extension responsible for one specific package ecosystem. |
