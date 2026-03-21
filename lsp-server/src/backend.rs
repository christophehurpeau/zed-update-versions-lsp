use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{debug, info};

use crate::cache::{VersionCache, VersionResult};
use crate::config::{ConfigManager, Settings};
use crate::providers::{DependencyStatus, ParsedDependency, ProviderRegistry, ResolvedDependency};
use crate::version_utils;

type LogReloadHandle =
    tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>;

pub struct Backend {
    client: Client,
    documents: Arc<RwLock<HashMap<Url, String>>>,
    config: Arc<ConfigManager>,
    cache: Arc<VersionCache>,
    providers: Arc<RwLock<ProviderRegistry>>,
    log_reload_handle: Arc<LogReloadHandle>,
}

/// Build a fresh [`ProviderRegistry`] from the given settings.
/// Called at startup and whenever `workspace/didChangeConfiguration` fires.
fn build_providers(settings: &Settings) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.register(Arc::new(crate::providers::npm::NpmProvider::new(
        settings.npm.registry.clone(),
        settings.npm.dependency_keys.clone(),
    )));
    registry.register(Arc::new(crate::providers::cargo::CargoProvider::new(
        settings.cargo.dependency_keys.clone(),
    )));
    registry.register(Arc::new(crate::providers::pypi::PypiProvider::new()));
    registry.register(Arc::new(crate::providers::composer::ComposerProvider::new()));
    registry.register(Arc::new(crate::providers::rubygems::RubyGemsProvider::new()));
    registry.register(Arc::new(crate::providers::deno::DenoProvider::new(
        settings.npm.registry.clone(),
    )));
    registry
}

impl Backend {
    pub fn new(client: Client, log_reload_handle: Arc<LogReloadHandle>) -> Self {
        let settings = Settings::default();
        let config = ConfigManager::new();
        let cache_ttl = Duration::from_secs(settings.cache_ttl_secs);
        let cache = Arc::new(VersionCache::new(cache_ttl));

        // Background sweep task: evict expired entries periodically, but only
        // while the cache is non-empty.  When the cache is empty the task
        // blocks on a Notify with no active timers, so it does not prevent the
        // OS from sleeping.
        {
            let cache = Arc::clone(&cache);
            tokio::spawn(async move {
                loop {
                    // No timers here — block until something is inserted.
                    cache.wait_until_populated().await;
                    // Purge on a 60-second cadence for as long as the cache
                    // holds entries.  Once it empties, break back to the
                    // outer loop and go dormant again.
                    loop {
                        tokio::time::sleep(Duration::from_secs(60)).await;
                        cache.purge_expired().await;
                        if cache.is_empty().await {
                            break;
                        }
                    }
                }
            });
        }

        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(config),
            cache,
            providers: Arc::new(RwLock::new(build_providers(&settings))),
            log_reload_handle,
        }
    }

    /// Resolve dependencies for a document: parse, fetch versions, classify updates.
    async fn resolve_dependencies(&self, uri: &Url, content: &str) -> Vec<ResolvedDependency> {
        let uri_str = uri.as_str();
        let provider = match self.providers.read().await.get_provider(uri_str) {
            Some(p) => p,
            None => return Vec::new(),
        };

        let parsed_deps = provider.parse_dependencies(uri_str, content);
        let provider_name = provider.name();

        let mut resolved = Vec::with_capacity(parsed_deps.len());

        for dep in parsed_deps {
            let cache_key = format!("{}:{}", provider_name, dep.name);
            let dep_name = dep.name.clone();
            let version_result = self
                .cache
                .resolve(&cache_key, || provider.fetch_version(&dep_name))
                .await;

            let status =
                classify_dependency(&dep, &version_result, |c| provider.normalize_constraint(c));
            let prerelease = version_result.prerelease.clone();

            resolved.push(ResolvedDependency {
                parsed: dep,
                status,
                prerelease,
            });
        }

        resolved
    }
}

