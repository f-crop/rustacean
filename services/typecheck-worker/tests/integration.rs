// Pull in type_extractor directly from the binary source so integration tests
// can access extract_typed_items without requiring a lib target.
#[path = "../src/type_extractor.rs"]
mod type_extractor;

use std::path::Path;

const FIXTURE_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/typecheck_inputs"
);

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(FIXTURE_DIR).join(name))
        .unwrap_or_else(|_| panic!("fixture {name} not found"))
}

// ── simple.rs corpus ────────────────────────────────────────────────────────

#[test]
fn simple_fixture_extracts_all_item_kinds() {
    let src = fixture("simple.rs");
    let items = type_extractor::extract_typed_items(&src);
    assert!(!items.is_empty(), "simple.rs must produce items");

    let names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"Config"), "expected Config");
    assert!(names.contains(&"connect"), "expected connect");
    assert!(names.contains(&"DEFAULT_PORT"), "expected DEFAULT_PORT");
    assert!(names.contains(&"Transport"), "expected Transport");
    assert!(names.contains(&"GREETING"), "expected GREETING");
}

#[test]
fn simple_fixture_fn_has_signature() {
    let src = fixture("simple.rs");
    let items = type_extractor::extract_typed_items(&src);
    let connect = items
        .iter()
        .find(|i| i.name == "connect")
        .expect("connect not found");
    assert!(
        connect.resolved_type_signature.contains("fn connect"),
        "expected fn connect in signature, got: {}",
        connect.resolved_type_signature
    );
    assert!(
        connect.resolved_type_signature.contains("Config"),
        "signature must include Config param"
    );
    assert!(
        connect.resolved_type_signature.contains("String"),
        "signature must include String return"
    );
}

#[test]
fn simple_fixture_struct_has_signature() {
    let src = fixture("simple.rs");
    let items = type_extractor::extract_typed_items(&src);
    let cfg = items
        .iter()
        .find(|i| i.name == "Config")
        .expect("Config not found");
    assert_eq!(cfg.resolved_type_signature, "struct Config");
    assert!(cfg.trait_bounds.is_empty());
}

#[test]
fn simple_fixture_line_numbers_are_positive() {
    let src = fixture("simple.rs");
    let items = type_extractor::extract_typed_items(&src);
    for item in &items {
        assert!(
            item.line_start > 0,
            "line_start must be ≥1 for {}",
            item.name
        );
        assert!(
            item.line_end >= item.line_start,
            "line_end must be ≥ line_start for {}",
            item.name
        );
    }
}

// ── complex.rs corpus ────────────────────────────────────────────────────────

#[test]
fn complex_fixture_generic_fn_extracts_bounds() {
    let src = fixture("complex.rs");
    let items = type_extractor::extract_typed_items(&src);

    let id_fn = items
        .iter()
        .find(|i| i.name == "id")
        .expect("id fn not found");
    assert!(
        id_fn.resolved_type_signature.contains("fn id"),
        "expected fn id signature"
    );
    assert!(
        id_fn.trait_bounds.iter().any(|b| b.contains("Display")),
        "expected T: Display bound, got: {:?}",
        id_fn.trait_bounds
    );
}

#[test]
fn complex_fixture_trait_with_supertraits() {
    let src = fixture("complex.rs");
    let items = type_extractor::extract_typed_items(&src);

    let trait_item = items
        .iter()
        .find(|i| i.name == "Describable")
        .expect("Describable trait not found");
    assert!(
        trait_item.resolved_type_signature.contains("Display"),
        "Describable signature must include Display supertrait: {}",
        trait_item.resolved_type_signature
    );
    assert!(
        trait_item.resolved_type_signature.contains("Clone"),
        "Describable signature must include Clone supertrait"
    );
}

#[test]
fn complex_fixture_dyn_trait_fn_found() {
    let src = fixture("complex.rs");
    let items = type_extractor::extract_typed_items(&src);

    let make_holder = items
        .iter()
        .find(|i| i.name == "make_holder")
        .expect("make_holder not found");
    assert!(
        make_holder.resolved_type_signature.contains("dyn"),
        "make_holder signature must reference dyn trait: {}",
        make_holder.resolved_type_signature
    );
}

#[test]
fn complex_fixture_container_impl_blocks_for_three_types() {
    let src = fixture("complex.rs");
    let items = type_extractor::extract_typed_items(&src);

    let impl_names: Vec<&str> = items
        .iter()
        .filter(|i| i.name.starts_with("impl "))
        .map(|i| i.name.as_str())
        .collect();
    assert!(
        impl_names.iter().any(|n| n.contains("Container")),
        "expected impl blocks for Container, got: {impl_names:?}"
    );
    // Three concrete Container impls: i32, String, f64
    let container_impls: Vec<&&str> = impl_names
        .iter()
        .filter(|n| n.contains("Container"))
        .collect();
    assert!(
        container_impls.len() >= 3,
        "expected ≥3 Container impls, got: {container_impls:?}"
    );
}

#[test]
fn complex_fixture_where_clause_bounds() {
    let src = fixture("complex.rs");
    let items = type_extractor::extract_typed_items(&src);

    let process_fn = items
        .iter()
        .find(|i| i.name == "process")
        .expect("process fn not found");
    assert!(
        process_fn.trait_bounds.iter().any(|b| b.contains("Clone")),
        "process bounds must include Clone: {:?}",
        process_fn.trait_bounds
    );
    assert!(
        process_fn.trait_bounds.iter().any(|b| b.contains("Send")),
        "process bounds must include Send: {:?}",
        process_fn.trait_bounds
    );
}

#[test]
fn complex_fixture_type_alias_extracted() {
    let src = fixture("complex.rs");
    let items = type_extractor::extract_typed_items(&src);

    let alias = items
        .iter()
        .find(|i| i.name == "Result")
        .expect("Result type alias not found");
    assert!(alias.resolved_type_signature.starts_with("type Result"));
}
