use async_trait::async_trait;
use serde::Deserialize;
use std::collections::HashMap;
use tower_lsp::lsp_types::{Position, Range};
use tracing::warn;

use crate::cache::VersionResult;
use crate::providers::{ParsedDependency, Provider};
use super::cargo::{extract_toml_version, find_toml_version_range};

/// PyPI provider — resolves versions for `requirements.txt` and `pyproject.toml`.
pub struct PypiProvider {
    http: reqwest::Client,
}

impl PypiProvider {
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

/// Minimal PyPI JSON API response.
#[derive(Deserialize)]
struct PypiResponse {
    releases: HashMap<String, serde_json::Value>,
}

#[async_trait]
impl Provider for PypiProvider {
    fn file_patterns(&self) -> &[&str] {
        &["requirements.txt", "pyproject.toml"]
    }

    fn name(&self) -> &str {
        "pypi"
    }

    fn parse_dependencies(&self, uri: &str, content: &str) -> Vec<ParsedDependency> {
        if uri.ends_with("requirements.txt") {
            parse_requirements_txt(content)
        } else if uri.ends_with("pyproject.toml") {
            parse_pyproject_toml(content)
        } else {
            Vec::new()
        }
    }

    async fn fetch_version(&self, package_name: &str) -> VersionResult {
        let url = format!("https://pypi.org/pypi/{}/json", package_name);

        let response = self.http.get(&url).send().await;

        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(
                    package = package_name,
                    status = %r.status(),
                    "PyPI returned non-success status"
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
                    "Failed to fetch from PyPI"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let pypi_data: PypiResponse = match response.json().await {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    package = package_name,
                    error = %e,
                    "Failed to parse PyPI response"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let mut stable_vs: Vec<semver::Version> = pypi_data
            .releases
            .keys()
            .filter(|v| !is_pep440_prerelease(v))
            .filter_map(|v| parse_pypi_version_stable(v))
            .collect();
        stable_vs.sort_unstable_by(|a, b| b.cmp(a));
        let stable_versions: Vec<String> = stable_vs.iter().map(|v| v.to_string()).collect();

        let prerelease = pypi_data
            .releases
            .keys()
            .filter(|v| is_pep440_prerelease(v))
            .filter_map(|v| parse_pep440_prerelease_as_semver(v))
            .max()
            .map(|v| v.to_string());

        VersionResult {
            stable_versions,
            prerelease,
        }
    }
}

// ---------------------------------------------------------------------------
// Version helpers
// ---------------------------------------------------------------------------

/// Return true if the version string represents a PEP 440 pre-release.
/// Stable post-releases (`.postN`) are NOT pre-releases.
fn is_pep440_prerelease(version: &str) -> bool {
    let lower = version.to_ascii_lowercase();
    // .postN is a stable post-release — treat as stable.
    // .devN is always a pre-release.
    if lower.contains(".dev") {
        return true;
    }
    if lower.contains(".post") {
        return false;
    }
    // alpha/beta/rc indicated by alphabetic chars embedded in the version
    version.chars().any(|c| c.is_ascii_alphabetic())
}

/// Parse a stable PEP 440 version into a [`semver::Version`].
/// Handles `.postN` suffix (strips it) and two-part versions (`1.2` → `1.2.0`).
fn parse_pypi_version_stable(version: &str) -> Option<semver::Version> {
    // Strip post-release suffix: "1.2.3.post1" → "1.2.3"
    let base = if let Some(idx) = version.to_ascii_lowercase().find(".post") {
        &version[..idx]
    } else {
        version
    };

    // Direct parse (strict semver)
    if let Ok(v) = semver::Version::parse(base) {
        return if v.pre.is_empty() { Some(v) } else { None };
    }

    // Pad two-part versions: "1.2" → "1.2.0"
    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() == 2 && parts.iter().all(|p| p.parse::<u64>().is_ok()) {
        if let Ok(v) = semver::Version::parse(&format!("{}.0", base)) {
            return if v.pre.is_empty() { Some(v) } else { None };
        }
    }

    None
}

/// Attempt to convert a PEP 440 pre-release version string into a semver [`Version`].
///
/// Mappings:
/// - `1.0a1`  → `1.0.0-alpha.1`
/// - `1.0b2`  → `1.0.0-beta.2`
/// - `1.0rc1` → `1.0.0-rc.1`
fn parse_pep440_prerelease_as_semver(version: &str) -> Option<semver::Version> {
    let lower = version.to_ascii_lowercase();

    // Skip .dev releases (hard to rank meaningfully)
    if lower.contains(".dev") {
        return None;
    }

    // Find where alphabetic characters (the marker) start
    let marker_pos = version.find(|c: char| c.is_ascii_alphabetic())?;
    let numeric = &version[..marker_pos];
    let marker = &lower[marker_pos..];

    // Pad numeric part to 3 components
    let padded = pad_to_semver_numeric(numeric)?;

    // Map PEP 440 marker to semver pre-release identifier
    let semver_pre = if let Some(n) = marker.strip_prefix("rc") {
        format!("rc.{}", n)
    } else if let Some(n) = marker.strip_prefix("alpha") {
        format!("alpha.{}", n)
    } else if let Some(n) = marker.strip_prefix('a') {
        format!("alpha.{}", n)
    } else if let Some(n) = marker.strip_prefix("beta") {
        format!("beta.{}", n)
    } else if let Some(n) = marker.strip_prefix('b') {
        format!("beta.{}", n)
    } else {
        return None;
    };

    semver::Version::parse(&format!("{}-{}", padded, semver_pre)).ok()
}

/// Pad a dotted numeric string to exactly three components (`x.y.z`).
fn pad_to_semver_numeric(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.split('.').collect();
    match parts.len() {
        1 => {
            parts[0].parse::<u64>().ok()?;
            Some(format!("{}.0.0", parts[0]))
        }
        2 => {
            parts[0].parse::<u64>().ok()?;
            parts[1].parse::<u64>().ok()?;
            Some(format!("{}.0", s))
        }
        _ => Some(s.to_string()),
    }
}

// ---------------------------------------------------------------------------
// requirements.txt parser
// ---------------------------------------------------------------------------

/// Parse a `requirements.txt` file and extract versioned dependencies.
fn parse_requirements_txt(content: &str) -> Vec<ParsedDependency> {
    let mut deps = Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Skip blank lines, comments, and pip options/flags
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with('-')
            || trimmed.starts_with('.')
        {
            continue;
        }

        // Strip inline comment — must be preceded by whitespace per PEP 508
        let effective = strip_inline_comment(trimmed);

        if let Some((pkg_name, constraint, constraint_offset)) = parse_req_line(effective) {
            // constraint_offset is measured from `effective` which starts at `trimmed`.
            // Find where `trimmed` starts in the original line.
            let trim_offset = line.len() - line.trim_start().len();
            let abs_start = trim_offset + constraint_offset;
            let abs_end = abs_start + constraint.len();

            deps.push(ParsedDependency {
                name: pkg_name,
                version_constraint: constraint,
                version_range: Range {
                    start: Position {
                        line: line_idx as u32,
                        character: abs_start as u32,
                    },
                    end: Position {
                        line: line_idx as u32,
                        character: abs_end as u32,
                    },
                },
            });
        }
    }

    deps
}

/// Strip an inline PEP 508 comment (`  # ...`) from a requirement line.
fn strip_inline_comment(line: &str) -> &str {
    // A comment starts with ` #` (space then hash)
    if let Some(pos) = line.find(" #") {
        line[..pos].trim_end()
    } else {
        line
    }
}

/// Parse a single requirement string (e.g. `requests>=2.28.0` or `Django[extra]~=4.2.0`).
///
/// Returns `(normalised_package_name, version_constraint, constraint_byte_offset)` where
/// `constraint_byte_offset` is the byte offset of the constraint within `line`.
fn parse_req_line(line: &str) -> Option<(String, String, usize)> {
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    // Consume package name: [A-Za-z0-9._-]+
    while i < len
        && (bytes[i].is_ascii_alphanumeric()
            || bytes[i] == b'-'
            || bytes[i] == b'_'
            || bytes[i] == b'.')
    {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    let raw_name = &line[..i];

    // Skip optional extras: [extra1,extra2]
    if i < len && bytes[i] == b'[' {
        i += 1;
        while i < len && bytes[i] != b']' {
            i += 1;
        }
        if i < len {
            i += 1; // consume ']'
        }
    }

    // Skip optional whitespace before specifier
    while i < len && bytes[i] == b' ' {
        i += 1;
    }

    let rest = &line[i..];
    if !starts_with_version_operator(rest) {
        return None;
    }

    let constraint = rest.trim_end().to_string();
    let constraint_offset = i;

    Some((normalize_pypi_name(raw_name), constraint, constraint_offset))
}

/// Normalise a PyPI package name: lowercase and replace `-`/`.` with `_`.
/// The PyPI API accepts both forms, but normalisation avoids duplicate cache keys.
pub(crate) fn normalize_pypi_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .replace(['-', '.'], "_")
}

/// Return true if the string starts with a PEP 440 version specifier operator.
fn starts_with_version_operator(s: &str) -> bool {
    s.starts_with("==")
        || s.starts_with("!=")
        || s.starts_with(">=")
        || s.starts_with("<=")
        || s.starts_with("~=")
        || s.starts_with('>')
        || s.starts_with('<')
}

// ---------------------------------------------------------------------------
// pyproject.toml parser
// ---------------------------------------------------------------------------

/// Parse a `pyproject.toml` file and extract versioned dependencies.
///
/// Supports:
/// - **PEP 621** (`[project].dependencies` array of PEP 508 strings)
/// - **Poetry** (`[tool.poetry.dependencies]` TOML table, same syntax as `Cargo.toml`)
fn parse_pyproject_toml(content: &str) -> Vec<ParsedDependency> {
    let mut deps = Vec::new();

    let parsed: toml::Value = match content.parse() {
        Ok(v) => v,
        Err(_) => return deps,
    };

    let lines: Vec<&str> = content.lines().collect();

    // --- PEP 621 ---
    if let Some(project_deps) = parsed
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for dep_val in project_deps {
            if let Some(dep_str) = dep_val.as_str() {
                if let Some((name, constraint, offset)) = parse_req_line(dep_str) {
                    if let Some(range) =
                        find_pep621_dep_range(&lines, dep_str, &constraint, offset)
                    {
                        deps.push(ParsedDependency {
                            name,
                            version_constraint: constraint,
                            version_range: range,
                        });
                    }
                }
            }
        }
    }