/// Classify a dependency based on its parsed constraint and all known stable versions.
/// The `normalize` closure translates the ecosystem's constraint syntax to semver
/// (provided by the provider via [`Provider::normalize_constraint`]).
fn classify_dependency(
    dep: &ParsedDependency,
    result: &VersionResult,
    normalize: impl Fn(&str) -> String,
) -> DependencyStatus {
    if result.stable_versions.is_empty() {
        return DependencyStatus::NotFound;
    }

    match version_utils::find_update_candidates(
        &dep.version_constraint,
        &result.stable_versions,
        normalize,
    ) {
        Some(candidates) => match candidates.in_range {
            Some(in_range_version) => {
                if candidates.patch.is_none()
                    && candidates.minor.is_none()
                    && candidates.major.is_none()
                {
                    DependencyStatus::UpToDate {
                        version: in_range_version,
                    }
                } else {
                    DependencyStatus::UpdateAvailable {
                        patch: candidates.patch,
                        minor: candidates.minor,
                        major: candidates.major,
                    }
                }
            }
            None => {
                // latest = highest available stable version regardless of constraint direction
                let latest = version_utils::find_latest(&result.stable_versions)
                    .unwrap_or_else(|| result.stable_versions[0].clone());
                DependencyStatus::VersionNotFound {
                    latest,
                    patch: candidates.patch,
                    minor: candidates.minor,
                    major: candidates.major,
                }
            }
        },
        None => DependencyStatus::Unsupported,
    }
}

/// Build the inlay hint label text based on the dependency status.
fn hint_label(status: &DependencyStatus) -> String {
    match status {
        DependencyStatus::Loading => "… fetching".to_string(),
        DependencyStatus::UpToDate { .. } => "✔ latest".to_string(),
        DependencyStatus::UpdateAvailable {
            major,
            minor,
            patch,
        } => {
            let (version, kind_str) = if let Some(v) = major.as_deref() {
                (v, "major")
            } else if let Some(v) = minor.as_deref() {
                (v, "minor")
            } else if let Some(v) = patch.as_deref() {
                (v, "patch")
            } else {
                unreachable!("UpdateAvailable with no candidates");
            };
            format!("↑ {} ({})", version, kind_str)
        }
        DependencyStatus::VersionNotFound {
            major,
            minor,
            patch,
            latest,
        } => {
            if let Some(v) = major.as_deref() {
                format!("✘ not found, ↑ {} (major)", v)
            } else if let Some(v) = minor.as_deref() {
                format!("✘ not found, ↑ {} (minor)", v)
            } else if let Some(v) = patch.as_deref() {
                format!("✘ not found, ↑ {} (patch)", v)
            } else {
                format!("✘ not found, ↑ {} (latest)", latest)
            }
        }
        DependencyStatus::NotFound => "✘ not found".to_string(),
        DependencyStatus::Unsupported => "⊘ unsupported".to_string(),
    }
}

