use async_trait::async_trait;
use serde::Deserialize;
use tower_lsp::lsp_types::{Position, Range};
use tracing::warn;

use crate::cache::VersionResult;
use crate::providers::{ParsedDependency, Provider};

/// Deno provider — resolves versions for `deno.json` and `import_map.json`.
///
/// Supports three specifier types found in the `imports` map:
/// - `jsr:@scope/pkg@constraint`  → fetched from jsr.io
/// - `npm:pkg@constraint`         → fetched from registry.npmjs.org
/// - `https://deno.land/x/pkg@vX.Y.Z/...` → fetched from cdn.deno.land
pub struct DenoProvider {
    http: reqwest::Client,
    npm_registry: String,
}

impl DenoProvider {
    pub fn new(npm_registry: String) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!(
                "update-versions-lsp/{} (zed-extension)",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .expect("failed to build HTTP client");

        Self { http, npm_registry }
    }
}

// ---------------------------------------------------------------------------
// Packaged registry response shapes
// ---------------------------------------------------------------------------

/// jsr.io meta.json — `https://jsr.io/@scope/pkg/meta.json`
#[derive(Deserialize)]
struct JsrMeta {
    versions: std::collections::HashMap<String, serde_json::Value>,
}

/// cdn.deno.land versions — `https://cdn.deno.land/{pkg}/meta/versions.json`
#[derive(Deserialize)]
struct DenoLandVersions {
    versions: Vec<String>,
}

/// Abbreviated npm packument (reused from npm provider logic).
#[derive(Deserialize)]
struct NpmPackument {
    #[serde(default)]
    versions: std::collections::HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Provider impl
// ---------------------------------------------------------------------------

#[async_trait]
impl Provider for DenoProvider {
    fn file_patterns(&self) -> &[&str] {
        &["deno.json", "import_map.json", "deno.jsonc"]
    }

    fn name(&self) -> &str {
        "deno"
    }

    /// Only deno.land/x specifiers use a bare `v`-prefix (e.g. `v12.6.1`).
    /// JSR and `npm:` specifiers already use standard semver operators and
    /// are handled correctly by the fallthrough in [`version_utils::normalize::deno`].
    fn normalize_constraint(&self, constraint: &str) -> String {
        crate::version_utils::normalize::deno(constraint)
    }

    fn parse_dependencies(&self, _uri: &str, content: &str) -> Vec<ParsedDependency> {
        parse_deno_config(content)
    }

