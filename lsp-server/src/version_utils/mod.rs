pub mod normalize;

use semver::{Version, VersionReq};

/// Extract the base version number from a constraint string,
/// stripping operators like `^`, `~`, `>=`, `~>`, etc.
pub fn extract_base_version(constraint: &str) -> Option<String> {
    let trimmed = constraint.trim();

    // Strip known operators (longest first to avoid partial matches).
    let stripped = trimmed
        .trim_start_matches("~>")
        .trim_start_matches(">=")
        .trim_start_matches("<=")
        .trim_start_matches("~=")
        .trim_start_matches("==")
        .trim_start_matches("!=")
        .trim_start_matches('^')
        .trim_start_matches('~')
        .trim_start_matches('>')
        .trim_start_matches('<')
        .trim_start_matches('=')
        .trim();

    // Strip bare v-prefix only when followed by a digit (e.g. "v12.6.1" → "12.6.1").
    let version_str = if stripped.starts_with('v')
        && stripped.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
    {
        &stripped[1..]
    } else {
        stripped
    };

    if version_str.is_empty() {
        return None;
    }

    // Pad to 3-component semver if needed (e.g. "1.2" → "1.2.0")
    let parts: Vec<&str> = version_str.split('.').collect();
    match parts.len() {
        1 => Some(format!("{}.0.0", parts[0])),
        2 => Some(format!("{}.{}.0", parts[0], parts[1])),
        _ => Some(version_str.to_string()),
    }
}

/// Check if a version constraint represents a prerelease (e.g. `"^1.0.0-alpha.1"`).
pub fn is_prerelease_constraint(constraint: &str) -> bool {
    if let Some(base) = extract_base_version(constraint) {
        if let Ok(v) = Version::parse(&base) {
            return !v.pre.is_empty();
        }
    }
    false
}

/// Return true if `prerelease_version` is strictly greater than the base version
/// extracted from `constraint`. Used to guard against showing outdated prereleases.
pub fn prerelease_newer_than_constraint(constraint: &str, prerelease_version: &str) -> bool {
    let pre = match Version::parse(prerelease_version) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let base_str = match extract_base_version(constraint) {
        Some(s) => s,
        None => return false,
    };
    let base = match Version::parse(&base_str) {
        Ok(v) => v,
        Err(_) => return false,
    };
    pre > base
}

/// Build a replacement version string preserving the original operator prefix.
///
/// Example: `"^1.2.0"` + `"2.0.0"` → `"^2.0.0"`.
/// Works for any prefix — including `v` (Deno), `~>` (Ruby), `==` (Python).
pub fn build_replacement_text(original: &str, new_version: &str) -> String {
    // Find where the version number starts (first digit).
    let op_end = original.find(|c: char| c.is_ascii_digit()).unwrap_or(0);
    let operator = &original[..op_end];
    format!("{}{}", operator, new_version)
}

/// All update candidates discovered for a given version constraint.
#[derive(Debug, Clone, Default)]
pub struct UpdateCandidates {
    /// Highest version satisfying the current range (shown in tooltip).
    pub in_range: Option<String>,
    /// Best patch update (same major.minor, newer patch).
    pub patch: Option<String>,
    /// Best minor update (same major, newer minor).
    pub minor: Option<String>,
    /// Best major update (higher major).
    pub major: Option<String>,
}

/// Given a version constraint, a list of all known stable versions, and an
/// ecosystem-specific normaliser, return the highest version currently
/// satisfying the range plus the best patch/minor/major candidates outside it.
///
/// The `normalize` closure translates the ecosystem's native constraint syntax
/// to a string that [`semver::VersionReq`] can parse.  Callers should pass the
/// normaliser that matches their ecosystem:
///
/// | Ecosystem | Normaliser |
/// |-----------|------------|
/// | npm, Cargo, Composer, Pub | [`normalize::standard`] |
/// | RubyGems, Dub             | [`normalize::ruby`]     |
/// | PyPI                      | [`normalize::python`]   |
/// | Deno (deno.land/x)        | [`normalize::deno`]     |
///
/// Returns `None` if the constraint cannot be parsed (marked as `Unsupported`).
pub fn find_update_candidates(
    constraint: &str,
    versions: &[String],
    normalize: impl Fn(&str) -> String,
) -> Option<UpdateCandidates> {
    let normalized = normalize(constraint);
    let req = VersionReq::parse(&normalized).ok()?;
    let base_str = extract_base_version(constraint)?;
    let base = Version::parse(&base_str).ok()?;

    let mut in_range: Option<Version> = None;
    let mut patch: Option<Version> = None;
    let mut minor: Option<Version> = None;
    let mut major: Option<Version> = None;

    for v_str in versions {
        let v = match Version::parse(v_str) {
            Ok(v) if v.pre.is_empty() => v,
            _ => continue,
        };

        if req.matches(&v) && in_range.as_ref().is_none_or(|best| v > *best) {
            in_range = Some(v.clone());
        }

        if v > base {
            if v.major > base.major {
                if major.as_ref().is_none_or(|best| v > *best) {
                    major = Some(v);
                }
            } else if v.major == base.major && v.minor > base.minor {
                if minor.as_ref().is_none_or(|best| v > *best) {
                    minor = Some(v);
                }
            } else if v.major == base.major
                && v.minor == base.minor
                && v.patch > base.patch
                && patch.as_ref().is_none_or(|best| v > *best)
            {
                patch = Some(v);
            }
        }
    }

    Some(UpdateCandidates {
        in_range: in_range.map(|v| v.to_string()),
        patch: patch.map(|v| v.to_string()),
        minor: minor.map(|v| v.to_string()),
        major: major.map(|v| v.to_string()),
    })
}

