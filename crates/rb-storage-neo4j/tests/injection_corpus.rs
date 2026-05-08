/// Runs the 100+ Cypher injection attack corpus.
///
/// `reject/` — multi-statement attacks that must return `CypherError::MultiStatement`.
/// `sanitize/` — cross-tenant access patterns that must be sanitized (tenant label injected).
use rb_storage_neo4j::inject_tenant_label;

const LABEL: &str = "Tenant_aabbcc112233445566778899";
const MIN_PER_DIR: usize = 50;

fn run_dir(dir: &str, should_reject: bool) {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path = format!("{manifest}/tests/fixtures/cypher_attacks/{dir}");
    let mut count = 0usize;

    let entries =
        std::fs::read_dir(&path).unwrap_or_else(|e| panic!("cannot read corpus dir {path}: {e}"));

    for entry in entries {
        let entry = entry.expect("dir entry");
        let fpath = entry.path();
        if fpath.extension().and_then(|e| e.to_str()) != Some("cypher") {
            continue;
        }
        let content = std::fs::read_to_string(&fpath)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", fpath.display()));
        let result = inject_tenant_label(&content, LABEL);

        if should_reject {
            assert!(
                result.is_err(),
                "corpus/{dir}/{} — expected CypherError, got Ok:\n{content}",
                fpath.file_name().unwrap().to_string_lossy(),
            );
        } else {
            let injected = result.unwrap_or_else(|e| {
                panic!(
                    "corpus/{dir}/{} — expected Ok, got Err({e}):\n{content}",
                    fpath.file_name().unwrap().to_string_lossy()
                )
            });
            assert!(
                injected.contains(LABEL),
                "corpus/{dir}/{} — tenant label not in output.\nInput:  {content}\nOutput: {injected}",
                fpath.file_name().unwrap().to_string_lossy(),
            );
        }
        count += 1;
    }

    assert!(
        count >= MIN_PER_DIR,
        "corpus/{dir} has only {count} fixtures — expected ≥{MIN_PER_DIR}"
    );
}

#[test]
fn corpus_reject() {
    run_dir("reject", true);
}

#[test]
fn corpus_sanitize() {
    run_dir("sanitize", false);
}