    // --- Poetry ---
    if let Some(poetry_table) = parsed
        .get("tool")
        .and_then(|t| t.get("poetry"))
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_table())
    {
        for (name, value) in poetry_table {
            // Skip the Python version constraint — it's not a PyPI package
            if name == "python" {
                continue;
            }

            if let Some(version_str) = extract_toml_version(value) {
                if let Some(range) = find_toml_version_range(
                    &lines,
                    "tool.poetry.dependencies",
                    name,
                    &version_str,
                ) {
                    deps.push(ParsedDependency {
                        name: normalize_pypi_name(name),
                        version_constraint: version_str,
                        version_range: range,
                    });
                }
            }
        }
    }

    deps
}

/// Find the LSP range of a version constraint inside a quoted PEP 621 dependency string.
///
/// Given a line like `    "requests>=2.28.0",`, this locates the `>=2.28.0` part.
fn find_pep621_dep_range(
    lines: &[&str],
    dep_str: &str,
    constraint: &str,
    constraint_offset: usize,
) -> Option<Range> {
    let quoted = format!("\"{}\"", dep_str);

    for (line_idx, line) in lines.iter().enumerate() {
        if let Some(quote_start) = line.find(&quoted) {
            // +1 to skip the opening double-quote
            let abs_start = quote_start + 1 + constraint_offset;
            let abs_end = abs_start + constraint.len();
            return Some(Range {
                start: Position {
                    line: line_idx as u32,
                    character: abs_start as u32,
                },
                end: Position {
                    line: line_idx as u32,
                    character: abs_end as u32,
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

    // --- is_pep440_prerelease ---

    #[test]
    fn test_stable_version() {
        assert!(!is_pep440_prerelease("1.2.3"));
    }

    #[test]
    fn test_post_release_is_stable() {
        assert!(!is_pep440_prerelease("1.2.3.post1"));
    }

    #[test]
    fn test_alpha_is_prerelease() {
        assert!(is_pep440_prerelease("1.0a1"));
    }

    #[test]
    fn test_beta_is_prerelease() {
        assert!(is_pep440_prerelease("2.0b3"));
    }

    #[test]
    fn test_rc_is_prerelease() {
        assert!(is_pep440_prerelease("3.0rc1"));
    }

    #[test]
    fn test_dev_is_prerelease() {
        assert!(is_pep440_prerelease("1.0.dev1"));
    }

    // --- parse_pypi_version_stable ---

    #[test]
    fn test_parse_stable_three_part() {
        let v = parse_pypi_version_stable("2.31.0").unwrap();
        assert_eq!(v.to_string(), "2.31.0");
    }

    #[test]
    fn test_parse_stable_two_part() {
        let v = parse_pypi_version_stable("5.0").unwrap();
        assert_eq!(v.to_string(), "5.0.0");
    }

    #[test]
    fn test_parse_post_release_stripped() {
        let v = parse_pypi_version_stable("1.2.3.post2").unwrap();
        assert_eq!(v.to_string(), "1.2.3");
    }

    #[test]
    fn test_parse_prerelease_returns_none() {
        assert!(parse_pypi_version_stable("1.0a1").is_none());
    }

    // --- parse_req_line ---

    #[test]
    fn test_req_line_eq() {
        let (name, constraint, offset) = parse_req_line("requests==2.28.0").unwrap();
        assert_eq!(name, "requests");
        assert_eq!(constraint, "==2.28.0");
        assert_eq!(offset, 8);
    }

    #[test]
    fn test_req_line_gte() {
        let (name, constraint, offset) = parse_req_line("flask>=2.0.0").unwrap();
        assert_eq!(name, "flask");
        assert_eq!(constraint, ">=2.0.0");
        assert_eq!(offset, 5);
    }

    #[test]
    fn test_req_line_compatible() {
        let (name, constraint, offset) = parse_req_line("django~=4.2.0").unwrap();
        assert_eq!(name, "django");
        assert_eq!(constraint, "~=4.2.0");
        assert_eq!(offset, 6);
    }

    #[test]
    fn test_req_line_with_extras() {
        let (name, constraint, offset) = parse_req_line("requests[security]>=2.28.0").unwrap();
        assert_eq!(name, "requests");
        assert_eq!(constraint, ">=2.28.0");
        assert_eq!(offset, 18);
    }

    #[test]
    fn test_req_line_no_version_returns_none() {
        assert!(parse_req_line("requests").is_none());
    }

    #[test]
    fn test_req_line_normalises_name() {
        let (name, _, _) = parse_req_line("My-Package>=1.0.0").unwrap();
        assert_eq!(name, "my_package");
    }

    // --- parse_requirements_txt ---

    #[test]
    fn test_parse_requirements_txt_basic() {
        let content = "requests==2.28.0\nflask>=2.0.0\n# comment\n\n-r base.txt\n";
        let deps = parse_requirements_txt(content);
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "requests");
        assert_eq!(deps[0].version_constraint, "==2.28.0");
        assert_eq!(deps[1].name, "flask");
        assert_eq!(deps[1].version_constraint, ">=2.0.0");
    }

    #[test]
    fn test_parse_requirements_txt_ranges() {
        let content = "requests==2.28.0\n";
        let deps = parse_requirements_txt(content);
        assert_eq!(deps[0].version_range.start.character, 8); // after "requests"
        assert_eq!(deps[0].version_range.end.character, 16);  // end of "==2.28.0"
    }

    // --- parse_pyproject_toml (PEP 621) ---

    #[test]
    fn test_parse_pyproject_pep621() {
        let content = r#"
[project]
name = "myapp"
dependencies = [
    "requests>=2.28.0",
    "flask>=2.0.0",
]
"#;
        let deps = parse_pyproject_toml(content);
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].name, "requests");
        assert_eq!(deps[0].version_constraint, ">=2.28.0");
        assert_eq!(deps[1].name, "flask");
        assert_eq!(deps[1].version_constraint, ">=2.0.0");
    }

    // --- parse_pyproject_toml (Poetry) ---

    #[test]
    fn test_parse_pyproject_poetry() {
        let content = r#"
[tool.poetry.dependencies]
python = "^3.9"
requests = "^2.28.0"
flask = "^2.0.0"
"#;
        let mut deps = parse_pyproject_toml(content);
        assert_eq!(deps.len(), 2); // python is skipped
        // Sort by name so the assertion is order-independent (HashMap iteration is non-deterministic)
        deps.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(deps[0].name, "flask");
        assert_eq!(deps[0].version_constraint, "^2.0.0");
        assert_eq!(deps[1].name, "requests");
        assert_eq!(deps[1].version_constraint, "^2.28.0");
    }

    // --- normalize_pypi_name ---

    #[test]
    fn test_normalize_name_dashes() {
        assert_eq!(normalize_pypi_name("My-Package"), "my_package");
    }

    #[test]
    fn test_normalize_name_dots() {
        assert_eq!(normalize_pypi_name("zope.interface"), "zope_interface");
    }
}
