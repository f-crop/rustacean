use super::*;

#[test]
fn redact_token_replaces_credential() {
    let url = "https://x-access-token:ghp_secret123@github.com/owner/repo.git";
    let redacted = redact_token(url);
    assert!(
        !redacted.contains("ghp_secret123"),
        "token must be redacted"
    );
    assert!(redacted.contains("<token>"), "placeholder must be present");
    assert!(redacted.contains("github.com/owner/repo.git"));
}

#[test]
fn redact_token_leaves_plain_url_unchanged() {
    let url = "https://github.com/owner/repo.git";
    let redacted = redact_token(url);
    assert_eq!(redacted, url);
}

#[test]
fn collect_rs_files_finds_rust_files() {
    let dir = tempfile::tempdir().unwrap();
    let rs_path = dir.path().join("lib.rs");
    let txt_path = dir.path().join("readme.txt");
    std::fs::write(&rs_path, b"fn main() {}").unwrap();
    std::fs::write(&txt_path, b"not rust").unwrap();

    let files = collect_rs_files(dir.path()).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].relative_path, "lib.rs");
}

#[test]
fn collect_rs_files_computes_sha256() {
    let dir = tempfile::tempdir().unwrap();
    let content = b"fn foo() {}";
    std::fs::write(dir.path().join("foo.rs"), content).unwrap();

    let files = collect_rs_files(dir.path()).unwrap();
    assert_eq!(files.len(), 1);

    let expected = hex::encode(Sha256::digest(content));
    assert_eq!(files[0].sha256, expected);
    assert_eq!(files[0].size, content.len() as u64);
}

#[test]
fn collect_rs_files_walks_subdirectories() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), b"fn main() {}").unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), b"pub fn foo() {}").unwrap();

    let mut files = collect_rs_files(dir.path()).unwrap();
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    assert_eq!(files.len(), 2);
    assert!(files.iter().any(|f| f.relative_path.contains("main.rs")));
    assert!(files.iter().any(|f| f.relative_path.contains("lib.rs")));
}

#[test]
fn create_tar_zst_produces_non_empty_archive() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("test.rs"), b"fn main() {}").unwrap();

    let bytes = create_tar_zst(dir.path()).unwrap();
    assert!(!bytes.is_empty(), "tar.zst must be non-empty");
    // Zstd magic bytes: 0xFD2FB528 in little-endian
    assert_eq!(&bytes[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
}

#[test]
fn create_tar_zst_is_decompressible() {
    let dir = tempfile::tempdir().unwrap();
    let content = b"pub struct Foo;";
    std::fs::write(dir.path().join("foo.rs"), content).unwrap();

    let compressed = create_tar_zst(dir.path()).unwrap();

    // Decompress and verify a tar archive is inside.
    let decoded = zstd::decode_all(std::io::Cursor::new(&compressed)).unwrap();
    assert!(!decoded.is_empty());
}

#[test]
fn topic_constants_are_distinct() {
    let topics = [
        TOPIC_CLONE_COMMANDS,
        TOPIC_SOURCE_FILES,
        TOPIC_EXPAND_COMMANDS,
        TOPIC_PROJECTOR_EVENTS,
    ];
    let unique: std::collections::HashSet<_> = topics.iter().collect();
    assert_eq!(
        unique.len(),
        topics.len(),
        "all topic constants must be unique"
    );
}

#[test]
fn inline_threshold_is_512kib() {
    assert_eq!(INLINE_MAX_BYTES, 512 * 1024);
}

#[test]
fn clone_timeout_is_five_minutes() {
    assert_eq!(CLONE_TIMEOUT_SECS, 300);
}

#[test]
fn processing_status_value_is_2() {
    assert_eq!(IngestStatus::Processing as i32, 2);
}

#[test]
fn clone_stage_seq_is_1() {
    assert_eq!(IngestStage::Clone as i32, 1);
}
