// Pull in extractor directly from the binary source.
#[path = "../src/extractor.rs"]
mod extractor;
#[path = "../src/extractor_calls.rs"]
mod extractor_calls;

use extractor::{Relation, extract_relations};
use rb_schemas::RelationKind;
use std::path::Path;

const FIXTURE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/graph_inputs");

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(FIXTURE_DIR).join(name))
        .unwrap_or_else(|_| panic!("fixture {name} not found"))
}

fn kinds(rels: &[Relation]) -> Vec<RelationKind> {
    rels.iter().map(|r| r.kind).collect()
}

// ── impls.rs corpus ──────────────────────────────────────────────────────────

#[test]
fn impls_fixture_display_relation() {
    let sig = "impl fmt::Display for Point";
    let rels = extract_relations("fixtures_impls::Point", sig, &[], "");
    let impls: Vec<_> = rels
        .iter()
        .filter(|r| r.kind == RelationKind::Impls)
        .collect();
    assert!(!impls.is_empty(), "expected at least one IMPLS relation");
    assert!(
        impls.iter().any(|r| r.to_fqn.contains("Display")),
        "expected to_fqn to contain Display, got: {impls:?}"
    );
}

#[test]
fn impls_fixture_clone_relation() {
    let sig = "impl Clone for Point";
    let rels = extract_relations("fixtures_impls::Point", sig, &[], "");
    let impls: Vec<_> = rels
        .iter()
        .filter(|r| r.kind == RelationKind::Impls)
        .collect();
    assert!(!impls.is_empty(), "expected IMPLS relation for Clone");
    assert!(impls.iter().any(|r| r.to_fqn == "Clone"));
}

// ── derives.rs corpus ────────────────────────────────────────────────────────

#[test]
fn derives_fixture_config_struct_all_derives() {
    let body = fixture("derives.rs");
    // Simulate processing the Config struct item
    let config_body = "#[derive(Clone, Debug, PartialEq, Eq, Hash)]\npub struct Config {\n    pub host: String,\n    pub port: u16,\n}";
    let rels = extract_relations(
        "fixtures_derives::Config",
        "struct Config",
        &[],
        config_body,
    );
    let derives: Vec<_> = rels
        .iter()
        .filter(|r| r.kind == RelationKind::Derives)
        .collect();
    assert_eq!(derives.len(), 5, "Config has 5 derives; got: {derives:?}");
    let to_fqns: Vec<&str> = derives.iter().map(|r| r.to_fqn.as_str()).collect();
    assert!(to_fqns.contains(&"Clone"), "missing Clone derive");
    assert!(to_fqns.contains(&"Debug"), "missing Debug derive");
    assert!(to_fqns.contains(&"PartialEq"), "missing PartialEq derive");
    assert!(to_fqns.contains(&"Eq"), "missing Eq derive");
    assert!(to_fqns.contains(&"Hash"), "missing Hash derive");
    let _ = body; // fixture loaded above
}

#[test]
fn derives_fixture_enum_derives() {
    let body = "#[derive(Clone, Copy, Debug)]\npub enum Direction { North, South, East, West }";
    let rels = extract_relations("fixtures_derives::Direction", "enum Direction", &[], body);
    let derives: Vec<_> = rels
        .iter()
        .filter(|r| r.kind == RelationKind::Derives)
        .collect();
    assert_eq!(
        derives.len(),
        3,
        "Direction has 3 derives; got: {derives:?}"
    );
}

#[test]
fn derives_fixture_trait_supertrait_relations() {
    let sig = "trait Describable: Clone + std::fmt::Display";
    let rels = extract_relations("fixtures_derives::Describable", sig, &[], "");
    let extends: Vec<_> = rels
        .iter()
        .filter(|r| r.kind == RelationKind::ExtendsTrait)
        .collect();
    assert_eq!(
        extends.len(),
        2,
        "Describable has 2 supertraits; got: {extends:?}"
    );
    let to_fqns: Vec<&str> = extends.iter().map(|r| r.to_fqn.as_str()).collect();
    assert!(to_fqns.contains(&"Clone"), "missing Clone supertrait");
    // std::fmt::Display — first_ident_segment returns the full path for multi-segment
    assert!(
        to_fqns.iter().any(|n| n.contains("Display")),
        "missing Display supertrait; got: {to_fqns:?}"
    );
}

