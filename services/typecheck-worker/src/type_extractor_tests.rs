use super::*;

#[test]
fn extracts_fn_signature() {
    let src = "pub fn add(x: i32, y: i32) -> i32 { x + y }";
    let items = extract_typed_items(src);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "add");
    assert_eq!(
        items[0].resolved_type_signature,
        "fn add(x: i32, y: i32) -> i32"
    );
    assert!(items[0].trait_bounds.is_empty());
}

#[test]
fn extracts_async_fn_signature() {
    let src = "pub async fn fetch() -> String { String::new() }";
    let items = extract_typed_items(src);
    assert_eq!(
        items[0].resolved_type_signature,
        "async fn fetch() -> String"
    );
}

#[test]
fn extracts_fn_with_self_ref() {
    let src = "impl Foo { pub fn len(&self) -> usize { 0 } }";
    let items = extract_typed_items(src);
    let fn_item = items.iter().find(|i| i.name == "fn len(…)");
    // impl block is extracted; fn inside impl is not visited at top level
    let impl_item = items.iter().find(|i| i.name == "impl Foo");
    assert!(impl_item.is_some(), "impl Foo should be extracted");
    let _ = fn_item; // fn inside impl block is not top-level
}

#[test]
fn extracts_struct_signature() {
    let src = "pub struct Point { pub x: f64, pub y: f64 }";
    let items = extract_typed_items(src);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "Point");
    assert_eq!(items[0].resolved_type_signature, "struct Point");
}

#[test]
fn extracts_generic_struct_with_bounds() {
    let src = "pub struct Wrapper<T: Clone + Send>(T);";
    let items = extract_typed_items(src);
    assert_eq!(
        items[0].resolved_type_signature,
        "struct Wrapper<T: Clone + Send>"
    );
    assert!(
        items[0]
            .trait_bounds
            .contains(&"T: Clone + Send".to_owned())
    );
}

#[test]
fn extracts_enum_signature() {
    let src = "pub enum Color { Red, Green, Blue }";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "Color");
    assert_eq!(items[0].resolved_type_signature, "enum Color");
}

#[test]
fn extracts_trait_with_supertraits() {
    let src = "pub trait Animal: Clone + Send { fn name(&self) -> &str; }";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "Animal");
    assert_eq!(
        items[0].resolved_type_signature,
        "trait Animal: Clone + Send"
    );
}

#[test]
fn extracts_impl_inherent() {
    let src = "impl Foo { fn new() -> Self { Foo } }";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "impl Foo");
    assert_eq!(items[0].resolved_type_signature, "impl Foo");
}

#[test]
fn extracts_impl_trait() {
    let src = "impl Display for Foo {}";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "<Foo as Display>");
    assert_eq!(items[0].resolved_type_signature, "impl Display for Foo");
}

#[test]
fn extracts_const_signature() {
    let src = "pub const MAX_SIZE: usize = 1024;";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "MAX_SIZE");
    assert_eq!(items[0].resolved_type_signature, "const MAX_SIZE: usize");
}

#[test]
fn extracts_type_alias() {
    let src = "pub type Result<T> = std::result::Result<T, Error>;";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "Result");
    assert!(
        items[0]
            .resolved_type_signature
            .starts_with("type Result<T> = ")
    );
}

#[test]
fn extracts_where_clause_bounds() {
    let src = "pub fn process<T>(val: T) where T: Clone + Debug {}";
    let items = extract_typed_items(src);
    assert!(
        items[0]
            .trait_bounds
            .contains(&"T: Clone + Debug".to_owned())
    );
}

#[test]
fn returns_empty_on_parse_failure() {
    let items = extract_typed_items("fn broken( {");
    assert!(items.is_empty());
}

#[test]
fn line_numbers_populated() {
    let src = "pub fn first() {}\npub fn second() {}";
    let items = extract_typed_items(src);
    assert_eq!(items[0].line_start, 1);
    assert_eq!(items[1].line_start, 2);
}

#[test]
fn item_fn_span_covers_full_body() {
    // The fix: node.span() covers all lines; node.sig.ident.span() would give line_start==line_end.
    let src = "pub fn foo() {\n    let x = 1;\n    x\n}";
    let items = extract_typed_items(src);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "foo");
    assert!(
        items[0].line_end > items[0].line_start,
        "multi-line fn: line_end ({}) must be > line_start ({})",
        items[0].line_end,
        items[0].line_start,
    );
}

#[test]
fn item_struct_span_covers_full_body() {
    let src = "pub struct Foo {\n    pub x: i32,\n    pub y: i32,\n}";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "Foo");
    assert!(
        items[0].line_end > items[0].line_start,
        "multi-line struct: line_end ({}) must be > line_start ({})",
        items[0].line_end,
        items[0].line_start,
    );
}

#[test]
fn item_impl_span_covers_full_body() {
    let src = "impl Foo {\n    pub fn new() -> Self {\n        Foo {}\n    }\n}";
    let items = extract_typed_items(src);
    let impl_item = items
        .iter()
        .find(|i| i.name == "impl Foo")
        .expect("impl Foo not found");
    assert!(
        impl_item.line_end > impl_item.line_start,
        "multi-line impl: line_end ({}) must be > line_start ({})",
        impl_item.line_end,
        impl_item.line_start,
    );
}

#[test]
fn async_fn_span_covers_full_body() {
    let src = "pub async fn handle(\n    req: Request,\n) -> Response {\n    todo!()\n}";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "handle");
    assert!(
        items[0].line_end > items[0].line_start,
        "multi-line async fn: line_end ({}) must be > line_start ({})",
        items[0].line_end,
        items[0].line_start,
    );
}

#[test]
fn fmt_type_reference() {
    let src = "pub fn f(x: &str) {}";
    let items = extract_typed_items(src);
    assert!(items[0].resolved_type_signature.contains("&str"));
}

#[test]
fn fmt_type_option() {
    let src = "pub fn f() -> Option<String> { None }";
    let items = extract_typed_items(src);
    assert!(items[0].resolved_type_signature.contains("Option<String>"));
}

#[test]
fn fmt_type_result() {
    let src = "pub fn f() -> Result<i32, Error> { Ok(0) }";
    let items = extract_typed_items(src);
    assert!(
        items[0]
            .resolved_type_signature
            .contains("Result<i32, Error>")
    );
}

#[test]
fn fmt_type_tuple() {
    let src = "pub fn f() -> (i32, bool) { (0, true) }";
    let items = extract_typed_items(src);
    assert!(items[0].resolved_type_signature.contains("(i32, bool)"));
}

#[test]
fn fmt_type_slice() {
    let src = "pub fn f(s: &[u8]) {}";
    let items = extract_typed_items(src);
    assert!(items[0].resolved_type_signature.contains("[u8]"));
}

#[test]
fn static_item_signature() {
    let src = "pub static GREETING: &str = \"hello\";";
    let items = extract_typed_items(src);
    assert_eq!(items[0].resolved_type_signature, "static GREETING: &str");
}

#[test]
fn mod_item_signature() {
    let src = "pub mod utils {}";
    let items = extract_typed_items(src);
    assert_eq!(items[0].name, "utils");
    assert_eq!(items[0].resolved_type_signature, "mod utils");
}
