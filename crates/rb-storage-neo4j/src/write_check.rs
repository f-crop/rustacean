/// Returns `true` when `cypher` contains any write-operator keyword
/// (CREATE, MERGE, SET, DELETE, DETACH, REMOVE) outside strings or comments.
///
/// Used by `POST /v1/graph/query` when `read_only = true` to pre-flight the
/// query before sending it to Neo4j.
pub fn has_write_operators(cypher: &str) -> bool {
    let bytes = cypher.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    #[derive(PartialEq)]
    enum State {
        Normal,
        SingleQuote,
        DoubleQuote,
        Backtick,
        LineComment,
        BlockComment,
    }

    let mut state = State::Normal;

    while i < len {
        let b = bytes[i];
        match state {
            State::SingleQuote => {
                if b == b'\\' && i + 1 < len {
                    i += 2;
                } else {
                    i += 1;
                    if b == b'\'' {
                        state = State::Normal;
                    }
                }
            }
            State::DoubleQuote => {
                if b == b'\\' && i + 1 < len {
                    i += 2;
                } else {
                    i += 1;
                    if b == b'"' {
                        state = State::Normal;
                    }
                }
            }
            State::Backtick => {
                i += 1;
                if b == b'`' {
                    state = State::Normal;
                }
            }
            State::LineComment => {
                i += 1;
                if b == b'\n' {
                    state = State::Normal;
                }
            }
            State::BlockComment => {
                if b == b'*' && i + 1 < len && bytes[i + 1] == b'/' {
                    i += 2;
                    state = State::Normal;
                } else {
                    i += 1;
                }
            }
            State::Normal => match b {
                b'\'' => {
                    state = State::SingleQuote;
                    i += 1;
                }
                b'"' => {
                    state = State::DoubleQuote;
                    i += 1;
                }
                b'`' => {
                    state = State::Backtick;
                    i += 1;
                }
                b'/' if i + 1 < len && bytes[i + 1] == b'/' => {
                    state = State::LineComment;
                    i += 2;
                }
                b'/' if i + 1 < len && bytes[i + 1] == b'*' => {
                    state = State::BlockComment;
                    i += 2;
                }
                b if b.is_ascii_alphabetic() || b == b'_' => {
                    let start = i;
                    while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                        i += 1;
                    }
                    if is_write_keyword(&cypher[start..i]) {
                        return true;
                    }
                }
                _ => {
                    i += 1;
                }
            },
        }
    }
    false
}

fn eq_ci(a: &str, upper: &str) -> bool {
    a.len() == upper.len() && a.bytes().zip(upper.bytes()).all(|(x, y)| x.to_ascii_uppercase() == y)
}

fn is_write_keyword(word: &str) -> bool {
    match word.len() {
        3 => eq_ci(word, "SET"),
        5 => eq_ci(word, "MERGE"),
        6 => {
            eq_ci(word, "CREATE")
                || eq_ci(word, "DELETE")
                || eq_ci(word, "DETACH")
                || eq_ci(word, "REMOVE")
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_create() {
        assert!(has_write_operators("CREATE (n:Foo)"));
    }

    #[test]
    fn detects_merge() {
        assert!(has_write_operators("MATCH (n) MERGE (n)-[:R]->(m:Bar)"));
    }

    #[test]
    fn detects_set() {
        assert!(has_write_operators("MATCH (n) SET n.name = $v"));
    }

    #[test]
    fn detects_delete() {
        assert!(has_write_operators("MATCH (n) DELETE n"));
    }

    #[test]
    fn detects_detach() {
        assert!(has_write_operators("MATCH (n) DETACH DELETE n"));
    }

    #[test]
    fn detects_remove() {
        assert!(has_write_operators("MATCH (n) REMOVE n.prop"));
    }

    #[test]
    fn pure_read_is_ok() {
        assert!(!has_write_operators("MATCH (n:Foo) RETURN n"));
    }

    #[test]
    fn keyword_in_single_quote_ignored() {
        assert!(!has_write_operators("MATCH (n) WHERE n.x = 'CREATE' RETURN n"));
    }

    #[test]
    fn keyword_in_double_quote_ignored() {
        assert!(!has_write_operators("MATCH (n) WHERE n.x = \"DELETE\" RETURN n"));
    }

    #[test]
    fn keyword_in_line_comment_ignored() {
        assert!(!has_write_operators("MATCH (n) // CREATE node\n RETURN n"));
    }

    #[test]
    fn keyword_in_block_comment_ignored() {
        assert!(!has_write_operators("MATCH (n) /* MERGE */ RETURN n"));
    }

    #[test]
    fn escaped_quote_does_not_exit_string() {
        // "SET" inside a string with escaped quote — must not trigger
        assert!(!has_write_operators("MATCH (n) WHERE n.x = 'foo\\'s SET' RETURN n"));
    }

    #[test]
    fn lowercase_create_detected() {
        assert!(has_write_operators("create (n:Foo)"));
    }

    #[test]
    fn mixed_case_set_detected() {
        assert!(has_write_operators("MATCH (n) Set n.x = 1"));
    }

    #[test]
    fn property_name_containing_keyword_not_detected() {
        // "created_at" is NOT a write keyword — suffix/prefix must not match
        assert!(!has_write_operators("MATCH (n) RETURN n.created_at"));
    }

    #[test]
    fn unwind_with_with_is_ok() {
        assert!(!has_write_operators(
            "MATCH (n) WITH n RETURN n.name LIMIT 10"
        ));
    }
}
