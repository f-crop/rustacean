//! Ollama HTTP client and composite embedding input builder.
//!
//! The composite prompt follows ADR-007 §3.5.7:
//!   fqn / `resolved_type_signature` / `trait_bounds` / source text
//!
//! All fields are best-effort: missing fields are omitted rather than erroring
//! (PARTIAL_* quality items per ADR-007 §13.4).

use anyhow::{Context as _, Result};
use serde_json::json;

/// Hard character limits for each composite field.
///
/// `nomic-embed-text` context: 8192 `WordPiece` tokens.  Code tokenises at
/// roughly 2–4 chars per token; complex generic signatures can tokenise as
/// low as 1 char per token.  Conservative per-field caps keep the total
/// well inside the model limit even in the worst case.
///
/// Budget (chars):
///   fqn              ≤   300  (generous for deep module paths)
///   `type_signature` ≤ 2 000  (complex generics in macro-generated items)
///   `trait_bounds`   ≤ 1 000  (joined; individual bounds are shorter)
///   source           ≤ 4 000  (most of the semantic content lives here)
///   labels/newlines  ≤    50
///   total            ≤ 7 350  → worst-case 7 350 tokens < 8 192 limit
const MAX_SIGNATURE_CHARS: usize = 2_000;
const MAX_BOUNDS_CHARS: usize = 1_000;
const MAX_SOURCE_CHARS: usize = 4_000;

/// Build the §3.5.7 composite embedding prompt for one item.
///
/// Fields that are empty or absent are omitted. Each field is capped at its
/// constant limit (see above) so the composite fits within the embedding
/// model context window on any tokeniser.
pub(crate) fn build_composite(
    fqn: &str,
    type_signature: &str,
    trait_bounds: &[String],
    source_text: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::with_capacity(5);

    parts.push(format!("fqn: {fqn}"));

    if !type_signature.is_empty() {
        if type_signature.len() > MAX_SIGNATURE_CHARS {
            parts.push(format!(
                "signature: {}[...]",
                &type_signature[..MAX_SIGNATURE_CHARS]
            ));
        } else {
            parts.push(format!("signature: {type_signature}"));
        }
    }

    if !trait_bounds.is_empty() {
        let joined = trait_bounds.join(", ");
        if joined.len() > MAX_BOUNDS_CHARS {
            parts.push(format!("bounds: {}[...]", &joined[..MAX_BOUNDS_CHARS]));
        } else {
            parts.push(format!("bounds: {joined}"));
        }
    }

    if let Some(src) = source_text {
        let trimmed = src.trim();
        if !trimmed.is_empty() {
            if trimmed.len() > MAX_SOURCE_CHARS {
                parts.push(format!(
                    "source:\n{}\n[... truncated]",
                    &trimmed[..MAX_SOURCE_CHARS]
                ));
            } else {
                parts.push(format!("source:\n{trimmed}"));
            }
        }
    }

    parts.join("\n")
}

/// POST to `{ollama_url}/api/embeddings` and return the embedding vector.
///
/// Ollama response: `{"embedding": [f64, ...]}`
#[allow(clippy::cast_possible_truncation)]
pub(crate) async fn call_ollama(
    http: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    prompt: &str,
) -> Result<Vec<f32>> {
    let url = format!("{ollama_url}/api/embeddings");
    let body = json!({ "model": model, "prompt": prompt });

    let resp = http
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("Ollama request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Ollama returned HTTP {status}: {text}");
    }

    let json: serde_json::Value = resp.json().await.context("Ollama response is not JSON")?;

    let embedding = json
        .get("embedding")
        .and_then(|v| v.as_array())
        .context("Ollama response missing 'embedding' array")?;

    let vector: Vec<f32> = embedding
        .iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_f64()
                .map(|f| f as f32)
                .with_context(|| format!("embedding[{i}] is not a number"))
        })
        .collect::<Result<_>>()?;

    Ok(vector)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_includes_all_non_empty_fields() {
        let composite = build_composite(
            "src_lib::Foo",
            "impl Display for Foo",
            &["T: Clone".to_owned()],
            Some("pub struct Foo;"),
        );
        assert!(composite.contains("fqn: src_lib::Foo"));
        assert!(composite.contains("signature: impl Display for Foo"));
        assert!(composite.contains("bounds: T: Clone"));
        assert!(composite.contains("source:\npub struct Foo;"));
    }

    #[test]
    fn composite_omits_empty_fields() {
        let composite = build_composite("src_lib::Bar", "", &[], None);
        assert_eq!(composite, "fqn: src_lib::Bar");
        assert!(!composite.contains("signature"));
        assert!(!composite.contains("bounds"));
        assert!(!composite.contains("source"));
    }

    #[test]
    fn composite_multiple_bounds() {
        let composite = build_composite(
            "my_mod::process",
            "fn process<T>()",
            &["T: Clone".to_owned(), "T: Send".to_owned()],
            None,
        );
        assert!(composite.contains("bounds: T: Clone, T: Send"));
    }

    #[test]
    fn composite_trims_source_whitespace() {
        let composite = build_composite("x::y", "", &[], Some("  fn foo() {}  "));
        assert!(composite.contains("source:\nfn foo() {}"));
    }

    #[test]
    fn composite_skips_whitespace_only_source() {
        let composite = build_composite("x::y", "", &[], Some("   \n  "));
        assert!(!composite.contains("source"));
    }

    #[test]
    fn composite_truncates_long_source() {
        let long_src = "x".repeat(MAX_SOURCE_CHARS + 100);
        let composite = build_composite("x::y", "", &[], Some(&long_src));
        assert!(
            composite.contains("[... truncated]"),
            "must append truncation marker"
        );
        let source_line = composite.lines().skip(1).collect::<Vec<_>>().join("\n");
        assert!(
            source_line.len() <= MAX_SOURCE_CHARS + 50,
            "source section must not exceed MAX_SOURCE_CHARS by more than the marker"
        );
    }

    #[test]
    fn composite_does_not_truncate_source_within_limit() {
        let short_src = "fn foo() {}";
        let composite = build_composite("x::y", "", &[], Some(short_src));
        assert!(!composite.contains("[... truncated]"));
        assert!(composite.contains("fn foo() {}"));
    }

    #[test]
    fn composite_truncates_exactly_at_boundary() {
        let exact_src = "a".repeat(MAX_SOURCE_CHARS);
        let composite = build_composite("x::y", "", &[], Some(&exact_src));
        assert!(
            !composite.contains("[... truncated]"),
            "exact limit must not truncate"
        );
    }
}
