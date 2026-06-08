//! URL helpers shared by the HTTP exporters.

/// Ensure `base` ends with `suffix`, exactly once. If `base` already ends
/// with `suffix` it is returned unchanged; otherwise `suffix` is appended
/// after trimming any trailing `/` from `base`. Idempotent: feeding the
/// result back in is a no-op.
pub fn ensure_suffix(base: &str, suffix: &str) -> String {
    if base.ends_with(suffix) {
        base.to_string()
    } else {
        format!("{}{suffix}", base.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_suffix_to_bare_url() {
        assert_eq!(
            ensure_suffix("http://localhost:3100", "/loki/api/v1/push"),
            "http://localhost:3100/loki/api/v1/push"
        );
    }

    #[test]
    fn does_not_double_append_when_already_suffixed() {
        assert_eq!(
            ensure_suffix("http://localhost:4318/v1/traces", "/v1/traces"),
            "http://localhost:4318/v1/traces"
        );
    }

    #[test]
    fn trims_a_trailing_slash_before_appending() {
        assert_eq!(
            ensure_suffix("http://localhost:4318/", "/v1/traces"),
            "http://localhost:4318/v1/traces"
        );
    }
}
