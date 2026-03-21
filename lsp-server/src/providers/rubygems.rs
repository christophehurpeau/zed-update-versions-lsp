use async_trait::async_trait;
use serde::Deserialize;
use tower_lsp::lsp_types::{Position, Range};
use tracing::warn;

use crate::cache::VersionResult;
use crate::providers::{ParsedDependency, Provider};

/// RubyGems provider — resolves versions for `Gemfile` files.
pub struct RubyGemsProvider {
    http: reqwest::Client,
}

impl RubyGemsProvider {
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

/// A single version entry from GET /api/v1/versions/{name}.json.
#[derive(Deserialize)]
struct GemVersion {
    number: String,
    prerelease: bool,
}

#[async_trait]
impl Provider for RubyGemsProvider {
    fn file_patterns(&self) -> &[&str] {
        &["Gemfile"]
    }

    fn name(&self) -> &str {
        "rubygems"
    }

    fn normalize_constraint(&self, constraint: &str) -> String {
        crate::version_utils::normalize::ruby(constraint)
    }

    fn parse_dependencies(&self, _uri: &str, content: &str) -> Vec<ParsedDependency> {
        parse_gemfile(content)
    }

    async fn fetch_version(&self, package_name: &str) -> VersionResult {
        // /versions/{name}.json returns all published versions with a `prerelease` flag,
        // which allows us to correctly classify constraints like `~> 7.1`.
        let url = format!("https://rubygems.org/api/v1/versions/{}.json", package_name);

        let response = self.http.get(&url).send().await;

        let response = match response {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                warn!(
                    package = package_name,
                    status = %r.status(),
                    "RubyGems returned non-success status"
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
                    "Failed to fetch from RubyGems"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let versions: Vec<GemVersion> = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    package = package_name,
                    error = %e,
                    "Failed to parse RubyGems versions response"
                );
                return VersionResult {
                    stable_versions: Vec::new(),
                    prerelease: None,
                };
            }
        };

        let mut stable_vs: Vec<semver::Version> = versions
            .iter()
            .filter(|v| !v.prerelease)
            .filter_map(|v| pad_to_semver(&v.number))
            .filter_map(|s| semver::Version::parse(&s).ok())
            .collect();
        stable_vs.sort_unstable_by(|a, b| b.cmp(a));
        let stable_versions: Vec<String> = stable_vs.iter().map(|v| v.to_string()).collect();

        let prerelease = versions
            .iter()
            .filter(|v| v.prerelease)
            .filter_map(|v| pad_to_semver(&v.number))
            .filter_map(|s| semver::Version::parse(&s).ok())
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

/// Normalise a Ruby version string to a semver-compatible `X.Y.Z` string.
/// Returns `None` if the string is not parseable as a version number.
pub(crate) fn pad_to_semver(version: &str) -> Option<String> {
    // Direct parse
    if semver::Version::parse(version).is_ok() {
        return Some(version.to_string());
    }

    // Pad two-part version: "1.2" → "1.2.0"
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() == 2 && parts.iter().all(|p| p.parse::<u64>().is_ok()) {
        return Some(format!("{}.0", version));
    }

    // One-part version: "1" → "1.0.0"
    if parts.len() == 1 && parts[0].parse::<u64>().is_ok() {
        return Some(format!("{}.0.0", version));
    }

    None
}

// ---------------------------------------------------------------------------
// Gemfile parser
// ---------------------------------------------------------------------------

/// Parse a `Gemfile` and extract versioned `gem` directives.
///
/// Supported forms:
/// ```ruby
/// gem 'rails', '~> 7.0'
/// gem "devise", ">= 4.9.0"
/// gem 'pg', '~> 1.4', '>= 1.4.3'   # multiple constraints — take the first
/// ```
///
/// Unsupported / skipped:
/// - gems with no version constraint
/// - `path:`, `git:`, `github:` source options
/// - gems declared via `:git`, `:path`, `:github` keyword args
pub fn parse_gemfile(content: &str) -> Vec<ParsedDependency> {
    let mut deps = Vec::new();

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Skip blank lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(dep) = parse_gem_line(line, line_idx as u32) {
            deps.push(dep);
        }
    }

    deps
}

/// Parse a single `gem ...` line.
fn parse_gem_line(line: &str, line_idx: u32) -> Option<ParsedDependency> {
    let trimmed = line.trim();

    // Must start with `gem ` or `gem\t`
    let rest = trimmed.strip_prefix("gem")?;
    if !rest.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let rest = rest.trim_start();

    // Strip inline comment
    let effective = strip_inline_comment(rest);

    // Tokenise the argument list
    let tokens = tokenise_gem_args(effective);
    if tokens.is_empty() {
        return None;
    }

    let name = tokens[0].trim_matches(|c| c == '\'' || c == '"');
    if name.is_empty() {
        return None;
    }

    // Skip gems with source options (git/path/github)
    let has_source_option = tokens.iter().any(|t| {
        let t = t.trim();
        t.starts_with("git:")
            || t.starts_with("path:")
            || t.starts_with("github:")
            || t.starts_with(":git")
            || t.starts_with(":path")
            || t.starts_with(":github")
    });
    if has_source_option {
        return None;
    }

    // Find the first version constraint token (second positional string arg)
    let version_token = tokens.iter().skip(1).find(|t| {
        let t = t.trim();
        let inner = t.trim_matches(|c| c == '\'' || c == '"');
        is_version_constraint(inner)
    })?;

    let version_str = version_token.trim().trim_matches(|c| c == '\'' || c == '"');
    if version_str.is_empty() {
        return None;
    }

    // Locate the version string in the original line.
    // We look for the quoted version token to find the exact character offset.
    let quoted_single = format!("'{}'", version_str);
    let quoted_double = format!("\"{}\"", version_str);

    let (version_start_in_line, _quote_char) =
        find_first_occurrence(line, &quoted_single, &quoted_double)?;
    let content_start = version_start_in_line + 1; // skip opening quote
    let content_end = content_start + version_str.len();

    Some(ParsedDependency {
        name: name.to_string(),
        version_constraint: version_str.to_string(),
        version_range: Range {
            start: Position {
                line: line_idx,
                character: content_start as u32,
            },
            end: Position {
                line: line_idx,
                character: content_end as u32,
            },
        },
    })
}

/// Strip a Ruby inline comment (`# ...`) from a line.
fn strip_inline_comment(line: &str) -> &str {
    // Only strip `#` if it is not inside a string (simple heuristic: count open quotes)
    let mut in_single = false;
    let mut in_double = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '#' if !in_single && !in_double => return line[..i].trim_end(),
            _ => {}
        }
    }
    line
}

