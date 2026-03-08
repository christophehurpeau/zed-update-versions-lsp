use async_trait::async_trait;
use serde::Deserialize;
use tower_lsp::lsp_types::{Position, Range};
use tracing::warn;

use crate::cache::VersionResult;
use crate::providers::{ParsedDependency, Provider};

/// Cargo/crates.io provider — resolves versions from crates.io.
pub struct CargoProvider {
    http: reqwest::Client,
    dependency_keys: Vec<String>,
}

impl CargoProvider {
    pub fn new(dependency_keys: Vec<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "update-versions-lsp/{} (zed-extension)",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("failed to build HTTP client");

        Self {
            http,
            dependency_keys,
        }
    }
}

/// Response from the crates.io API.
#[derive(Deserialize)]
struct CratesIoResponse {
    versions: Vec<CrateVersion>,
}

#[derive(Deserialize)]
struct CrateVersion {
    num: String,
    yanked: bool,
}

#[async_trait]
impl Provider for CargoProvider {
    fn file_patterns(&self) -> &[&str] {
        &["Cargo.toml"]
    }

    fn name(&self) -> &str {
        "cargo"
    }

    fn parse_dependencies(&self, _uri: &str, content: &str) -> Vec<ParsedDependency> {
        parse_cargo_toml(content, &self.dependency_keys)
    }