    async fn fetch_version(&self, package_name: &str) -> VersionResult {
        if let Some(jsr_name) = package_name.strip_prefix("jsr:") {
            self.fetch_jsr(jsr_name).await
        } else if let Some(npm_name) = package_name.strip_prefix("npm:") {
            self.fetch_npm(npm_name).await
        } else if let Some(deno_name) = package_name.strip_prefix("deno:") {
            self.fetch_deno_land(deno_name).await
        } else {
            VersionResult {
                stable_versions: Vec::new(),
                prerelease: None,
            }
        }
    }
}

impl DenoProvider {
    async fn fetch_jsr(&self, jsr_name: &str) -> VersionResult {
        // jsr_name is e.g. "@std/path"
        let url = format!("https://jsr.io/{}/meta.json", jsr_name);
        let response = self.http.get(&url).send().await;

        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(package = jsr_name, status = %r.status(), "jsr.io returned non-success status");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
            Err(e) => {
                warn!(package = jsr_name, error = %e, "Failed to fetch from jsr.io");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let meta: JsrMeta = match response.json().await {
            Ok(m) => m,
            Err(e) => {
                warn!(package = jsr_name, error = %e, "Failed to parse jsr.io meta.json");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let mut stable_vs: Vec<semver::Version> = meta
            .versions
            .keys()
            .filter_map(|v| semver::Version::parse(v).ok())
            .filter(|v| v.pre.is_empty())
            .collect();
        stable_vs.sort_unstable_by(|a, b| b.cmp(a));
        let stable_versions: Vec<String> = stable_vs.iter().map(|v| v.to_string()).collect();

        let prerelease = meta
            .versions
            .keys()
            .filter_map(|v| semver::Version::parse(v).ok())
            .filter(|v| !v.pre.is_empty())
            .max()
            .map(|v| v.to_string());

        VersionResult {
            stable_versions,
            prerelease,
        }
    }

    async fn fetch_npm(&self, npm_name: &str) -> VersionResult {
        let encoded = if npm_name.starts_with('@') {
            npm_name.replacen('/', "%2F", 1)
        } else {
            npm_name.to_string()
        };

        let url = format!("{}/{}", self.npm_registry, encoded);
        let response = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.npm.install-v1+json")
            .send()
            .await;

        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(package = npm_name, status = %r.status(), "npm registry returned non-success status");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
            Err(e) => {
                warn!(package = npm_name, error = %e, "Failed to fetch from npm registry");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let packument: NpmPackument = match response.json().await {
            Ok(p) => p,
            Err(e) => {
                warn!(package = npm_name, error = %e, "Failed to parse npm packument");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let mut stable_vs: Vec<semver::Version> = packument
            .versions
            .keys()
            .filter_map(|v| semver::Version::parse(v).ok())
            .filter(|v| v.pre.is_empty())
            .collect();
        stable_vs.sort_unstable_by(|a, b| b.cmp(a));
        let stable_versions: Vec<String> = stable_vs.iter().map(|v| v.to_string()).collect();

        let prerelease = packument
            .versions
            .keys()
            .filter_map(|v| semver::Version::parse(v).ok())
            .filter(|v| !v.pre.is_empty())
            .max()
            .map(|v| v.to_string());

        VersionResult {
            stable_versions,
            prerelease,
        }
    }

    async fn fetch_deno_land(&self, pkg_name: &str) -> VersionResult {
        let url = format!("https://cdn.deno.land/{}/meta/versions.json", pkg_name);
        let response = self.http.get(&url).send().await;

        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(package = pkg_name, status = %r.status(), "deno.land returned non-success status");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
            Err(e) => {
                warn!(package = pkg_name, error = %e, "Failed to fetch from deno.land");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let versions_data: DenoLandVersions = match response.json().await {
            Ok(d) => d,
            Err(e) => {
                warn!(package = pkg_name, error = %e, "Failed to parse deno.land versions.json");
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        // deno.land versions have a `v` prefix (e.g. "v12.6.1"); strip it for semver parsing.
        let mut stable_vs: Vec<semver::Version> = versions_data
            .versions
            .iter()
            .filter_map(|v| {
                let s = v.strip_prefix('v').unwrap_or(v.as_str());
                semver::Version::parse(s).ok()
            })
            .filter(|v| v.pre.is_empty())
            .collect();
        stable_vs.sort_unstable_by(|a, b| b.cmp(a));
        // Store without `v` prefix so semver_utils can compare them directly.
        // build_replacement_text("v12.6.1", "13.0.0") → "v13.0.0", preserving the prefix.
        let stable_versions: Vec<String> = stable_vs.iter().map(|v| v.to_string()).collect();

        let prerelease = versions_data
            .versions
            .iter()
            .filter_map(|v| {
                let s = v.strip_prefix('v').unwrap_or(v.as_str());
                semver::Version::parse(s).ok()
            })
            .filter(|v| !v.pre.is_empty())
            .max()
            .map(|v| v.to_string());

        VersionResult {
            stable_versions,
            prerelease,
        }
    }
}

// ---------------------------------------------------------------------------
// Specifier parsers
// ---------------------------------------------------------------------------

/// Parse a `jsr:@scope/pkg@constraint` specifier.
/// Returns `(provider_key, constraint)` e.g. `("jsr:@std/path", "^0.221.0")`.
pub(crate) fn parse_jsr_specifier(specifier: &str) -> Option<(String, String)> {
    let rest = specifier.strip_prefix("jsr:")?;
    // rest is like `@std/path@^0.221.0` or `@std/path` (no version)
    // The package name is `@scope/name` (starts with `@`, contains exactly one `/`)
    if !rest.starts_with('@') {
        return None;
    }
    let slash = rest.find('/')?;
    // After the slash, find the second `@` that separates name from version
    let after_slash = &rest[slash + 1..];
    let at_pos = after_slash.find('@')?;
    let pkg_end = slash + 1 + at_pos;
    let pkg = &rest[..pkg_end]; // e.g. "@std/path"
    let constraint = &rest[pkg_end + 1..]; // e.g. "^0.221.0" (or "/sub/path" — skip that)
    if constraint.is_empty() || !is_version_start(constraint) {
        return None;
    }
    // Strip a trailing path segment if present (e.g. `^0.221.0/mod.ts` → `^0.221.0`)
    let constraint = constraint.split('/').next().unwrap_or(constraint);
    Some((format!("jsr:{}", pkg), constraint.to_string()))
}

/// Parse a `npm:pkg@constraint` specifier.
/// Returns `("npm:pkg", "constraint")`.
pub(crate) fn parse_npm_specifier(specifier: &str) -> Option<(String, String)> {
    let rest = specifier.strip_prefix("npm:")?;
    // Handle scoped packages: `@scope/pkg@constraint`
    let at_pos = if rest.starts_with('@') {
        let slash = rest.find('/')?;
        rest[slash..].find('@').map(|i| slash + i)?
    } else {
        rest.find('@')?
    };
    let pkg = &rest[..at_pos];
    let constraint = &rest[at_pos + 1..];
    if pkg.is_empty() || constraint.is_empty() || !is_version_start(constraint) {
        return None;
    }
    Some((format!("npm:{}", pkg), constraint.to_string()))
}

/// Parse a `https://deno.land/x/pkg@vX.Y.Z/...` URL.
/// Returns `("deno:pkg", "vX.Y.Z")`.
pub(crate) fn parse_deno_land_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://deno.land/x/")?;
    // rest is `pkg@vX.Y.Z/mod.ts` or `pkg@vX.Y.Z`
    let at_pos = rest.find('@')?;
    let pkg = &rest[..at_pos];
    if pkg.is_empty() {
        return None;
    }
    let after_at = &rest[at_pos + 1..];
    // Strip trailing path
    let version = after_at.split('/').next().unwrap_or(after_at);
    if version.is_empty() {
        return None;
    }
    // Validate: strip `v` prefix and check it's a semver-like string
    let bare = version.strip_prefix('v').unwrap_or(version);
    if !bare.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((format!("deno:{}", pkg), version.to_string()))
}

/// Return `true` if `s` starts with a character that begins a version constraint.
fn is_version_start(s: &str) -> bool {
    s.chars()
        .next()
        .is_some_and(|c| matches!(c, '0'..='9' | '^' | '~' | '>' | '<' | '=' | 'v'))
}

// ---------------------------------------------------------------------------
// deno.json / import_map.json parser
// ---------------------------------------------------------------------------

/// Parse a `deno.json` or `import_map.json` and extract versioned dependencies.
pub fn parse_deno_config(content: &str) -> Vec<ParsedDependency> {
    // Strip JSONC comments for parsing (naive single-line comment strip)
    let json_content = strip_jsonc_comments(content);

    let parsed: serde_json::Value = match serde_json::from_str(&json_content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let lines: Vec<&str> = content.lines().collect();
    let mut deps = Vec::new();

    // Handle both `imports` at top level (deno.json) and the standard import map shape.
    if let Some(imports) = parsed.get("imports").and_then(|v| v.as_object()) {
        deps.extend(extract_deps_from_imports(imports, &lines));
    }

    // Also handle `scopes` in import maps (each scope has its own imports map).
    if let Some(scopes) = parsed.get("scopes").and_then(|v| v.as_object()) {
        for scope_imports in scopes.values() {
            if let Some(obj) = scope_imports.as_object() {
                deps.extend(extract_deps_from_imports(obj, &lines));
            }
        }
    }

    deps
}

/// Extract dependencies from an `imports` JSON object.
fn extract_deps_from_imports(
    imports: &serde_json::Map<String, serde_json::Value>,
    lines: &[&str],
) -> Vec<ParsedDependency> {
    let mut deps = Vec::new();

    for (_key, value) in imports {
        let specifier = match value.as_str() {
            Some(s) => s,
            None => continue,
        };

        // Skip path prefixes (end with `/`)
        if specifier.ends_with('/') {
            continue;
        }

        let parsed = parse_jsr_specifier(specifier)
            .or_else(|| parse_npm_specifier(specifier))
            .or_else(|| parse_deno_land_url(specifier));

        if let Some((name, constraint)) = parsed {
            if let Some(range) = find_constraint_range_in_specifier(lines, specifier, &constraint) {
                deps.push(ParsedDependency {
                    name,
                    version_constraint: constraint,
                    version_range: range,
                });
            }
        }
    }

    deps
}

/// Find the LSP range of `constraint` inside the JSON value string `specifier`.
///
/// We search for a line that contains a JSON string value equal to `specifier`,
/// then locate `constraint` within the specifier's position.
fn find_constraint_range_in_specifier(
    lines: &[&str],
    specifier: &str,
    constraint: &str,
) -> Option<Range> {
    // Build the exact JSON quoted string to search for
    let quoted_specifier = format!("\"{}\"", specifier);

    for (line_idx, line) in lines.iter().enumerate() {
        if let Some(spec_start) = line.find(&quoted_specifier) {
            // Find the constraint within the specifier string value.
            // Offset: start of specifier value content (skip opening quote)
            let value_content_start = spec_start + 1;
            // Find constraint relative to start of the value content
            let specifier_in_line =
                &line[value_content_start..value_content_start + specifier.len()];
            if let Some(constraint_offset) = specifier_in_line.find(constraint) {
                let char_start = (value_content_start + constraint_offset) as u32;
                let char_end = char_start + constraint.len() as u32;
                return Some(Range {
                    start: Position {
                        line: line_idx as u32,
                        character: char_start,
                    },
                    end: Position {
                        line: line_idx as u32,
                        character: char_end,
                    },
                });
            }
        }
    }
    None
}

/// Strip single-line comments (`// ...`) from JSONC content.
/// This is a minimal implementation: it handles `//` comments outside strings.
fn strip_jsonc_comments(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    for line in content.lines() {
        let stripped = strip_line_comment(line);
        result.push_str(stripped);
        result.push('\n');
    }
    result
}

/// Strip a `//` comment from a single line, respecting string literals.
fn strip_line_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escape_next = false;
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if escape_next {
            escape_next = false;
            i += 1;
            continue;
        }
        match chars[i] {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '/' if !in_string && i + 1 < chars.len() && chars[i + 1] == '/' => {
                // Find byte offset for this char index
                let byte_offset = line
                    .char_indices()
                    .nth(i)
                    .map(|(b, _)| b)
                    .unwrap_or(line.len());
                return line[..byte_offset].trim_end();
            }
            _ => {}
        }
        i += 1;
    }
    line
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_jsr_specifier() {
        assert_eq!(
            parse_jsr_specifier("jsr:@std/path@^0.221.0"),
            Some(("jsr:@std/path".to_string(), "^0.221.0".to_string()))
        );
        assert_eq!(
            parse_jsr_specifier("jsr:@std/assert@^0.221.0"),
            Some(("jsr:@std/assert".to_string(), "^0.221.0".to_string()))
        );
        // No version → None
        assert_eq!(parse_jsr_specifier("jsr:@std/path"), None);
        // Non-jsr → None
        assert_eq!(parse_jsr_specifier("npm:react@^18.0.0"), None);
    }

    #[test]
    fn test_parse_jsr_specifier_with_path() {
        // Version with trailing sub-path should strip the path
        let result = parse_jsr_specifier("jsr:@std/path@^0.221.0/posix");
        assert_eq!(
            result,
            Some(("jsr:@std/path".to_string(), "^0.221.0".to_string()))
        );
    }

    #[test]
    fn test_parse_npm_specifier() {
        assert_eq!(
            parse_npm_specifier("npm:express@^4.18.0"),
            Some(("npm:express".to_string(), "^4.18.0".to_string()))
        );
        assert_eq!(
            parse_npm_specifier("npm:@types/node@^20.0.0"),
            Some(("npm:@types/node".to_string(), "^20.0.0".to_string()))
        );
        assert_eq!(parse_npm_specifier("npm:express"), None);
        assert_eq!(parse_npm_specifier("jsr:@std/path@^1.0.0"), None);
    }

    #[test]
    fn test_parse_deno_land_url() {
        assert_eq!(
            parse_deno_land_url("https://deno.land/x/oak@v12.6.1/mod.ts"),
            Some(("deno:oak".to_string(), "v12.6.1".to_string()))
        );
        // Trailing `/` is on the specifier string in import maps; parse_deno_land_url
        // still extracts the version — the caller (extract_deps_from_imports) skips
        // specifiers that end with `/`.
        assert_eq!(
            parse_deno_land_url("https://deno.land/x/fresh@1.6.5/"),
            Some(("deno:fresh".to_string(), "1.6.5".to_string()))
        );
        assert_eq!(
            parse_deno_land_url("https://deno.land/x/std@0.170.0/path/mod.ts"),
            Some(("deno:std".to_string(), "0.170.0".to_string()))
        );
    }

    #[test]
    fn test_parse_deno_config_imports() {
        let content = r#"{
  "imports": {
    "@std/path": "jsr:@std/path@^0.221.0",
    "express": "npm:express@^4.18.0",
    "oak": "https://deno.land/x/oak@v12.6.1/mod.ts",
    "$fresh/": "https://deno.land/x/fresh@1.6.5/"
  }
}"#;
        let deps = parse_deno_config(content);
        assert_eq!(deps.len(), 3);

        let jsr = deps.iter().find(|d| d.name == "jsr:@std/path").unwrap();
        assert_eq!(jsr.version_constraint, "^0.221.0");

        let npm = deps.iter().find(|d| d.name == "npm:express").unwrap();
        assert_eq!(npm.version_constraint, "^4.18.0");

        let deno = deps.iter().find(|d| d.name == "deno:oak").unwrap();
        assert_eq!(deno.version_constraint, "v12.6.1");
    }

    #[test]
    fn test_parse_deno_config_version_range() {
        let content = r#"{
  "imports": {
    "@std/path": "jsr:@std/path@^0.221.0"
  }
}"#;
        let deps = parse_deno_config(content);
        assert_eq!(deps.len(), 1);
        let dep = &deps[0];
        let line = r#"    "@std/path": "jsr:@std/path@^0.221.0""#;
        let expected_start = line.find("^0.221.0").unwrap() as u32;
        assert_eq!(dep.version_range.start.line, 2);
        assert_eq!(dep.version_range.start.character, expected_start);
        assert_eq!(
            dep.version_range.end.character,
            expected_start + "^0.221.0".len() as u32
        );
    }

    #[test]
    fn test_strip_jsonc_comments() {
        let input = r#"{ // top-level comment
  "imports": {
    "@std/path": "jsr:@std/path@^0.221.0" // inline comment
  }
}"#;
        let stripped = strip_jsonc_comments(input);
        let parsed: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert!(parsed.get("imports").is_some());
    }

    #[test]
    fn test_parse_deno_config_jsonc() {
        let content = r#"{
  // Deno configuration
  "imports": {
    "@std/path": "jsr:@std/path@^0.221.0" // std library
  }
}"#;
        let deps = parse_deno_config(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "jsr:@std/path");
    }

    #[test]
    fn test_parse_deno_land_url_no_path() {
        // URL without a sub-path after the version
        assert_eq!(
            parse_deno_land_url("https://deno.land/x/oak@v12.6.1"),
            Some(("deno:oak".to_string(), "v12.6.1".to_string()))
        );
    }

    #[test]
    fn test_parse_deno_config_scopes() {
        // `scopes` in import maps should also be walked for versioned specifiers.
        let content = r#"{
  "scopes": {
    "https://example.com/": {
      "@std/path": "jsr:@std/path@^0.221.0"
    }
  }
}"#;
        let deps = parse_deno_config(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "jsr:@std/path");
        assert_eq!(deps[0].version_constraint, "^0.221.0");
    }
}