/// Split `gem` arguments by comma, respecting quoted strings.
fn tokenise_gem_args(s: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut in_single = false;
    let mut in_double = false;

    for (i, ch) in s.char_indices() {
        match ch {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ',' if !in_single && !in_double => {
                let tok = s[start..i].trim();
                if !tok.is_empty() {
                    tokens.push(tok);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    let tok = s[start..].trim();
    if !tok.is_empty() {
        tokens.push(tok);
    }
    tokens
}

/// Return `true` if `s` looks like a Ruby version constraint string.
fn is_version_constraint(s: &str) -> bool {
    let s = s.trim();
    // Must start with a digit or a comparison operator / tilde-rocket
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    matches!(first, '0'..='9' | '~' | '>' | '<' | '=' | '!')
}

/// Find the byte offset of the first occurrence of `a` or `b` in `haystack`.
/// Returns `(offset, matched_string)` for whichever appears first.
fn find_first_occurrence<'a>(haystack: &str, a: &'a str, b: &'a str) -> Option<(usize, &'a str)> {
    let pos_a = haystack.find(a);
    let pos_b = haystack.find(b);
    match (pos_a, pos_b) {
        (Some(pa), Some(pb)) => {
            if pa <= pb {
                Some((pa, a))
            } else {
                Some((pb, b))
            }
        }
        (Some(pa), None) => Some((pa, a)),
        (None, Some(pb)) => Some((pb, b)),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gemfile_basic() {
        let content = r#"
source 'https://rubygems.org'

gem 'rails', '~> 7.0'
gem 'pg', '>= 1.4.0'
gem 'devise', '~> 4.9'
"#;
        let deps = parse_gemfile(content);
        assert_eq!(deps.len(), 3);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"rails"));
        assert!(names.contains(&"pg"));
        assert!(names.contains(&"devise"));
    }

    #[test]
    fn test_parse_gemfile_no_version_skipped() {
        let content = r#"
gem 'rails'
gem 'pg', '>= 1.4.0'
"#;
        let deps = parse_gemfile(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "pg");
    }

    #[test]
    fn test_parse_gemfile_git_skipped() {
        let content = r#"
gem 'rails', git: 'https://github.com/rails/rails'
gem 'pg', '>= 1.4.0'
"#;
        let deps = parse_gemfile(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "pg");
    }

    #[test]
    fn test_parse_gemfile_double_quotes() {
        let content = r#"gem "puma", ">= 5.0""#;
        let deps = parse_gemfile(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].version_constraint, ">= 5.0");
    }

    #[test]
    fn test_parse_gemfile_version_range_character_offset() {
        let line = "gem 'devise', '~> 4.9'";
        let deps = parse_gemfile(line);
        assert_eq!(deps.len(), 1);
        let dep = &deps[0];
        let expected_start = line.find("~> 4.9").unwrap() as u32;
        assert_eq!(dep.version_range.start.character, expected_start);
        assert_eq!(
            dep.version_range.end.character,
            expected_start + "~> 4.9".len() as u32
        );
    }

    #[test]
    fn test_parse_gemfile_comment_skipped() {
        let content = r#"
# gem 'old', '1.0'
gem 'rails', '~> 7.0'
"#;
        let deps = parse_gemfile(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "rails");
    }

    #[test]
    fn test_pad_to_semver() {
        assert_eq!(pad_to_semver("1.2.3"), Some("1.2.3".to_string()));
        assert_eq!(pad_to_semver("1.2"), Some("1.2.0".to_string()));
        assert_eq!(pad_to_semver("1"), Some("1.0.0".to_string()));
        assert_eq!(pad_to_semver("not-a-version"), None);
    }

    #[test]
    fn test_strip_inline_comment() {
        assert_eq!(
            strip_inline_comment("gem 'rails', '~> 7.0' # web"),
            "gem 'rails', '~> 7.0'"
        );
        assert_eq!(
            strip_inline_comment("gem 'rails', '~> 7.0'"),
            "gem 'rails', '~> 7.0'"
        );
    }

    #[test]
    fn test_is_version_constraint() {
        assert!(is_version_constraint("~> 4.9"));
        assert!(is_version_constraint(">= 1.0"));
        assert!(is_version_constraint("1.2.3"));
        assert!(!is_version_constraint(""));
        assert!(!is_version_constraint("path:"));
    }

    #[test]
    fn test_parse_gemfile_multiple_constraints_takes_first() {
        // When multiple constraints are given, the first version constraint wins.
        let content = r#"gem 'pg', '~> 1.4', '>= 1.4.3'"#;
        let deps = parse_gemfile(content);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "pg");
        assert_eq!(deps[0].version_constraint, "~> 1.4");
    }
}