/// Build the tooltip text for a dependency.
fn hint_tooltip(dep: &ResolvedDependency) -> String {
    match &dep.status {
        DependencyStatus::UpToDate { version } => {
            format!("✔ Up to date ({})", version)
        }
        DependencyStatus::UpdateAvailable {
            patch,
            minor,
            major,
        } => {
            let mut lines = vec![format!("Current: {}", dep.parsed.version_constraint)];
            if let Some(v) = major {
                lines.push(format!("Major: ↑ {}", v));
            }
            if let Some(v) = minor {
                lines.push(format!("Minor: ↑ {}", v));
            }
            if let Some(v) = patch {
                lines.push(format!("Patch: ↑ {}", v));
            }
            if let Some(pre) = &dep.prerelease {
                if version_utils::is_prerelease_constraint(&dep.parsed.version_constraint) {
                    lines.push(format!("Prerelease: {}", pre));
                }
            }
            lines.join("\n")
        }
        DependencyStatus::VersionNotFound {
            patch,
            minor,
            major,
            latest,
        } => {
            let has_candidates = major.is_some() || minor.is_some() || patch.is_some();
            let mut lines = vec![format!(
                "Version '{}' not found in registry",
                dep.parsed.version_constraint
            )];
            if let Some(v) = major {
                lines.push(format!("Major: ↑ {}", v));
            }
            if let Some(v) = minor {
                lines.push(format!("Minor: ↑ {}", v));
            }
            if let Some(v) = patch {
                lines.push(format!("Patch: ↑ {}", v));
            }
            if !has_candidates {
                lines.push(format!("Latest: {}", latest));
            }
            lines.join("\n")
        }
        DependencyStatus::NotFound => {
            format!("Package '{}' not found in registry", dep.parsed.name)
        }
        DependencyStatus::Unsupported => {
            format!(
                "Version constraint '{}' is not supported",
                dep.parsed.version_constraint
            )
        }
        DependencyStatus::Loading => "Fetching version information…".to_string(),
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                inlay_hint_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                // No executeCommand entries exposed; code actions apply edits directly.
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "update-versions-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        info!("update-versions-lsp ready");
        self.client
            .log_message(MessageType::INFO, "update-versions-lsp ready")
            .await;
        // After a server restart, prompt the client to re-request inlay hints
        // for all already-open documents.
        self.client
            .send_request::<request::InlayHintRefreshRequest>(())
            .await
            .ok();
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    // --- Document lifecycle ---

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let content = params.text_document.text;
        debug!(uri = %uri, "document opened");
        self.documents
            .write()
            .await
            .insert(uri.clone(), content.clone());

        // Proactively prefetch versions for this document so that when Zed
        // calls inlayHint (including after a server restart), results are ready.
        let uri_str = uri.as_str().to_string();
        if let Some(provider) = self.providers.read().await.get_provider(&uri_str) {
            let parsed = provider.parse_dependencies(&uri_str, &content);
            let provider_name = provider.name().to_string();
            let cache = Arc::clone(&self.cache);
            let client = self.client.clone();
            let mut needs_fetch: Vec<String> = Vec::new();
            for dep in &parsed {
                let key = format!("{}:{}", provider_name, dep.name);
                if cache.get(&key).await.is_none() {
                    needs_fetch.push(dep.name.clone());
                }
            }
            if !needs_fetch.is_empty() {
                tokio::spawn(async move {
                    for name in needs_fetch {
                        let key = format!("{}:{}", provider_name, name);
                        cache.resolve(&key, || provider.fetch_version(&name)).await;
                    }
                    client
                        .send_request::<request::InlayHintRefreshRequest>(())
                        .await
                        .ok();
                });
            }
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        if let Some(change) = params.content_changes.into_iter().last() {
            self.documents
                .write()
                .await
                .insert(params.text_document.uri, change.text);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);
    }

    // --- Configuration ---

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        if let Some(settings_val) = params.settings.get("update-versions") {
            if let Ok(settings) = serde_json::from_value::<Settings>(settings_val.clone()) {
                // Apply log level to the live tracing subscriber.
                if let Ok(new_filter) = settings.log_level.parse::<tracing_subscriber::EnvFilter>()
                {
                    let _ = self.log_reload_handle.reload(new_filter);
                }
                // Rebuild providers so registry URL and dependency keys take effect immediately.
                *self.providers.write().await = build_providers(&settings);
                // Update cache TTL without invalidating existing entries.
                self.cache.update_ttl(settings.cache_ttl_secs);
                self.config.update_settings(settings).await;
                // Refresh hints after config change
                self.client
                    .send_request::<request::InlayHintRefreshRequest>(())
                    .await
                    .ok();
            }
        }
    }

    // --- Inlay Hints ---

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri.clone();
        let content = {
            let docs = self.documents.read().await;
            match docs.get(&uri) {
                Some(c) => c.clone(),
                None => return Ok(Some(Vec::new())),
            }
        };

        let uri_str = uri.as_str();
        let provider = match self.providers.read().await.get_provider(uri_str) {
            Some(p) => p,
            None => return Ok(Some(Vec::new())),
        };

        let parsed_deps = provider.parse_dependencies(uri_str, &content);
        if parsed_deps.is_empty() {
            return Ok(Some(Vec::new()));
        }

        let provider_name = provider.name().to_string();
        let lines: Vec<&str> = content.lines().collect();

        // Build hints immediately from cache; queue anything not yet cached
        let mut hints = Vec::with_capacity(parsed_deps.len());
        let mut needs_fetch: Vec<String> = Vec::new();

        for dep in &parsed_deps {
            let cache_key = format!("{}:{}", provider_name, dep.name);
            let resolved = match self.cache.get(&cache_key).await {
                Some(version_result) => {
                    let status = classify_dependency(dep, &version_result, |c| {
                        provider.normalize_constraint(c)
                    });
                    let prerelease = version_result.prerelease.clone();
                    ResolvedDependency {
                        parsed: dep.clone(),
                        status,
                        prerelease,
                    }
                }
                None => {
                    needs_fetch.push(dep.name.clone());
                    ResolvedDependency {
                        parsed: dep.clone(),
                        status: DependencyStatus::Loading,
                        prerelease: None,
                    }
                }
            };

            let line = resolved.parsed.version_range.start.line as usize;
            let line_len = lines.get(line).map(|l| l.len()).unwrap_or(0);
            let label_text = hint_label(&resolved.status);
            let tooltip_text = hint_tooltip(&resolved);

            hints.push(InlayHint {
                position: Position {
                    line: line as u32,
                    character: line_len as u32,
                },
                label: InlayHintLabel::String(label_text),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: Some(InlayHintTooltip::String(tooltip_text)),
                padding_left: Some(true),
                padding_right: None,
                data: None,
            });
        }

        // Fetch uncached versions in the background, then ask Zed to re-request hints.
        if !needs_fetch.is_empty() {
            let client = self.client.clone();
            let cache = Arc::clone(&self.cache);
            tokio::spawn(async move {
                for name in needs_fetch {
                    let cache_key = format!("{}:{}", provider_name, name);
                    cache
                        .resolve(&cache_key, || provider.fetch_version(&name))
                        .await;
                }
                client
                    .send_request::<request::InlayHintRefreshRequest>(())
                    .await
                    .ok();
            });
        }

        Ok(Some(hints))
    }

    // --- Code Actions ---

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let cursor_line = params.range.start.line;

        let content = {
            let docs = self.documents.read().await;
            match docs.get(uri) {
                Some(c) => c.clone(),
                None => return Ok(None),
            }
        };

        let resolved = self.resolve_dependencies(uri, &content).await;
        let hide_prereleases = self.config.hide_prereleases();

        // Find dependencies on the cursor line
        let actions: Vec<CodeActionOrCommand> = resolved
            .iter()
            .filter(|dep| dep.parsed.version_range.start.line == cursor_line)
            .filter_map(|dep| {
                // Extract candidates and an optional "latest" fallback (VersionNotFound only).
                let (patch, minor, major, latest_fallback) = match &dep.status {
                    DependencyStatus::UpdateAvailable {
                        patch,
                        minor,
                        major,
                    } => (patch, minor, major, None),
                    DependencyStatus::VersionNotFound {
                        patch,
                        minor,
                        major,
                        latest,
                    } => (patch, minor, major, Some(latest.as_str())),
                    _ => return None,
                };

                let mut actions = Vec::new();

                for (candidate, kind_str, is_preferred) in [
                    (patch, "patch", true),
                    (minor, "minor", false),
                    (major, "major", false),
                ] {
                    if let Some(version) = candidate {
                        let new_text = version_utils::build_replacement_text(
                            &dep.parsed.version_constraint,
                            version,
                        );
                        let edit = WorkspaceEdit {
                            changes: Some(HashMap::from([(
                                uri.clone(),
                                vec![TextEdit {
                                    range: dep.parsed.version_range,
                                    new_text,
                                }],
                            )])),
                            ..Default::default()
                        };
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: format!(
                                "Update {} to {} ({})",
                                dep.parsed.name, version, kind_str
                            ),
                            kind: Some(CodeActionKind::QUICKFIX),
                            edit: Some(edit),
                            is_preferred: Some(is_preferred),
                            ..Default::default()
                        }));
                    }
                }

                // For VersionNotFound with no higher candidates, offer a "set to latest" action.
                if let Some(latest) = latest_fallback {
                    if patch.is_none() && minor.is_none() && major.is_none() {
                        let new_text = version_utils::build_replacement_text(
                            &dep.parsed.version_constraint,
                            latest,
                        );
                        let edit = WorkspaceEdit {
                            changes: Some(HashMap::from([(
                                uri.clone(),
                                vec![TextEdit {
                                    range: dep.parsed.version_range,
                                    new_text,
                                }],
                            )])),
                            ..Default::default()
                        };
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: format!("Update {} to {} (latest)", dep.parsed.name, latest),
                            kind: Some(CodeActionKind::QUICKFIX),
                            edit: Some(edit),
                            is_preferred: Some(true),
                            ..Default::default()
                        }));
                    }
                }

                // Add prerelease action if available, newer than current, and not disabled
                if let Some(pre) = &dep.prerelease {
                    let current_is_prerelease =
                        version_utils::is_prerelease_constraint(&dep.parsed.version_constraint);
                    let pre_is_newer = version_utils::prerelease_newer_than_constraint(
                        &dep.parsed.version_constraint,
                        pre,
                    );
                    if pre_is_newer && (!hide_prereleases || current_is_prerelease) {
                        let pre_text = version_utils::build_replacement_text(
                            &dep.parsed.version_constraint,
                            pre,
                        );
                        let pre_edit = WorkspaceEdit {
                            changes: Some(HashMap::from([(
                                uri.clone(),
                                vec![TextEdit {
                                    range: dep.parsed.version_range,
                                    new_text: pre_text,
                                }],
                            )])),
                            ..Default::default()
                        };
                        actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                            title: format!("Update {} to {} (prerelease)", dep.parsed.name, pre),
                            kind: Some(CodeActionKind::QUICKFIX),
                            edit: Some(pre_edit),
                            is_preferred: Some(false),
                            ..Default::default()
                        }));
                    }
                }

                if actions.is_empty() {
                    None
                } else {
                    Some(actions)
                }
            })
            .flatten()
            .collect();

        Ok(Some(actions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hint_label_up_to_date() {
        let status = DependencyStatus::UpToDate {
            version: "1.0.0".to_string(),
        };
        assert_eq!(hint_label(&status), "✔ latest");
    }

    #[test]
    fn test_hint_label_major_update() {
        let status = DependencyStatus::UpdateAvailable {
            major: Some("2.0.0".to_string()),
            minor: None,
            patch: None,
        };
        assert_eq!(hint_label(&status), "↑ 2.0.0 (major)");
    }

    #[test]
    fn test_hint_label_minor_update() {
        let status = DependencyStatus::UpdateAvailable {
            major: None,
            minor: Some("1.1.0".to_string()),
            patch: None,
        };
        assert_eq!(hint_label(&status), "↑ 1.1.0 (minor)");
    }

    #[test]
    fn test_hint_label_patch_update() {
        let status = DependencyStatus::UpdateAvailable {
            major: None,
            minor: None,
            patch: Some("1.0.1".to_string()),
        };
        assert_eq!(hint_label(&status), "↑ 1.0.1 (patch)");
    }

    #[test]
    fn test_hint_label_version_not_found_with_candidate() {
        let status = DependencyStatus::VersionNotFound {
            latest: "2.0.0".to_string(),
            patch: None,
            minor: None,
            major: Some("2.0.0".to_string()),
        };
        assert_eq!(hint_label(&status), "✘ not found, ↑ 2.0.0 (major)");
    }

    #[test]
    fn test_hint_label_version_not_found_no_candidate() {
        // No higher candidate — falls back to showing latest.
        let status = DependencyStatus::VersionNotFound {
            latest: "1.0.0".to_string(),
            patch: None,
            minor: None,
            major: None,
        };
        assert_eq!(hint_label(&status), "✘ not found, ↑ 1.0.0 (latest)");
    }

    #[test]
    fn test_hint_label_not_found() {
        assert_eq!(hint_label(&DependencyStatus::NotFound), "✘ not found");
    }

    #[test]
    fn test_hint_label_unsupported() {
        assert_eq!(hint_label(&DependencyStatus::Unsupported), "⊘ unsupported");
    }

    #[test]
    fn test_hint_label_loading() {
        assert_eq!(hint_label(&DependencyStatus::Loading), "… fetching");
    }

    #[test]
    fn test_classify_dependency_up_to_date() {
        // Only the base version itself is available — nothing newer, so it's up to date.
        let dep = ParsedDependency {
            name: "react".to_string(),
            version_constraint: "^18.2.0".to_string(),
            version_range: Range::default(),
        };
        let result = VersionResult {
            stable_versions: vec!["18.2.0".to_string()],
            prerelease: None,
        };
        match classify_dependency(&dep, &result, version_utils::normalize::standard) {
            DependencyStatus::UpToDate { .. } => {}
            other => panic!("Expected UpToDate, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_dependency_major_update() {
        // ^1.0.0 is satisfied by 1.0.0 (in_range = Some), so 2.0.0 shows as UpdateAvailable.
        let dep = ParsedDependency {
            name: "react".to_string(),
            version_constraint: "^1.0.0".to_string(),
            version_range: Range::default(),
        };
        let result = VersionResult {
            stable_versions: vec!["1.0.0".to_string(), "2.0.0".to_string()],
            prerelease: None,
        };
        match classify_dependency(&dep, &result, version_utils::normalize::standard) {
            DependencyStatus::UpdateAvailable {
                major: Some(_),
                minor: None,
                patch: None,
            } => {}
            other => panic!("Expected Major update, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_dependency_not_found() {
        let dep = ParsedDependency {
            name: "nonexistent".to_string(),
            version_constraint: "^1.0.0".to_string(),
            version_range: Range::default(),
        };
        let result = VersionResult {
            stable_versions: vec![],
            prerelease: None,
        };
        match classify_dependency(&dep, &result, version_utils::normalize::standard) {
            DependencyStatus::NotFound => {}
            other => panic!("Expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_dependency_patch_update() {
        // ~1.0.0 (>=1.0.0, <1.1.0) is satisfied by 1.0.5, so it shows as UpdateAvailable.
        let dep = ParsedDependency {
            name: "serde".to_string(),
            version_constraint: "~1.0.0".to_string(),
            version_range: Range::default(),
        };
        let result = VersionResult {
            stable_versions: vec!["1.0.0".to_string(), "1.0.5".to_string()],
            prerelease: None,
        };
        match classify_dependency(&dep, &result, version_utils::normalize::standard) {
            DependencyStatus::UpdateAvailable {
                patch: Some(_),
                minor: None,
                major: None,
            } => {}
            other => panic!("Expected patch update, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_dependency_version_not_found_with_higher() {
        // =1.0.0 pins an exact version that doesn't exist; a higher version is available.
        let dep = ParsedDependency {
            name: "serde".to_string(),
            version_constraint: "=1.0.0".to_string(),
            version_range: Range::default(),
        };
        let result = VersionResult {
            stable_versions: vec!["1.0.5".to_string()],
            prerelease: None,
        };
        match classify_dependency(&dep, &result, version_utils::normalize::standard) {
            DependencyStatus::VersionNotFound {
                latest,
                patch: Some(_),
                minor: None,
                major: None,
            } => assert_eq!(latest, "1.0.5"),
            other => panic!(
                "Expected VersionNotFound with patch candidate, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_classify_dependency_version_not_found_no_candidates() {
        // Pinned above all available versions — latest is still offered as fallback.
        let dep = ParsedDependency {
            name: "pkg".to_string(),
            version_constraint: "=99.0.0".to_string(),
            version_range: Range::default(),
        };
        let result = VersionResult {
            stable_versions: vec!["1.0.0".to_string()],
            prerelease: None,
        };
        match classify_dependency(&dep, &result, version_utils::normalize::standard) {
            DependencyStatus::VersionNotFound {
                latest,
                patch: None,
                minor: None,
                major: None,
            } => assert_eq!(latest, "1.0.0"),
            other => panic!(
                "Expected VersionNotFound with no candidates, got {:?}",
                other
            ),
        }
    }

    #[test]
    fn test_classify_dependency_minor_update() {
        let dep = ParsedDependency {
            name: "tokio".to_string(),
            version_constraint: "~1.2.0".to_string(),
            version_range: Range::default(),
        };
        let result = VersionResult {
            stable_versions: vec!["1.3.0".to_string(), "1.2.5".to_string()],
            prerelease: None,
        };
        match classify_dependency(&dep, &result, version_utils::normalize::standard) {
            DependencyStatus::UpdateAvailable {
                minor: Some(_),
                major: None,
                ..
            } => {}
            other => panic!("Expected minor update, got {:?}", other),
        }
    }

    #[test]
    fn test_classify_dependency_unsupported() {
        let dep = ParsedDependency {
            name: "pkg".to_string(),
            version_constraint: "*".to_string(),
            version_range: Range::default(),
        };
        let result = VersionResult {
            stable_versions: vec!["1.0.0".to_string()],
            prerelease: None,
        };
        match classify_dependency(&dep, &result, version_utils::normalize::standard) {
            DependencyStatus::Unsupported => {}
            other => panic!("Expected Unsupported, got {:?}", other),
        }
    }
}
