mod deno;
mod python;
mod ruby;

pub use deno::deno;
pub use python::python;
pub use ruby::ruby;

use semver::VersionReq;

/// Standard semver normalisation: handles bare version strings (no operator)
/// and passes everything else through unchanged.
///
/// A bare version like `1.2.3` that is already a valid [`VersionReq`] is
/// returned as-is (the `semver` crate accepts bare versions as `^`-style
/// ranges).  A bare version that is *not* a valid `VersionReq` (e.g. a
/// two-component `1.2`) is prefixed with `^`.
///
/// Used by: **npm** (`package.json`), **Cargo** (`Cargo.toml`),
/// **Composer** (`composer.json`), **Pub** (`pubspec.yaml`).
pub fn standard(constraint: &str) -> String {
    let trimmed = constraint.trim();

    if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        if VersionReq::parse(trimmed).is_ok() {
            return trimmed.to_string();
        }
        return format!("^{}", trimmed);
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standard_caret_passthrough() {
        assert_eq!(standard("^1.2.3"), "^1.2.3");
    }

    #[test]
    fn test_standard_tilde_passthrough() {
        assert_eq!(standard("~1.2.3"), "~1.2.3");
    }

    #[test]
    fn test_standard_bare_valid() {
        assert_eq!(standard("1.2.3"), "1.2.3");
    }

    #[test]
    fn test_standard_bare_two_part() {
        // "1.2" is a valid VersionReq in the semver crate (treated as ^1.2) — passed through as-is.
        assert_eq!(standard("1.2"), "1.2");
    }
}
