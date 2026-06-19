//! Query normalisation shared by both the MCP and REST search paths.
//!
//! nomic-embed-text collapses single CamelCase tokens (e.g. `AnalyticsFlow`)
//! to a constant degenerate vector because the tokenizer treats the whole token
//! as a single unit.  Splitting at camelCase boundaries, replacing Rust path
//! separators, and prepending the Nomic asymmetric task prefix restores
//! distinct, meaningful vectors.

/// Normalize a raw search query before sending it to Ollama for embedding.
pub(crate) fn normalize_query(query: &str) -> String {
    // Replace Rust path separators and underscores with spaces.
    let s = query.replace("::", " ").replace('_', " ");

    // Insert spaces at camelCase boundaries.
    let mut with_spaces = String::with_capacity(s.len() + 16);
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            if prev.is_lowercase() || prev.is_ascii_digit() {
                // lowercase→uppercase: always split ("FooBar" → "Foo Bar")
                with_spaces.push(' ');
            } else if prev.is_uppercase() {
                // acronym run: split before the last uppercase that precedes
                // a lowercase ("FQNMethod" → "FQN Method")
                if let Some(&next) = chars.get(i + 1) {
                    if next.is_lowercase() {
                        with_spaces.push(' ');
                    }
                }
            }
        }
        with_spaces.push(c);
    }

    // Collapse whitespace, lowercase, and add the Nomic asymmetric task prefix.
    let normalized = with_spaces.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("search_query: {}", normalized.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_camel_case() {
        assert_eq!(normalize_query("HelloWorld"), "search_query: hello world");
        assert_eq!(
            normalize_query("AnalyticsFlow"),
            "search_query: analytics flow"
        );
        assert_eq!(normalize_query("FooBar"), "search_query: foo bar");
    }

    #[test]
    fn handles_underscores_and_colons() {
        assert_eq!(
            normalize_query("analytics_flow"),
            "search_query: analytics flow"
        );
        assert_eq!(normalize_query("module::Type"), "search_query: module type");
        assert_eq!(
            normalize_query("my_crate::MyStruct"),
            "search_query: my crate my struct"
        );
    }

    #[test]
    fn handles_acronym_runs() {
        assert_eq!(normalize_query("FQNMethod"), "search_query: fqn method");
    }

    #[test]
    fn single_camel_case_tokens_produce_distinct_results() {
        // All three inputs must embed to distinct vectors — the core bug fix.
        let a = normalize_query("AnalyticsFlow");
        let b = normalize_query("HelloWorld");
        let c = normalize_query("FooBar");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }
}
