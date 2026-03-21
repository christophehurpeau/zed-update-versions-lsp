use async_trait::async_trait;
use serde::Deserialize;
use tower_lsp::lsp_types::{Position, Range};
use tracing::warn;

use crate::cache::VersionResult;
use crate::providers::{ParsedDependency, Provider};

/// Composer provider — resolves versions from packagist.org for `composer.json`.
pub struct ComposerProvider {
    http: reqwest::Client,
}

impl ComposerProvider {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "update-versions-lsp/{} (zed-extension)",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("failed to build HTTP client");

        Self { http }
    }
}

/// A single version entry from the Packagist p2 API.
#[derive(Deserialize)]
struct PackagistVersionEntry {
    version: String,
}

/// Minimal Packagist p2 API response.
/// The `packages` map contains an array of version entries per package name.
#[derive(Deserialize)]
struct PackagistResponse {
    packages: std::collections::HashMap<String, Vec<PackagistVersionEntry>>,
}

#[async_trait]
impl Provider for ComposerProvider {
    fn file_patterns(&self) -> &[&str] {
        &["composer.json"]
    }

    fn name(&self) -> &str {
        "composer"
    }

    fn parse_dependencies(&self, _uri: &str, content: &str) -> Vec<ParsedDependency> {
        parse_composer_json(content)
    }

    async fn fetch_version(&self, package_name: &str) -> VersionResult {
        let url = format!("https://repo.packagist.org/p2/{}.json", package_name);

        let response = self.http.get(&url).send().await;

        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(
                    package = package_name,
                    status = %r.status(),
                    "Packagist returned non-success status"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
            Err(e) => {
                warn!(
                    package = package_name,
                    error = %e,
                    "Failed to fetch from Packagist"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let data: PackagistResponse = match response.json().await {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    package = package_name,
                    error = %e,
                    "Failed to parse Packagist response"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        // The p2 endpoint key is the package name (lowercase).
        let entries = match data.packages.get(package_name) {
            Some(e) => e,
            None => {
                // Try first available key (case-insensitive fallback)
                match data.packages.values().next() {
                    Some(e) => e,
                    None => {
                        return VersionResult {
                            stable_versions: Vec::new(),
                            prerelease: None,
                        }
                    }
                }
            }
        };

        let mut stable_vs: Vec<semver::Version> = entries
            .iter()
            .map(|e| &e.version)
            .filter(|v| !is_composer_prerelease(v))
            .filter_map(|v| parse_composer_version(v))
            .collect();
        stable_vs.sort_unstable_by(|a, b| b.cmp(a));
        let stable_versions: Vec<String> = stable_vs.iter().map(format_composer_version).collect();

        let prerelease = entries
            .iter()
            .map(|e| &e.version)
            .filter(|v| is_composer_prerelease(v))
            .filter_map(|v| parse_composer_version(v))
            .max()
            .map(|v| format_composer_version(&v));

        VersionResult {
            stable_versions,
            prerelease,
        }
    }
}

// ---------------------------------------------------------------------------
// Version helpers
// ---------------------------------------------------------------------------

/// Return true if the Composer version string is a pre-release.
/// Composer uses suffixes like `-alpha`, `-beta`, `-RC`, `-dev`, `.x-dev`.
fn is_composer_prerelease(version: &str) -> bool {
    let lower = version.to_ascii_lowercase();
    lower.contains("alpha")
        || lower.contains("beta")
        || lower.contains("-rc")
        || lower.contains(".rc")
        || lower.contains("dev")
        || lower.contains("patch")
}

/// Strip Composer's `v` prefix and normalise to a semver-compatible string.
fn parse_composer_version(version: &str) -> Option<semver::Version> {
    // Strip leading `v` or `V`
    let s = version
        .strip_prefix('v')
        .or_else(|| version.strip_prefix('V'))
        .unwrap_or(version);

    // Direct parse (strict semver like "1.2.3")
    if let Ok(v) = semver::Version::parse(s) {
        if v.pre.is_empty() {
            return Some(v);
        }
        return None;
    }

    let parts: Vec<&str> = s.split('.').collect();

    // Strip 4th component: Composer allows "1.2.3.0" (four parts).
    if parts.len() == 4 && parts.iter().all(|p| p.parse::<u64>().is_ok()) {
        let three = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
        if let Ok(v) = semver::Version::parse(&three) {
            if v.pre.is_empty() {
                return Some(v);
            }
        }
    }

    // Pad two-part versions e.g. "1.2" → "1.2.0"
    if parts.len() == 2 && parts.iter().all(|p| p.parse::<u64>().is_ok()) {
        if let Ok(v) = semver::Version::parse(&format!("{}.0", s)) {
            if v.pre.is_empty() {
                return Some(v);
            }
        }
    }

    None
}

/// Format a semver version back to a canonical `X.Y.Z` string.
fn format_composer_version(v: &semver::Version) -> String {
    format!("{}.{}.{}", v.major, v.minor, v.patch)
}

// ---------------------------------------------------------------------------
// composer.json parser
// ---------------------------------------------------------------------------

const COMPOSER_DEP_KEYS: &[&str] = &["require", "require-dev"];

/// Parse a `composer.json` and extract versioned dependencies.
fn parse_composer_json(content: &str) -> Vec<ParsedDependency> {
    let mut deps = Vec::new();

    let parsed: serde_json::Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return deps,
    };

    let lines: Vec<&str> = content.lines().collect();

    for key in COMPOSER_DEP_KEYS {
        let obj = match parsed.get(*key).and_then(|v| v.as_object()) {
            Some(o) => o,
            None => continue,
        };

        for (name, value) in obj {
            // Skip the `php` and `ext-*` platform requirements
            if name == "php" || name.starts_with("ext-") || name.starts_with("lib-") {
                continue;
            }

            let version_str = match value.as_str() {
                Some(v) => v,
                None => continue,
            };

            // Skip unsupported specifiers (dev-master, dev-*, VCS URLs, etc.)
            if is_unsupported_composer_constraint(version_str) {
                continue;
            }

            if let Some(range) = find_version_range(&lines, name, version_str) {
                deps.push(ParsedDependency {
                    name: name.clone(),
                    version_constraint: version_str.to_string(),
                    version_range: range,
                });
            }
        }
    }

    deps
}

/// Return `true` for composer constraints we can't resolve to a registry version.
fn is_unsupported_composer_constraint(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("dev-") || lower == "self.version" || value.starts_with('@') || value == "*"
}

/// Find the LSP range of a version string in a JSON document.
/// Looks for a line containing `"name": "version_str"`.
fn find_version_range(lines: &[&str], name: &str, version_str: &str) -> Option<Range> {
    let name_pattern = format!("\"{}\"", name);
    let version_pattern = format!("\"{}\"", version_str);

    for (line_idx, line) in lines.iter().enumerate() {
        if !line.contains(&name_pattern) {
            continue;
        }
        if let Some(val_start) = line.find(&version_pattern) {
            let content_start = val_start + 1; // skip opening quote
            let content_end = content_start + version_str.len();
            return Some(Range {
                start: Position {
                    line: line_idx as u32,
                    character: content_start as u32,
                },
                end: Position {
                    line: line_idx as u32,
                    character: content_end as u32,
                },
            });
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_composer_json_basic() {
        let content = r#"{
    "require": {
        "php": "^8.1",
        "symfony/console": "^6.0",
        "guzzlehttp/guzzle": "^7.5"
    },
    "require-dev": {
        "phpunit/phpunit": "^10.0"
    }
}"#;
        let deps = parse_composer_json(content);
        assert_eq!(deps.len(), 3);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"symfony/console"));
        assert!(names.contains(&"guzzlehttp/guzzle"));
        assert!(names.contains(&"phpunit/phpunit"));
        // php platform requirement should be skipped
        assert!(!names.contains(&"php"));
    }

    #[test]
    fn test_parse_composer_json_skips_platform_and_dev() {
        let content = r#"{
    "require": {
        "php": "^8.0",
        "ext-json": "*",
        "lib-curl": "*",
        "vendor/pkg": "dev-main",
        "vendor/valid": "^1.0"
    }
}"#;
        let deps = parse_composer_json(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "vendor/valid");
    }

    #[test]
    fn test_parse_composer_json_version_range() {
        let content = r#"{
    "require": {
        "vendor/package": "^2.3.4"
    }
}"#;
        let deps = parse_composer_json(content);
        assert_eq!(deps.len(), 1);
        let dep = &deps[0];
        assert_eq!(dep.version_constraint, "^2.3.4");
        // The version string starts after the opening quote
        assert_eq!(dep.version_range.start.line, 2);
        let line = r#"        "vendor/package": "^2.3.4""#;
        let expected_start = line.find("^2.3.4").unwrap() as u32;
        assert_eq!(dep.version_range.start.character, expected_start);
        assert_eq!(
            dep.version_range.end.character,
            expected_start + "^2.3.4".len() as u32
        );
    }

