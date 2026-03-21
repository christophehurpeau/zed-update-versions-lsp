use super::standard;

/// Normalise Deno bare `v`-prefixed version pins to plain semver.
///
/// `v12.6.1` → `12.6.1`
///
/// The `v` prefix is an artefact of deno.land/x specifier URLs such as
/// `https://deno.land/x/oak@v12.6.1/mod.ts`.  The provider strips the URL
/// down to just the version string (e.g. `v12.6.1`), and this normaliser
/// removes the leading `v` so the semver crate can parse it.
///
/// > **Operator preservation:** `build_replacement_text` treats the `v` as a
/// > prefix operator, so replacements write back `v13.0.0`, not `13.0.0`.
///
/// Everything else falls through to [`standard`].
///
/// Used by: **Deno** (`deno.json`, `import_map.json`) — deno.land/x specifiers.
/// JSR and `npm:` specifiers within Deno already use standard semver operators.
pub fn deno(constraint: &str) -> String {
    let trimmed = constraint.trim();

    if trimmed.starts_with('v') && trimmed.chars().nth(1).is_some_and(|c| c.is_ascii_digit()) {
        return standard(&trimmed[1..]);
    }

    standard(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deno_v_prefix() {
        assert_eq!(deno("v12.6.1"), "12.6.1");
    }

    #[test]
    fn test_deno_no_v_prefix() {
        assert_eq!(deno("^12.6.1"), "^12.6.1");
    }

    #[test]
    fn test_deno_bare_version_no_v() {
        // A bare semver without v-prefix — falls through to standard
        assert_eq!(deno("12.6.1"), "12.6.1");
    }

    #[test]
    fn test_deno_v_not_followed_by_digit() {
        // Not a version pin — falls through unchanged
        assert_eq!(deno("vendor"), "vendor");
    }
}
