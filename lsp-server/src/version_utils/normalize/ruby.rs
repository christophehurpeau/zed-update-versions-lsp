use super::standard;

/// Normalise Ruby/Gemfile and Dub pessimistic version constraint syntax (`~>`).
///
/// | Input      | Output              | Semantics                          |
/// |------------|---------------------|------------------------------------|
/// | `~> 7`     | `>=7.0.0, <8.0.0`  | ≥ 7.0, < 8        (1 component)   |
/// | `~> 7.1`   | `>=7.1.0, <8.0.0`  | ≥ 7.1, < 8        (2 components)  |
/// | `~> 7.1.0` | `~7.1.0`           | ≥ 7.1.0, < 7.2.0  (3+ components) |
///
/// All other operators fall through to [`standard`].
///
/// Used by: **RubyGems** (`Gemfile`), **Dub** (`dub.json`, `dub.sdl`).
pub fn ruby(constraint: &str) -> String {
    let trimmed = constraint.trim();

    if let Some(version_part) = trimmed.strip_prefix("~>") {
        let v = version_part.trim();
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() <= 2 {
            if let Ok(major) = parts[0].parse::<u64>() {
                let minor = parts.get(1).copied().unwrap_or("0");
                return format!(">={}.{}.0, <{}.0.0", major, minor, major + 1);
            }
        }
        return format!("~{}", v);
    }

    standard(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ~> pessimistic constraint operator
    #[test]
    fn test_ruby_pessimistic_one_part() {
        assert_eq!(ruby("~> 7"), ">=7.0.0, <8.0.0");
    }

    #[test]
    fn test_ruby_pessimistic_one_part_zero() {
        assert_eq!(ruby("~> 0"), ">=0.0.0, <1.0.0");
    }

    #[test]
    fn test_ruby_pessimistic_two_part() {
        assert_eq!(ruby("~> 7.1"), ">=7.1.0, <8.0.0");
    }

    #[test]
    fn test_ruby_pessimistic_two_part_zero_major() {
        assert_eq!(ruby("~> 0.9"), ">=0.9.0, <1.0.0");
    }

    #[test]
    fn test_ruby_pessimistic_three_part() {
        assert_eq!(ruby("~> 7.1.0"), "~7.1.0");
    }

    #[test]
    fn test_ruby_pessimistic_four_part() {
        // 4-component versions (uncommon but valid in RubyGems)
        assert_eq!(ruby("~> 7.1.0.1"), "~7.1.0.1");
    }

    // Standard Ruby comparison operators (passed through as-is)
    #[test]
    fn test_ruby_exact() {
        // Ruby uses = for exact match; semver crate also recognises =
        assert_eq!(ruby("= 1.0.0"), "= 1.0.0");
    }

    #[test]
    fn test_ruby_not_equal() {
        // != is valid Ruby syntax; no semver-crate equivalent, passes through
        assert_eq!(ruby("!= 1.0.0"), "!= 1.0.0");
    }

    #[test]
    fn test_ruby_gte() {
        assert_eq!(ruby(">= 1.0.0"), ">= 1.0.0");
    }

    #[test]
    fn test_ruby_lte() {
        assert_eq!(ruby("<= 2.0.0"), "<= 2.0.0");
    }

    #[test]
    fn test_ruby_gt() {
        assert_eq!(ruby("> 1.0.0"), "> 1.0.0");
    }

    #[test]
    fn test_ruby_lt() {
        assert_eq!(ruby("< 2.0.0"), "< 2.0.0");
    }

    // Bare version (no operator)
    #[test]
    fn test_ruby_bare_version() {
        assert_eq!(ruby("7.0.0"), "7.0.0");
    }

    // Whitespace handling
    #[test]
    fn test_ruby_leading_trailing_whitespace() {
        assert_eq!(ruby("  ~> 7.1  "), ">=7.1.0, <8.0.0");
    }
}
