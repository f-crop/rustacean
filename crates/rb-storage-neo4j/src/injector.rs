use crate::error::CypherError;

#[derive(PartialEq, Eq)]
enum ScanState {
    Normal,
    SingleQuote,
    DoubleQuote,
    Backtick,
    LineComment,
    BlockComment,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum PathClauseKind {
    Match,
    CreateOrMerge,
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Keywords that put us into a path-pattern context (node patterns expected).
/// Returns the kind of clause so the injector can track bound variables.
fn path_start_kind(upper: &str) -> Option<PathClauseKind> {
    match upper {
        "MATCH" => Some(PathClauseKind::Match),
        "CREATE" | "MERGE" => Some(PathClauseKind::CreateOrMerge),
        _ => None,
    }
}

/// Keywords that end the current path-pattern context.
fn is_path_end(upper: &str) -> bool {
    matches!(
        upper,
        "WHERE"
            | "WITH"
            | "RETURN"
            | "SET"
            | "REMOVE"
            | "DELETE"
            | "DETACH"
            | "ORDER"
            | "SKIP"
            | "LIMIT"
            | "UNION"
            | "UNWIND"
            | "CALL"
            | "YIELD"
            | "CASE"
            | "WHEN"
            | "THEN"
            | "ELSE"
            | "END"
    )
}

/// Scans from `*i` to the matching `]`, advancing `*i` past the `]`.
/// Handles strings inside relationship patterns so keywords within them are not misread.
#[allow(clippy::unnecessary_wraps)]
fn scan_bracket(bytes: &[u8], i: &mut usize) -> Result<String, CypherError> {
    let mut out = String::new();
    let mut depth: usize = 1;
    let mut in_single = false;
    let mut in_double = false;

    while *i < bytes.len() {
        let b = bytes[*i];
        if in_single {
            out.push(b as char);
            *i += 1;
            if b == b'\'' {
                in_single = false;
            }
        } else if in_double {
            out.push(b as char);
            *i += 1;
            if b == b'"' {
                in_double = false;
            }
        } else {
            match b {
                b'\'' => {
                    in_single = true;
                    out.push(b as char);
                    *i += 1;
                }
                b'"' => {
                    in_double = true;
                    out.push(b as char);
                    *i += 1;
                }
                b'[' => {
                    depth += 1;
                    out.push(b as char);
                    *i += 1;
                }
                b']' => {
                    depth -= 1;
                    if depth == 0 {
                        *i += 1; // consume ']'
                        return Ok(out);
                    }
                    out.push(b as char);
                    *i += 1;
                }
                _ => {
                    out.push(b as char);
                    *i += 1;
                }
            }
        }
    }
    Ok(out) // unclosed bracket — let caller decide; not a security concern
}

/// Collects the content of a node pattern `(...)`, advancing `*i` past the closing `)`.
///
/// Handles strings and `{...}` property maps. Does NOT recurse on inner `(...)` because
/// Cypher node patterns do not nest parentheses in their label/property position.
fn collect_node_pattern(bytes: &[u8], i: &mut usize) -> Result<String, CypherError> {
    let mut inner = String::new();
    let mut brace_depth: usize = 0;
    let mut in_single = false;
    let mut in_double = false;

    while *i < bytes.len() {
        let b = bytes[*i];

        if in_single {
            if b == b'\\' && *i + 1 < bytes.len() {
                inner.push(b as char);
                inner.push(bytes[*i + 1] as char);
                *i += 2;
            } else {
                inner.push(b as char);
                *i += 1;
                if b == b'\'' {
                    in_single = false;
                }
            }
            continue;
        }

        if in_double {
            if b == b'\\' && *i + 1 < bytes.len() {
                inner.push(b as char);
                inner.push(bytes[*i + 1] as char);
                *i += 2;
            } else {
                inner.push(b as char);
                *i += 1;
                if b == b'"' {
                    in_double = false;
                }
            }
            continue;
        }

        match b {
            b'\'' => {
                in_single = true;
                inner.push(b as char);
                *i += 1;
            }
            b'"' => {
                in_double = true;
                inner.push(b as char);
                *i += 1;
            }
            b'{' => {
                brace_depth += 1;
                inner.push(b as char);
                *i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                inner.push(b as char);
                *i += 1;
            }
            b')' if brace_depth == 0 => {
                *i += 1; // consume closing ')'
                return Ok(inner);
            }
            _ => {
                inner.push(b as char);
                *i += 1;
            }
        }
    }
    Err(CypherError::UnclosedNodePattern)
}

/// Splices `:<label>` into the node-pattern interior immediately after the optional variable.
///
/// ```text
/// ""           → ":Label"
/// "n"          → "n:Label"
/// "n:Foo"      → "n:Label:Foo"
/// ":Foo"       → ":Label:Foo"
/// "n {p: $v}"  → "n:Label {p: $v}"
/// ```
fn splice_label(inner: &str, label: &str) -> String {
    let trimmed = inner.trim_start();
    let leading_ws = &inner[..inner.len() - trimmed.len()];

    // Find end of optional variable identifier
    let var_end = trimmed
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(trimmed.len());
    let var = &trimmed[..var_end];
    let rest = &trimmed[var_end..];

    format!("{leading_ws}{var}:{label}{rest}")
}

/// Rewrites `cypher` so that every node pattern in a MATCH / MERGE / CREATE / OPTIONAL MATCH
/// clause has `:<label>` injected after the optional variable and before any existing labels.
///
/// Variables bound by MATCH clauses are tracked. A bare variable reference in MERGE/CREATE
/// (no labels, no properties) that names an already-bound variable is left as-is so that
/// Neo4j 5.x `MERGE (a)-[:R]->(b)` patterns work when `a` and `b` were bound by a prior MATCH.
///
/// Also rejects queries that contain a bare semicolon outside a string or comment, as those
/// indicate multi-statement injection attempts.
///
/// # Errors
///
/// - [`CypherError::MultiStatement`] — semicolon found outside a string/comment.
/// - [`CypherError::UnclosedNodePattern`] — `(` in a path clause was never closed.
#[allow(clippy::too_many_lines)]
pub fn inject_tenant_label(cypher: &str, label: &str) -> Result<String, CypherError> {
    let bytes = cypher.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    let mut out = String::with_capacity(cypher.len() + label.len() * 4);
    let mut state = ScanState::Normal;

    // Whether the most recent non-whitespace content was a bare identifier character.
    // A `(` that follows an identifier is a function call, not a node pattern.
    let mut last_was_ident = false;

    // Whether the scanner is inside a MATCH/CREATE/MERGE path clause, and which kind.
    let mut in_path_clause = false;
    let mut path_clause_kind: Option<PathClauseKind> = None;

    // Variables declared by MATCH clauses — skip label injection for bare references to these
    // in MERGE/CREATE so Neo4j 5.x doesn't reject re-labelling already-bound variables.
    let mut bound_vars: std::collections::HashSet<String> = std::collections::HashSet::new();

    while i < len {
        let b = bytes[i];

        match state {
            ScanState::SingleQuote => {
                if b == b'\\' && i + 1 < len {
                    out.push(b as char);
                    out.push(bytes[i + 1] as char);
                    i += 2;
                } else {
                    out.push(b as char);
                    i += 1;
                    if b == b'\'' {
                        state = ScanState::Normal;
                        last_was_ident = false;
                    }
                }
            }

            ScanState::DoubleQuote => {
                if b == b'\\' && i + 1 < len {
                    out.push(b as char);
                    out.push(bytes[i + 1] as char);
                    i += 2;
                } else {
                    out.push(b as char);
                    i += 1;
                    if b == b'"' {
                        state = ScanState::Normal;
                        last_was_ident = false;
                    }
                }
            }

            ScanState::Backtick => {
                out.push(b as char);
                i += 1;
                if b == b'`' {
                    state = ScanState::Normal;
                    last_was_ident = true; // backtick-quoted identifier
                }
            }

            ScanState::LineComment => {
                out.push(b as char);
                i += 1;
                if b == b'\n' {
                    state = ScanState::Normal;
                }
            }

            ScanState::BlockComment => {
                out.push(b as char);
                i += 1;
                if b == b'*' && i < len && bytes[i] == b'/' {
                    out.push('/');
                    i += 1;
                    state = ScanState::Normal;
                }
            }

            ScanState::Normal => {
                // String starters
                if b == b'\'' {
                    state = ScanState::SingleQuote;
                    out.push(b as char);
                    i += 1;
                    last_was_ident = false;
                    continue;
                }
                if b == b'"' {
                    state = ScanState::DoubleQuote;
                    out.push(b as char);
                    i += 1;
                    last_was_ident = false;
                    continue;
                }
                if b == b'`' {
                    state = ScanState::Backtick;
                    out.push(b as char);
                    i += 1;
                    continue;
                }

                // Comments
                if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
                    state = ScanState::LineComment;
                    out.push('/');
                    out.push('/');
                    i += 2;
                    continue;
                }
                if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
                    state = ScanState::BlockComment;
                    out.push('/');
                    out.push('*');
                    i += 2;
                    continue;
                }

                // Reject multi-statement injection
                if b == b';' {
                    return Err(CypherError::MultiStatement);
                }

                // Relationship bracket — scan without keyword processing to prevent
                // relationship type names (e.g. `:WHERE`) from exiting path context.
                if b == b'[' {
                    i += 1;
                    let content = scan_bracket(bytes, &mut i)?;
                    out.push('[');
                    out.push_str(&content);
                    out.push(']');
                    last_was_ident = false;
                    continue;
                }

                // Identifier / keyword
                if is_ident_char(b) {
                    let word_start = i;
                    while i < len && is_ident_char(bytes[i]) {
                        i += 1;
                    }
                    let word = &cypher[word_start..i];
                    let upper = word.to_ascii_uppercase();

                    if let Some(kind) = path_start_kind(&upper) {
                        in_path_clause = true;
                        path_clause_kind = Some(kind);
                        // Allow `MATCH(n)` (no space) — keyword itself is not an identifier
                        // for the purpose of distinguishing function calls.
                        last_was_ident = false;
                    } else if is_path_end(&upper) {
                        in_path_clause = false;
                        path_clause_kind = None;
                        last_was_ident = true;
                    } else {
                        last_was_ident = true;
                    }
                    out.push_str(word);
                    continue;
                }

                // Node pattern: `(` in path context NOT preceded by a bare identifier
                if b == b'(' && in_path_clause && !last_was_ident {
                    i += 1;
                    let inner = collect_node_pattern(bytes, &mut i)?;

                    // Extract the variable name (if any) from the node pattern.
                    let trimmed = inner.trim_start();
                    let var_end = trimmed
                        .find(|c: char| !c.is_alphanumeric() && c != '_')
                        .unwrap_or(trimmed.len());
                    let var_name = &trimmed[..var_end];
                    let rest_after_var = trimmed[var_end..].trim_start();

                    // In MERGE/CREATE, a bare variable reference (no labels, no properties)
                    // that names an already-bound variable must NOT receive a label injection.
                    // Neo4j 5.x rejects MERGE/CREATE on already-declared variables with labels.
                    let is_bound_ref = path_clause_kind == Some(PathClauseKind::CreateOrMerge)
                        && !var_name.is_empty()
                        && rest_after_var.is_empty()
                        && bound_vars.contains(var_name);

                    if is_bound_ref {
                        // Emit the pattern unchanged.
                        out.push('(');
                        out.push_str(&inner);
                        out.push(')');
                    } else {
                        // Track variables declared by MATCH for future MERGE/CREATE checks.
                        if path_clause_kind == Some(PathClauseKind::Match) && !var_name.is_empty() {
                            bound_vars.insert(var_name.to_owned());
                        }
                        let patched = splice_label(&inner, label);
                        out.push('(');
                        out.push_str(&patched);
                        out.push(')');
                    }
                    last_was_ident = false;
                    continue;
                }

                // Whitespace does not change last_was_ident
                if !b.is_ascii_whitespace() {
                    last_was_ident = false;
                }
                out.push(b as char);
                i += 1;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LBL: &str = "Tenant_aabbcc112233445566778899";

    fn inj(s: &str) -> String {
        inject_tenant_label(s, LBL).expect("should not fail")
    }

    #[test]
    fn rejects_semicolon() {
        assert!(matches!(
            inject_tenant_label("MATCH (n) RETURN n; DROP ALL", LBL),
            Err(CypherError::MultiStatement)
        ));
    }

    #[test]
    fn injects_simple_match() {
        let out = inj("MATCH (n) RETURN n");
        assert!(out.contains(&format!("(n:{LBL})")), "got: {out}");
    }

    #[test]
    fn injects_with_existing_label() {
        let out = inj("MATCH (n:Person) RETURN n");
        assert!(out.contains(&format!("(n:{LBL}:Person)")), "got: {out}");
    }

    #[test]
    fn injects_anonymous_node() {
        let out = inj("MATCH ()-[r]->(n:Foo) RETURN n");
        assert!(out.contains(&format!("(:{LBL})")), "got: {out}");
        assert!(out.contains(&format!("(n:{LBL}:Foo)")), "got: {out}");
    }

    #[test]
    fn injects_create() {
        let out = inj("CREATE (n:Person {name: $name})");
        assert!(out.contains(&format!("(n:{LBL}:Person")), "got: {out}");
    }

    #[test]
    fn injects_merge() {
        let out = inj("MERGE (n:Foo {id: $id})");
        assert!(out.contains(&format!("(n:{LBL}:Foo")), "got: {out}");
    }

    #[test]
    fn injects_optional_match() {
        let out = inj("OPTIONAL MATCH (n:Bar) RETURN n");
        assert!(out.contains(&format!("(n:{LBL}:Bar)")), "got: {out}");
    }

    #[test]
    fn does_not_inject_function_call() {
        let out = inj("MATCH (n) RETURN count(n)");
        // count(n) must remain unchanged
        assert!(out.contains("count(n)"), "got: {out}");
    }

    #[test]
    fn does_not_inject_in_where_paren_expr() {
        // RETURN (expr) — last token before ( is RETURN keyword, which is a path-ender
        // so in_path_clause is false; no injection
        let out = inj("MATCH (n) WHERE (n.age > 18) RETURN n");
        // (n.age > 18) should NOT be injected
        assert!(!out.contains(&format!("({LBL}")), "got: {out}");
        // The MATCH node should be injected
        assert!(out.contains(&format!("(n:{LBL})")), "got: {out}");
    }

    #[test]
    fn semicolon_in_string_is_ok() {
        let out = inj("MATCH (n) WHERE n.name = ';' RETURN n");
        assert!(out.contains(&format!("(n:{LBL})")), "got: {out}");
    }

    #[test]
    fn match_keyword_no_space() {
        let out = inj("MATCH(n) RETURN n");
        assert!(out.contains(&format!("(n:{LBL})")), "got: {out}");
    }

    #[test]
    fn path_with_multiple_nodes() {
        let out = inj("MATCH (a)-[r]->(b) RETURN a, b");
        assert!(out.contains(&format!("(a:{LBL})")), "got: {out}");
        assert!(out.contains(&format!("(b:{LBL})")), "got: {out}");
    }

    #[test]
    fn relationship_type_name_where_does_not_break_path_clause() {
        // `:WHERE` as a relationship type name must not end path clause context
        let out = inj("MATCH (a)-[r:WHERE]->(b) RETURN a, b");
        assert!(out.contains(&format!("(a:{LBL})")), "got: {out}");
        assert!(out.contains(&format!("(b:{LBL})")), "got: {out}");
    }

    #[test]
    fn block_comment_does_not_trigger_keyword() {
        let out = inj("MATCH /* RETURN */ (n) RETURN n");
        assert!(out.contains(&format!("(n:{LBL})")), "got: {out}");
    }

    #[test]
    fn line_comment_does_not_trigger_keyword() {
        let out = inj("MATCH (n) // RETURN\n RETURN n");
        assert!(out.contains(&format!("(n:{LBL})")), "got: {out}");
    }

    #[test]
    fn keyword_in_string_does_not_affect_state() {
        let out = inj("MATCH (n) WHERE n.name = 'RETURN WHERE' RETURN n");
        assert!(out.contains(&format!("(n:{LBL})")), "got: {out}");
    }

    #[test]
    fn node_with_no_variable() {
        let out = inj("MATCH (:Person) RETURN 1");
        assert!(out.contains(&format!("(:{LBL}:Person)")), "got: {out}");
    }

    #[test]
    fn empty_node_pattern() {
        let out = inj("MATCH () RETURN 1");
        assert!(out.contains(&format!("(:{LBL})")), "got: {out}");
    }

    #[test]
    fn preserves_properties() {
        let out = inj("MATCH (n:Foo {name: $n, age: $a}) RETURN n");
        assert!(
            out.contains(&format!("(n:{LBL}:Foo {{name: $n, age: $a}})")),
            "got: {out}"
        );
    }

    #[test]
    fn semicolon_in_block_comment_rejected() {
        // Semicolon outside string/comment is rejected; inside block comment is allowed...
        // wait, actually the semicolon is INSIDE the block comment here, so should NOT be rejected.
        let out = inj("MATCH (n) /* ; */ RETURN n");
        assert!(out.contains(&format!("(n:{LBL})")), "got: {out}");
    }

    #[test]
    fn semicolon_after_comment_rejected() {
        let r = inject_tenant_label("MATCH (n) /* comment */ ; RETURN n", LBL);
        assert!(matches!(r, Err(CypherError::MultiStatement)));
    }
}
