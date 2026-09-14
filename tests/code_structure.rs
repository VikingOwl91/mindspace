use mindspace::core::{
    sha256_hex, CodeContextDraft, CodeInputDraft, CodeLanguage, CodeObservation,
};
use mindspace::ingest::code;
use mindspace::storage::Store;
use mindspace::SqliteStore;

fn input(path: &str, source: &str) -> CodeInputDraft {
    CodeInputDraft {
        language: CodeLanguage::Rust,
        content_hash: sha256_hex(source.as_bytes()),
        content: source.to_string(),
        repository_id: None,
        blob_oid: None,
        contexts: vec![CodeContextDraft {
            path: path.to_string(),
            commit_oid: None,
        }],
        derivation: code::parse(CodeLanguage::Rust, source),
    }
}

#[test]
fn derivation_is_idempotent_and_parser_versions_coexist() {
    let store = SqliteStore::open_in_memory().unwrap();
    let source = "mod a { struct A; impl A { fn load() {} } }";
    let first = store
        .record_code_observation(CodeObservation {
            inputs: vec![input("/tmp/a.rs", source)],
        })
        .unwrap();
    assert_eq!((first.parsed, first.reused), (1, 0));
    let repeat = store
        .record_code_observation(CodeObservation {
            inputs: vec![input("/tmp/a.rs", source)],
        })
        .unwrap();
    assert_eq!((repeat.parsed, repeat.reused), (0, 1));
    let mut changed = input("/tmp/a.rs", source);
    changed.derivation.identity.extractor_version = "2".into();
    assert_eq!(
        store
            .record_code_observation(CodeObservation {
                inputs: vec![changed]
            })
            .unwrap()
            .parsed,
        1
    );
    let structures = store.code_structure("/tmp/a.rs").unwrap();
    assert_eq!(structures.len(), 2);
    assert!(structures[0]
        .entities
        .iter()
        .any(|e| e.qualified_name.as_deref() == Some("a::A::load")));
}

#[test]
fn malformed_input_is_retained_without_memories() {
    let store = SqliteStore::open_in_memory().unwrap();
    let report = store
        .record_code_observation(CodeObservation {
            inputs: vec![input("/tmp/bad.rs", "fn broken(")],
        })
        .unwrap();
    assert_eq!(report.parse_errors, 1);
    assert_eq!(
        store.code_structure("/tmp/bad.rs").unwrap()[0]
            .status
            .as_str(),
        "recovered"
    );
}