// ── bounds.rs corpus ─────────────────────────────────────────────────────────

#[test]
fn bounds_fixture_process_fn_bounded_by() {
    let bounds = ["T: Clone + Debug + Display".to_owned()];
    let rels = extract_relations(
        "fixtures_bounds::process",
        "fn process<T>(val: T) -> String",
        &bounds,
        "",
    );
    let bounded: Vec<_> = rels
        .iter()
        .filter(|r| r.kind == RelationKind::BoundedBy)
        .collect();
    assert_eq!(bounded.len(), 3, "process has 3 bounds; got: {bounded:?}");
    let to_fqns: Vec<&str> = bounded.iter().map(|r| r.to_fqn.as_str()).collect();
    assert!(to_fqns.contains(&"Clone"));
    assert!(to_fqns.contains(&"Debug"));
    assert!(to_fqns.contains(&"Display"));
    assert!(
        bounded
            .iter()
            .all(|r| r.from_fqn == "fixtures_bounds::process::T"),
        "from_fqn must include the item FQN; got: {bounded:?}"
    );
}

#[test]
fn bounds_fixture_container_struct_bounded() {
    let bounds = ["T: Clone + Send".to_owned()];
    let rels = extract_relations(
        "fixtures_bounds::Container",
        "struct Container<T: Clone + Send>",
        &bounds,
        "",
    );
    let bounded: Vec<_> = rels
        .iter()
        .filter(|r| r.kind == RelationKind::BoundedBy)
        .collect();
    assert_eq!(
        bounded.len(),
        2,
        "Container bounds: Clone + Send; got: {bounded:?}"
    );
}

// ── cross-cutting ─────────────────────────────────────────────────────────────

#[test]
fn empty_item_no_relations() {
    let rels = extract_relations("src_lib::empty", "fn empty()", &[], "");
    assert!(
        rels.is_empty(),
        "plain fn with no bounds/impl should emit no relations"
    );
}

#[test]
fn relation_kinds_round_trip_as_i32() {
    for kind in [
        RelationKind::Impls,
        RelationKind::BoundedBy,
        RelationKind::ExtendsTrait,
        RelationKind::Derives,
    ] {
        let as_i32 = kind as i32;
        let back = RelationKind::try_from(as_i32).expect("must round-trip");
        assert_eq!(kind, back);
    }
}

#[test]
fn all_kinds_present_in_fixture_corpus() {
    let mut seen = std::collections::HashSet::new();

    // IMPLS
    let rels = extract_relations("p::Point", "impl Display for Point", &[], "");
    for r in &rels {
        seen.insert(r.kind);
    }

    // EXTENDS_TRAIT
    let rels = extract_relations("p::Animal", "trait Animal: Clone", &[], "");
    for r in &rels {
        seen.insert(r.kind);
    }

    // BOUNDED_BY
    let rels = extract_relations("p::f", "fn f<T>()", &["T: Clone".to_owned()], "");
    for r in &rels {
        seen.insert(r.kind);
    }

    // DERIVES
    let rels = extract_relations("p::S", "struct S", &[], "#[derive(Clone)] pub struct S {}");
    for r in &rels {
        seen.insert(r.kind);
    }

    assert!(seen.contains(&RelationKind::Impls), "IMPLS not seen");
    assert!(
        seen.contains(&RelationKind::ExtendsTrait),
        "EXTENDS_TRAIT not seen"
    );
    assert!(
        seen.contains(&RelationKind::BoundedBy),
        "BOUNDED_BY not seen"
    );
    assert!(seen.contains(&RelationKind::Derives), "DERIVES not seen");
}

#[test]
fn kinds_helper_works() {
    let rels = extract_relations(
        "m::Wrapper",
        "impl Clone for Wrapper",
        &["T: Send".to_owned()],
        "",
    );
    let k = kinds(&rels);
    assert!(k.contains(&RelationKind::Impls));
    assert!(k.contains(&RelationKind::BoundedBy));
}
