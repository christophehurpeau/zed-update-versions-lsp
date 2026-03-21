use super::standard;

/// Normalise PEP 440 constraint operators to their semver equivalents.
///
/// | PEP 440  | semver  | Semantics                       |
/// |----------|---------|---------------------------------|
/// | `==x`    | `=x`    | Exact version match             |
/// | `~=x.y`  | `~x.y`  | Compatible release (≥ x.y, < x+1) |
///
/// `!=`, `>=`, `<=`, `>`, `<` are already valid semver operators and pass
/// through unchanged.  Everything else falls through to [`standard`].
///
/// > **Note on PEP 440 version strings:** PyPI versions like `1.0rc1`, `1.0b2`,
/// > `.post1` are coerced to semver equivalents (`1.0.0-rc.1`, `1.0.0-beta.2`)
/// > by the PyPI *provider* before they reach version comparison.  This
/// > normaliser only handles the **constraint syntax**, not the version strings.
///
/// Used by: **PyPI** (`requirements.txt`, `pyproject.toml`).
pub fn python(constraint: &str) -> String {
    let trimmed = constraint.trim();

    if let Some(version_part) = trimmed.strip_prefix("==") {
        return format!("={}", version_part.trim());
    }
    if let Some(version_part) = trimmed.strip_prefix("~=") {
        return format!("~{}", version_part.trim());
    }

    standard(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_exact() {
        assert_eq!(python("==1.2.3"), "=1.2.3");
    }

    #[test]
    fn test_python_compatible() {
        assert_eq!(python("~=1.2"), "~1.2");
    }

    #[test]
    fn test_python_passthrough_gte() {
        assert_eq!(python(">=1.2.0"), ">=1.2.0");
    }

    #[test]
    fn test_python_passthrough_neq() {
        assert_eq!(python("!=1.2.0"), "!=1.2.0");
    }
}
