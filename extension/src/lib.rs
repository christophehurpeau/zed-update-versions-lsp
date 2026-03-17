//! update-versions — WASM extension
//!
//! Thin wrapper that locates (and, on first use, downloads) the native LSP
//! server binary and hands its path to Zed.  All intelligence lives in the
//! `update-versions-lsp` binary.
//!
//! # Development workflow
//!
//! 1. Build the native binary:   `make build-lsp`
//! 2. Copy it into the bin dir:  `make install-dev`
//! 3. In Zed: "zed: install dev extension" → pick the `extension/` folder.
//!
//! When developing locally the binary is already present at `bin/` so the
//! download path is never triggered.  For production installs Zed will
//! download the correct pre-built binary from GitHub Releases automatically.

use zed_extension_api::{
    self as zed, Architecture, DownloadedFileType, LanguageServerId,
    LanguageServerInstallationStatus, Os, Result, Worktree,
};

/// Version of the LSP server to download.  Kept in sync with the workspace
/// version — both crates are released together via release-plz.
const LSP_VERSION: &str = env!("CARGO_PKG_VERSION");

struct UpdateVersionsExtension {
    /// Cache the resolved binary path within the extension host's lifetime.
    cached_binary_path: Option<String>,
}

impl zed::Extension for UpdateVersionsExtension {
    fn new() -> Self {
        UpdateVersionsExtension {
            cached_binary_path: None,
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        _worktree: &Worktree,
    ) -> Result<zed::Command> {
        Ok(zed::Command {
            command: self.language_server_binary(language_server_id)?,
            args: vec![],
            env: vec![],
        })
    }
}

impl UpdateVersionsExtension {
    /// Return the path to the LSP binary, downloading it from GitHub Releases
    /// the first time it is needed.
    fn language_server_binary(&mut self, language_server_id: &LanguageServerId) -> Result<String> {
        // Return the cached path if the binary is still present on disk.
        if let Some(path) = &self.cached_binary_path {
            if std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
                return Ok(path.clone());
            }
        }

        let (os, arch) = zed::current_platform();
        let ext = match os {
            Os::Windows => ".exe",
            _ => "",
        };
        let binary_name = format!("update-versions-lsp{ext}");
        // Relative path: where production downloads land (work dir).
        let binary_path = format!("bin/{binary_name}");

        // If not already present in the work dir, download the appropriate
        // release asset from GitHub Releases.
        if !std::fs::metadata(&binary_path).is_ok_and(|m| m.is_file()) {
            let (os_str, arch_str) = match (os, arch) {
                (Os::Mac, Architecture::Aarch64) => ("apple-darwin", "aarch64"),
                (Os::Mac, Architecture::X8664) => ("apple-darwin", "x86_64"),
                (Os::Linux, Architecture::Aarch64) => ("unknown-linux-gnu", "aarch64"),
                (Os::Linux, Architecture::X8664) => ("unknown-linux-gnu", "x86_64"),
                (Os::Windows, Architecture::X8664) => ("pc-windows-msvc", "x86_64"),
                _ => {
                    return Err(format!(
                        "prebuilt binaries are not available for {arch:?} {os:?}"
                    ));
                }
            };

            let release_asset = format!("update-versions-lsp-{arch_str}-{os_str}{ext}");
            let url = format!(
                "https://github.com/christophehurpeau/zed-update-versions-lsp/releases/download/update-versions-lsp-v{LSP_VERSION}/{release_asset}"
            );

            zed::set_language_server_installation_status(
                language_server_id,
                &LanguageServerInstallationStatus::Downloading,
            );

            zed::download_file(&url, &binary_path, DownloadedFileType::Uncompressed)
                .map_err(|e| format!("failed to download {release_asset}: {e}"))?;
            zed::make_file_executable(&binary_path)
                .map_err(|e| format!("failed to make binary executable: {e}"))?;
        }

        self.cached_binary_path = Some(binary_path.clone());
        Ok(binary_path)
    }
}

zed::register_extension!(UpdateVersionsExtension);
