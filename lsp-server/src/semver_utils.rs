use semver::{Version, VersionReq};

/// Normalize ecosystem-specific version constraint syntax to semver VersionReq format.
fn normalize_constraint(constraint: &str) -> String {
    let trimmed = constraint.trim();

    // Handle Ruby-style ~> operator
    if let Some(version_part) = trimmed.strip_prefix("~>") {
        return format!("~{}", version_part.trim());
    }

    // Handle Python == (exact)
    if let Some(version_part) = trimmed.strip_prefix("==") {
        return format!("={}", version_part.trim());
    }

    // Handle Python ~= (compatible release)
    if let Some(version_part) = trimmed.strip_prefix("~=") {
        return format!("~{}", version_part.trim());
    }

    // Handle bare version (no operator) — treat as exact in cargo, ^ in npm contexts
    // For now, if it looks like a bare version, add ^
    if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        // Check if it's already a valid VersionReq
        if VersionReq::parse(trimmed).is_ok() {
            return trimmed.to_string();
        }
        return format!("^{}", trimmed);
    }

    trimmed.to_string()
}

/// Extract the base version number from a constraint string,
/// stripping operators like ^, ~, >=, etc.
pub fn extract_base_version(constraint: &str) -> Option<String> {
    let trimmed = constraint.trim();

    // Strip known operators
    let version_str = trimmed
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

/// Check if a version constraint represents a prerelease (e.g. "^1.0.0-alpha.1").
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
/// Example: `"^1.2.0"` + `"2.0.0"` → `"^2.0.0"`
pub fn build_replacement_text(original: &str, new_version: &str) -> String {
    // Find where the version number starts (first digit)
    let op_end = original.find(|c: char| c.is_ascii_digit()).unwrap_or(0);
    let operator = &original[..op_end];
    format!("{}{}", operator, new_version)
}

/// All update candidates discovered for a given version constraint.
#[derive(Debug, Clone, Default)]
pub struct UpdateCandidates {
    /// Highest version satisfying the current range (shown in tooltip).
    pub in_range: Option<String>,
    /// Best patch update outside the current range (same major.minor, newer patch).
    pub patch: Option<String>,
    /// Best minor update outside the current range (same major, newer minor).
    pub minor: Option<String>,
    /// Best major update available (higher major).
    pub major: Option<String>,
}

/// Given a version constraint and a list of all known stable versions, return the highest
/// version currently satisfying the range plus the best patch/minor/major candidates outside it.
///
/// Returns `None` if the constraint cannot be parsed.
pub fn find_update_candidates(constraint: &str, versions: &[String]) -> Option<UpdateCandidates> {
    let normalized = normalize_constraint(constraint);
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

    // --- normalize_constraint tests ---

    #[test]
    fn test_normalize_caret() {
        assert_eq!(normalize_constraint("^1.2.3"), "^1.2.3");
    }

    #[test]
    fn test_normalize_tilde() {
        assert_eq!(normalize_constraint("~1.2.3"), "~1.2.3");
    }

    #[test]
    fn test_normalize_ruby_pessimistic() {
        assert_eq!(normalize_constraint("~> 1.2"), "~1.2");
    }

    #[test]
    fn test_normalize_python_compatible() {
        assert_eq!(normalize_constraint("~=1.2"), "~1.2");
    }

    #[test]
    fn test_normalize_python_exact() {
        assert_eq!(normalize_constraint("==1.2.3"), "=1.2.3");
    }

    // --- extract_base_version tests ---

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

    // --- build_replacement_text tests ---

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

    // --- find_latest tests ---

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

    // --- find_update_candidates tests ---

    #[test]
    fn test_find_update_candidates_in_range_only() {
        // Versions in-range but newer than base are still reported as patch updates
        let versions = vec!["1.2.5".to_string(), "1.2.3".to_string()];
        let candidates = find_update_candidates("^1.2.0", &versions).unwrap();
        assert_eq!(candidates.in_range, Some("1.2.5".to_string()));
        assert_eq!(candidates.patch, Some("1.2.5".to_string()));
        assert!(candidates.minor.is_none());
        assert!(candidates.major.is_none());
    }

    #[test]
    fn test_find_update_candidates_patch() {
        let versions = vec!["1.2.5".to_string()];
        let candidates = find_update_candidates("=1.2.0", &versions).unwrap();
        assert_eq!(candidates.patch, Some("1.2.5".to_string()));
        assert!(candidates.minor.is_none());
        assert!(candidates.major.is_none());
    }

    #[test]
    fn test_find_update_candidates_minor() {
        let versions = vec!["1.3.0".to_string(), "1.2.5".to_string()];
        let candidates = find_update_candidates("~1.2.0", &versions).unwrap();
        // ~1.2.0 is >=1.2.0 <1.3.0 so 1.2.5 is in range (and a patch update), 1.3.0 is a minor update
        assert_eq!(candidates.in_range, Some("1.2.5".to_string()));
        assert_eq!(candidates.minor, Some("1.3.0".to_string()));
        assert_eq!(candidates.patch, Some("1.2.5".to_string()));
        assert!(candidates.major.is_none());
    }

    #[test]
    fn test_find_update_candidates_major() {
        let versions = vec!["2.0.0".to_string(), "1.2.5".to_string()];
        let candidates = find_update_candidates("^1.2.0", &versions).unwrap();
        assert_eq!(candidates.in_range, Some("1.2.5".to_string()));
        assert_eq!(candidates.major, Some("2.0.0".to_string()));
        assert!(candidates.minor.is_none());
        assert_eq!(candidates.patch, Some("1.2.5".to_string()));
    }

    #[test]
    fn test_find_update_candidates_picks_best() {
        // Multiple versions available across all tiers — best of each should win.
        let versions = vec![
            "3.0.0".to_string(),
            "2.1.0".to_string(),
            "2.0.0".to_string(),
            "1.3.0".to_string(),
            "1.2.2".to_string(),
            "1.2.1".to_string(),
        ];
        let candidates = find_update_candidates("~1.2.0", &versions).unwrap();
        // ~1.2.0 is >=1.2.0 <1.3.0; 1.2.2 and 1.2.1 are in range (and patch updates), best patch is 1.2.2
        assert_eq!(candidates.in_range, Some("1.2.2".to_string()));
        assert_eq!(candidates.minor, Some("1.3.0".to_string()));
        assert_eq!(candidates.major, Some("3.0.0".to_string()));
        assert_eq!(candidates.patch, Some("1.2.2".to_string()));
    }

    #[test]
    fn test_find_update_candidates_invalid_constraint() {
        let versions = vec!["1.0.0".to_string()];
        // Wildcard "*" can't be parsed to a base version
        assert!(find_update_candidates("*", &versions).is_none());
    }

    #[test]
    fn test_find_update_candidates_empty_versions() {
        let candidates = find_update_candidates("^1.0.0", &[]).unwrap();
        assert!(candidates.in_range.is_none());
        assert!(candidates.patch.is_none());
        assert!(candidates.minor.is_none());
        assert!(candidates.major.is_none());
    }

    // --- is_prerelease_constraint tests ---

    #[test]
    fn test_is_prerelease_constraint_yes() {
        assert!(is_prerelease_constraint("^1.0.0-alpha.1"));
        assert!(is_prerelease_constraint("1.0.0-beta.2"));
        assert!(is_prerelease_constraint("~2.0.0-rc.1"));
    }

    #[test]
    fn test_is_prerelease_constraint_no() {
        assert!(!is_prerelease_constraint("^1.0.0"));
        assert!(!is_prerelease_constraint("~1.2.3"));
        assert!(!is_prerelease_constraint(">=2.0.0"));
    }

    // --- prerelease_newer_than_constraint tests ---

    #[test]
    fn test_prerelease_newer_than_constraint_yes() {
        assert!(prerelease_newer_than_constraint("^1.0.0", "2.0.0-alpha.1"));
        assert!(prerelease_newer_than_constraint("^1.0.0", "1.1.0-rc.1"));
        assert!(prerelease_newer_than_constraint("~1.2.0", "1.3.0-beta.1"));
    }

    #[test]
    fn test_prerelease_newer_than_constraint_no() {
        // Pre-release same base as constraint base — not strictly greater
        assert!(!prerelease_newer_than_constraint("^1.0.0", "1.0.0-alpha.1"));
        // Pre-release older than constraint base
        assert!(!prerelease_newer_than_constraint("^2.0.0", "1.9.9-rc.1"));
    }

    #[test]
    fn test_prerelease_newer_than_constraint_invalid() {
        assert!(!prerelease_newer_than_constraint("^1.0.0", "not-a-version"));
        assert!(!prerelease_newer_than_constraint("*", "2.0.0-alpha.1"));
    }
}
