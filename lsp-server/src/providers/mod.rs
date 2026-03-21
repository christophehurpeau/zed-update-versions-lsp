pub mod cargo;
pub mod composer;
pub mod deno;
pub mod npm;
pub mod pypi;
pub mod rubygems;

use std::sync::Arc;

use async_trait::async_trait;
use tower_lsp::lsp_types::Range;

use crate::cache::VersionResult;

/// A single dependency parsed from a manifest file.
#[derive(Debug, Clone)]
pub struct ParsedDependency {
    /// Package name (e.g. "react", "serde").
    pub name: String,
    /// The raw version constraint string (e.g. "^18.2.0", "1.0").
    pub version_constraint: String,
    /// LSP range covering the entire version string (including operator) in the document.
    pub version_range: Range,
}

/// Status of a resolved dependency.
#[derive(Debug, Clone)]
pub enum DependencyStatus {
    /// Currently fetching from registry.
    Loading,
    /// The declared range already satisfies the latest version.
    UpToDate { version: String },
    /// A newer version is available.
    UpdateAvailable {
        patch: Option<String>,
        minor: Option<String>,
        major: Option<String>,
    },
    /// The version constraint matched no available version in the registry.
    /// `latest` is the highest stable version available; patch/minor/major are
    /// candidates strictly higher than the constraint's base version (if any).
    VersionNotFound {
        /// The overall highest stable version in the registry.
        latest: String,
        patch: Option<String>,
        minor: Option<String>,
        major: Option<String>,
    },
    /// Package was not found in the registry.
    NotFound,
    /// The version constraint syntax is not supported (e.g. git URLs).
    Unsupported,
}

/// A fully resolved dependency (parsed + version lookup result).
#[derive(Debug, Clone)]
pub struct ResolvedDependency {
    pub parsed: ParsedDependency,
    pub status: DependencyStatus,
    /// Prerelease version, if available and different from stable.
    pub prerelease: Option<String>,
}

/// Trait that each package ecosystem provider must implement.
#[async_trait]
pub trait Provider: Send + Sync {
    /// File name patterns this provider handles (matched against the document URI path).
    /// Examples: `["package.json"]`, `["Cargo.toml"]`
    fn file_patterns(&self) -> &[&str];

    /// Parse a document and extract dependencies.
    fn parse_dependencies(&self, uri: &str, content: &str) -> Vec<ParsedDependency>;

    /// Fetch the latest version(s) of a package from the registry.
    async fn fetch_version(&self, package_name: &str) -> VersionResult;

    /// Return the provider name (used as cache key prefix).
    fn name(&self) -> &str;

    /// Translate an ecosystem-specific version constraint to a string that
    /// [`semver::VersionReq`] can parse.
    ///
    /// The default implementation handles bare versions and standard semver
    /// operators (`^`, `~`, `>=`, …) — correct for npm, Cargo, Composer, Pub.
    ///
    /// Providers whose ecosystems use different syntax **must** override this:
    /// - RubyGems / Dub → [`crate::version_utils::normalize::ruby`]
    /// - PyPI            → [`crate::version_utils::normalize::python`]
    /// - Deno (deno.land/x) → [`crate::version_utils::normalize::deno`]
    fn normalize_constraint(&self, constraint: &str) -> String {
        crate::version_utils::normalize::standard(constraint)
    }
}

/// Registry that selects the right provider for a given document URI.
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: Vec::new(),
        }
    }

    pub fn register(&mut self, provider: Arc<dyn Provider>) {
        self.providers.push(provider);
    }

    /// Find the provider that handles the given document URI.
    pub fn get_provider(&self, uri: &str) -> Option<Arc<dyn Provider>> {
        self.providers.iter().find_map(|p| {
            if p.file_patterns().iter().any(|pat| uri.ends_with(pat)) {
                Some(Arc::clone(p))
            } else {
                None
            }
        })
    }
}