    #[test]
    fn test_is_composer_prerelease() {
        assert!(is_composer_prerelease("1.0.0-alpha.1"));
        assert!(is_composer_prerelease("1.0.0-beta1"));
        assert!(is_composer_prerelease("2.0.0-RC1"));
        assert!(is_composer_prerelease("3.0.0-dev"));
        assert!(!is_composer_prerelease("1.0.0"));
        assert!(!is_composer_prerelease("2.3.4"));
    }

    #[test]
    fn test_parse_composer_version_normal() {
        assert!(parse_composer_version("1.0.0").is_some());
        assert!(parse_composer_version("v2.3.4").is_some());
        assert!(parse_composer_version("1.2").is_some());
    }

    #[test]
    fn test_parse_composer_version_prerelease_returns_none() {
        assert!(parse_composer_version("1.0.0-alpha1").is_none());
        assert!(parse_composer_version("2.0.0-beta").is_none());
    }

    #[test]
    fn test_is_unsupported_composer_constraint() {
        assert!(is_unsupported_composer_constraint("dev-main"));
        assert!(is_unsupported_composer_constraint("dev-master"));
        assert!(is_unsupported_composer_constraint("*"));
        assert!(is_unsupported_composer_constraint("self.version"));
        assert!(!is_unsupported_composer_constraint("^1.0"));
        assert!(!is_unsupported_composer_constraint("~2.3.4"));
    }

    #[test]
    fn test_parse_composer_version_four_part() {
        // Composer allows four-part versions like "1.2.3.0"; the 4th component is dropped.
        let v = parse_composer_version("1.2.3.0").expect("should parse 4-part version");
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 2);
        assert_eq!(v.patch, 3);
    }

    #[test]
    fn test_format_composer_version() {
        let v = semver::Version::new(1, 2, 3);
        assert_eq!(format_composer_version(&v), "1.2.3");
    }
}