/// Return the highest stable version from a list, or `None` if the list is empty.
pub fn find_latest(versions: &[String]) -> Option<String> {
    versions
        .iter()
        .filter_map(|v| {
            Version::parse(v)
                .ok()
                .filter(|p| p.pre.is_empty())
                .map(|p| (v, p))
        })
        .max_by(|(_, a), (_, b)| a.cmp(b))
        .map(|(s, _)| s.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use normalize::{deno, ruby, standard};

    // --- extract_base_version ---

    #[test]
    fn test_extract_base_caret() {
        assert_eq!(extract_base_version("^1.2.3"), Some("1.2.3".to_string()));
    }

    #[test]
    fn test_extract_base_tilde() {
        assert_eq!(extract_base_version("~1.2.3"), Some("1.2.3".to_string()));
    }

    #[test]
    fn test_extract_base_gte() {
        assert_eq!(extract_base_version(">=1.2.0"), Some("1.2.0".to_string()));
    }

    #[test]
    fn test_extract_base_bare() {
        assert_eq!(extract_base_version("1.2.3"), Some("1.2.3".to_string()));
    }

    #[test]
    fn test_extract_base_two_component() {
        assert_eq!(extract_base_version("~1.2"), Some("1.2.0".to_string()));
    }

    #[test]
    fn test_extract_base_one_component() {
        assert_eq!(extract_base_version("^1"), Some("1.0.0".to_string()));
    }

    #[test]
    fn test_extract_base_v_prefix() {
        assert_eq!(extract_base_version("v12.6.1"), Some("12.6.1".to_string()));
    }

    #[test]
    fn test_extract_base_ruby_pessimistic() {
        assert_eq!(extract_base_version("~> 7.1"), Some("7.1.0".to_string()));
    }

    // --- build_replacement_text ---

    #[test]
    fn test_replace_caret() {
        assert_eq!(build_replacement_text("^1.2.0", "2.0.0"), "^2.0.0");
    }

    #[test]
    fn test_replace_tilde() {
        assert_eq!(build_replacement_text("~1.2.0", "1.2.5"), "~1.2.5");
    }

    #[test]
    fn test_replace_gte() {
        assert_eq!(build_replacement_text(">=1.0.0", "2.0.0"), ">=2.0.0");
    }

    #[test]
    fn test_replace_bare() {
        assert_eq!(build_replacement_text("1.0.0", "2.0.0"), "2.0.0");
    }

    #[test]
    fn test_replace_ruby_pessimistic() {
        assert_eq!(build_replacement_text("~>1.2", "1.3.0"), "~>1.3.0");
    }

    #[test]
    fn test_replace_v_prefix() {
        assert_eq!(build_replacement_text("v12.6.1", "13.0.0"), "v13.0.0");
    }

    // --- find_latest ---

    #[test]
    fn test_find_latest_picks_highest() {
        let versions = vec![
            "1.0.0".to_string(),
            "2.0.0".to_string(),
            "1.5.0".to_string(),
        ];
        assert_eq!(find_latest(&versions), Some("2.0.0".to_string()));
    }

    #[test]
    fn test_find_latest_ignores_prerelease() {
        let versions = vec!["1.0.0".to_string(), "2.0.0-alpha.1".to_string()];
        assert_eq!(find_latest(&versions), Some("1.0.0".to_string()));
    }

    #[test]
    fn test_find_latest_empty() {
        assert_eq!(find_latest(&[]), None);
    }

    // --- find_update_candidates (standard semver: npm, Cargo, Composer, Pub) ---

    #[test]
    fn test_candidates_in_range_only() {
        let versions = vec!["1.2.5".to_string(), "1.2.3".to_string()];
        let c = find_update_candidates("^1.2.0", &versions, standard).unwrap();
        assert_eq!(c.in_range, Some("1.2.5".to_string()));
        assert_eq!(c.patch, Some("1.2.5".to_string()));
        assert!(c.minor.is_none());
        assert!(c.major.is_none());
    }

    #[test]
    fn test_candidates_patch() {
        let versions = vec!["1.2.5".to_string()];
        let c = find_update_candidates("=1.2.0", &versions, standard).unwrap();
        assert_eq!(c.patch, Some("1.2.5".to_string()));
        assert!(c.minor.is_none());
        assert!(c.major.is_none());
    }

    #[test]
    fn test_candidates_minor() {
        let versions = vec!["1.3.0".to_string(), "1.2.5".to_string()];
        let c = find_update_candidates("~1.2.0", &versions, standard).unwrap();
        assert_eq!(c.in_range, Some("1.2.5".to_string()));
        assert_eq!(c.minor, Some("1.3.0".to_string()));
        assert_eq!(c.patch, Some("1.2.5".to_string()));
        assert!(c.major.is_none());
    }

    #[test]
    fn test_candidates_major() {
        let versions = vec!["2.0.0".to_string(), "1.2.5".to_string()];
        let c = find_update_candidates("^1.2.0", &versions, standard).unwrap();
        assert_eq!(c.in_range, Some("1.2.5".to_string()));
        assert_eq!(c.major, Some("2.0.0".to_string()));
        assert!(c.minor.is_none());
        assert_eq!(c.patch, Some("1.2.5".to_string()));
    }

    #[test]
    fn test_candidates_picks_best() {
        let versions = vec![
            "3.0.0".to_string(),
            "2.1.0".to_string(),
            "2.0.0".to_string(),
            "1.3.0".to_string(),
            "1.2.2".to_string(),
            "1.2.1".to_string(),
        ];
        let c = find_update_candidates("~1.2.0", &versions, standard).unwrap();
        assert_eq!(c.in_range, Some("1.2.2".to_string()));
        assert_eq!(c.minor, Some("1.3.0".to_string()));
        assert_eq!(c.major, Some("3.0.0".to_string()));
        assert_eq!(c.patch, Some("1.2.2".to_string()));
    }

    #[test]
    fn test_candidates_invalid_constraint() {
        let versions = vec!["1.0.0".to_string()];
        assert!(find_update_candidates("*", &versions, standard).is_none());
    }

    #[test]
    fn test_candidates_empty_versions() {
        let c = find_update_candidates("^1.0.0", &[], standard).unwrap();
        assert!(c.in_range.is_none());
        assert!(c.patch.is_none());
        assert!(c.minor.is_none());
        assert!(c.major.is_none());
    }

    // --- find_update_candidates with ruby normaliser (RubyGems, Dub) ---

    #[test]
    fn test_candidates_ruby_pessimistic() {
        let versions = vec![
            "7.1.0".to_string(),
            "7.2.0".to_string(),
            "8.0.0".to_string(),
        ];
        let c = find_update_candidates("~> 7.1", &versions, ruby).unwrap();
        assert_eq!(c.in_range, Some("7.2.0".to_string()));
        assert_eq!(c.minor, Some("7.2.0".to_string()));
        assert_eq!(c.major, Some("8.0.0".to_string()));
        assert!(c.patch.is_none());
    }

    // --- find_update_candidates with deno normaliser (Deno deno.land/x) ---

    #[test]
    fn test_candidates_deno_v_prefix() {
        let versions = vec![
            "12.6.1".to_string(),
            "12.7.0".to_string(),
            "13.0.0".to_string(),
        ];
        let c = find_update_candidates("v12.6.1", &versions, deno).unwrap();
        assert_eq!(c.in_range, Some("12.7.0".to_string()));
        assert_eq!(c.minor, Some("12.7.0".to_string()));
        assert_eq!(c.major, Some("13.0.0".to_string()));
        assert!(c.patch.is_none());
    }

    // --- is_prerelease_constraint ---

    #[test]
    fn test_is_prerelease_yes() {
        assert!(is_prerelease_constraint("^1.0.0-alpha.1"));
        assert!(is_prerelease_constraint("1.0.0-beta.2"));
        assert!(is_prerelease_constraint("~2.0.0-rc.1"));
    }

    #[test]
    fn test_is_prerelease_no() {
        assert!(!is_prerelease_constraint("^1.0.0"));
        assert!(!is_prerelease_constraint("~1.2.3"));
        assert!(!is_prerelease_constraint(">=2.0.0"));
    }

    // --- prerelease_newer_than_constraint ---

    #[test]
    fn test_prerelease_newer_yes() {
        assert!(prerelease_newer_than_constraint("^1.0.0", "2.0.0-alpha.1"));
        assert!(prerelease_newer_than_constraint("^1.0.0", "1.1.0-rc.1"));
        assert!(prerelease_newer_than_constraint("~1.2.0", "1.3.0-beta.1"));
    }

    #[test]
    fn test_prerelease_newer_no() {
        assert!(!prerelease_newer_than_constraint("^1.0.0", "1.0.0-alpha.1"));
        assert!(!prerelease_newer_than_constraint("^2.0.0", "1.9.9-rc.1"));
    }

    #[test]
    fn test_prerelease_newer_invalid() {
        assert!(!prerelease_newer_than_constraint("^1.0.0", "not-a-version"));
        assert!(!prerelease_newer_than_constraint("*", "2.0.0-alpha.1"));
    }
}