    async fn fetch_version(&self, package_name: &str) -> VersionResult {
        let url = format!("https://crates.io/api/v1/crates/{}", package_name);

        let response = self.http.get(&url).send().await;

        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(
                    package = package_name,
                    status = %r.status(),
                    "crates.io returned non-success status"
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
                    "Failed to fetch from crates.io"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let crate_data: CratesIoResponse = match response.json().await {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    package = package_name,
                    error = %e,
                    "Failed to parse crates.io response"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let mut stable_vs: Vec<semver::Version> = crate_data
            .versions
            .iter()
            .filter(|v| !v.yanked)
            .filter_map(|v| semver::Version::parse(&v.num).ok())
            .filter(|v| v.pre.is_empty())
            .collect();
        stable_vs.sort_unstable_by(|a, b| b.cmp(a));
        let stable_versions: Vec<String> = stable_vs.iter().map(|v| v.to_string()).collect();

        let prerelease = crate_data
            .versions
            .iter()
            .filter(|v| !v.yanked)
            .filter_map(|v| semver::Version::parse(&v.num).ok())
            .filter(|v| !v.pre.is_empty())
            .max()
            .map(|v| v.to_string());

        VersionResult {
            stable_versions,
            prerelease,
        }
    }
}

/// Parse Cargo.toml and extract dependencies.
fn parse_cargo_toml(content: &str, dependency_keys: &[String]) -> Vec<ParsedDependency> {
    let mut deps = Vec::new();

    let parsed: toml::Value = match content.parse() {
        Ok(v) => v,
        Err(_) => return deps,
    };

    let lines: Vec<&str> = content.lines().collect();

    for dep_key in dependency_keys {
        let table = resolve_toml_key(&parsed, dep_key);
        let table = match table.and_then(|v| v.as_table()) {
            Some(t) => t,
            None => continue,
        };

        for (name, value) in table {
            let version_constraint = match extract_toml_version(value) {
                Some(v) => v,
                None => continue, // path/git dependency → skip
            };

            if let Some(range) = find_toml_version_range(&lines, dep_key, name, &version_constraint)
            {
                deps.push(ParsedDependency {
                    name: name.clone(),
                    version_constraint,
                    version_range: range,
                });
            }
        }
    }

    deps
}

/// Resolve a possibly nested+dotted TOML key.
/// Handles both `dependencies` and `workspace.dependencies` forms.
fn resolve_toml_key<'a>(value: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    let mut current = value;
    for part in key.split('.') {
        current = current.get(part)?;
    }
    Some(current)
}

/// Extract version string from a TOML dependency value.
/// Values can be:
///  - Plain string: `"1.0"`
///  - Table with `version` key: `{ version = "1.0", features = [...] }`
///  - Table with only `path`/`git` → returns None
pub(crate) fn extract_toml_version(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Table(t) => t.get("version")?.as_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// Find the line and character range of a version string in the TOML source.
pub(crate) fn find_toml_version_range(
    lines: &[&str],
    dep_key: &str,
    name: &str,
    version_str: &str,
) -> Option<Range> {
    // We need to find the section and then the dependency within it.
    // Strategy: find a line starting with `[dep_key]` or `[dep_key.` header,
    // then find `name = ...` within that section.

    let section_header = build_section_headers(dep_key);
    let mut in_section = false;

    for (line_idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // Check if we're entering the right section
        if trimmed.starts_with('[') {
            in_section = trimmed == section_header;
            continue;
        }

        if !in_section {
            continue;
        }

        // Look for `name = "version"` or `name = { version = "..." }`
        if !trimmed.starts_with(name) {
            continue;
        }

        // Make sure it's an exact key match (not a prefix)
        let after_name = trimmed.get(name.len()..)?;
        if !after_name.starts_with([' ', '=']) {
            continue;
        }

        // Find the version string in this line
        let version_quoted = format!("\"{}\"", version_str);
        if let Some(pos) = line.find(&version_quoted) {
            let content_start = pos + 1; // skip opening quote
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

/// Build the TOML section header for a dep_key.
/// e.g. "dependencies" → `"[dependencies]"`
/// e.g. "workspace.dependencies" → `"[workspace.dependencies]"`
fn build_section_headers(dep_key: &str) -> String {
    format!("[{}]", dep_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_cargo_version_string() {
        let val = toml::Value::String("1.0.0".to_string());
        assert_eq!(extract_toml_version(&val), Some("1.0.0".to_string()));
    }

    #[test]
    fn test_extract_cargo_version_table() {
        let toml_str = r#"
[dep]
version = "1.0.0"
features = ["derive"]
"#;
        let parsed: toml::Value = toml_str.parse().unwrap();
        let val = parsed.get("dep").unwrap();
        assert_eq!(extract_toml_version(val), Some("1.0.0".to_string()));
    }

    #[test]
    fn test_extract_cargo_version_path_only() {
        let toml_str = r#"
[dep]
path = "../my-crate"
"#;
        let parsed: toml::Value = toml_str.parse().unwrap();
        let val = parsed.get("dep").unwrap();
        assert_eq!(extract_toml_version(val), None);
    }

    #[test]
    fn test_extract_cargo_version_git_only() {
        let toml_str = r#"
[dep]
git = "https://github.com/user/repo"
"#;
        let parsed: toml::Value = toml_str.parse().unwrap();
        let val = parsed.get("dep").unwrap();
        assert_eq!(extract_toml_version(val), None);
    }

    #[test]
    fn test_parse_cargo_toml_basic() {
        let content = r#"[package]
name = "my-project"
version = "0.1.0"

[dependencies]
serde = "1.0"
tokio = { version = "1.0", features = ["full"] }

[dev-dependencies]
pretty_assertions = "1.3.0"
"#;

        let keys = vec!["dependencies".to_string(), "dev-dependencies".to_string()];
        let deps = parse_cargo_toml(content, &keys);

        assert_eq!(deps.len(), 3);

        let serde_dep = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde_dep.version_constraint, "1.0");

        let tokio_dep = deps.iter().find(|d| d.name == "tokio").unwrap();
        assert_eq!(tokio_dep.version_constraint, "1.0");

        let pa_dep = deps.iter().find(|d| d.name == "pretty_assertions").unwrap();
        assert_eq!(pa_dep.version_constraint, "1.3.0");
    }

    #[test]
    fn test_parse_cargo_toml_skips_path_deps() {
        let content = r#"[dependencies]
serde = "1.0"
local-crate = { path = "../local" }
git-crate = { git = "https://github.com/user/repo" }
"#;

        let keys = vec!["dependencies".to_string()];
        let deps = parse_cargo_toml(content, &keys);

        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "serde");
    }

    #[test]
    fn test_parse_cargo_toml_workspace_deps() {
        let content = r#"[workspace.dependencies]
serde = "1.0"
tokio = { version = "1.35", features = ["full"] }
"#;

        let keys = vec!["workspace.dependencies".to_string()];
        let deps = parse_cargo_toml(content, &keys);

        assert_eq!(deps.len(), 2);
    }

    #[test]
    fn test_parse_cargo_toml_version_range_location() {
        let content = r#"[dependencies]
serde = "1.0"
"#;

        let keys = vec!["dependencies".to_string()];
        let deps = parse_cargo_toml(content, &keys);

        assert_eq!(deps.len(), 1);
        let dep = &deps[0];
        assert_eq!(dep.version_range.start.line, 1); // line 1 (0-indexed)
        assert!(dep.version_range.start.character < dep.version_range.end.character);
    }

    #[test]
    fn test_parse_cargo_toml_empty() {
        let content = r#"[package]
name = "my-project"
"#;
        let keys = vec!["dependencies".to_string()];
        let deps = parse_cargo_toml(content, &keys);
        assert!(deps.is_empty());
    }

    #[test]
    fn test_parse_cargo_toml_invalid() {
        let content = "not valid toml {{{";
        let keys = vec!["dependencies".to_string()];
        let deps = parse_cargo_toml(content, &keys);
        assert!(deps.is_empty());
    }
}
