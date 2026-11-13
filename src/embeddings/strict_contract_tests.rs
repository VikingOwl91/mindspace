use super::*;
fn current() -> StrictContract {
    StrictContract::current("nomic-embed-text-v1.5").unwrap()
}
#[test]
fn assets_versions_and_budget_match_actual_contract() {
    let c = current();
    c.validate_current().unwrap();
    assert_eq!(
        c.budget_identity,
        "e735b259ffc71ab9bccdbafa1b8f94d9d4e051d4fb34d7d82a6c2a819d9ba841"
    );
    assert_eq!(c.weights_bytes, 546_938_168);
    assert_eq!(c.tokenizer_bytes, 711_396);
    assert_eq!(c.candle_core_version, "0.11.1");
    assert_eq!(c.candle_nn_version, "0.11.1");
    assert_eq!(c.candle_transformers_version, "0.11.1");
    assert_eq!(c.tokenizers_version, "0.23.2");
    assert_eq!(
        c.budget_identity,
        budget_identity(manifest::default_model()).unwrap()
    );
    assert_ne!(
        c.key(),
        manifest::derivation_from_manifest(manifest::default_model()).derivation_key()
    );
    let restored: StrictContract = serde_json::from_slice(&c.canonical()).unwrap();
    assert_eq!(restored, c);
}
#[test]
fn historical_contract_is_readable_without_current_build_or_registry_equality() {
    let mut c = current();
    c.model_revision = "historical-revision".into();
    c.candle_core_version = "historical-runtime".into();
    c.lock_sha256 = sha256_hex(b"old lock");
    c.build_fingerprint = c.expected_build_fingerprint();
    c.validate_stored().unwrap();
    assert!(c.validate_current().is_err());
}
#[test]
fn altered_assets_prefix_config_and_runtime_change_keys() {
    let c = current();
    for i in 0..6 {
        let mut altered = c.clone();
        match i {
            0 => altered.weights_sha256 = sha256_hex(b"other weights"),
            1 => altered.tokenizer_sha256 = sha256_hex(b"other tokenizer"),
            2 => altered.prefix_document = "different: ".into(),
            3 => altered.prefix_query = "different-query: ".into(),
            4 => altered.dimension = 1024,
            _ => altered.pooling = "other".into(),
        };
        assert_ne!(c.key(), altered.key());
        assert!(altered.validate_current().is_err());
    }
    let mut c = c;
    c.config_json = c.config_json.replace("30528", "30529");
    assert!(c.validate_stored().is_err());
}
#[test]
fn finite_normalized_vector_storage_is_strict() {
    let c = current();
    let mut unit = vec![0.0; 768];
    unit[0] = 1.0;
    let bytes = crate::core::encode_f32_le(&unit).unwrap();
    validate_blob(&c, &bytes).unwrap();
    for vector in [
        vec![0.0; 768],
        vec![f32::NAN; 768],
        vec![f32::INFINITY; 768],
        vec![1.0; 768],
        vec![1.0; 767],
    ] {
        assert!(validate_blob(
            &c,
            &crate::core::encode_f32_le(&vector).unwrap_or_else(|_| vec![0; 3072])
        )
        .is_err());
    }
    let mut corrupt = bytes.clone();
    corrupt[0..4].copy_from_slice(&f32::NAN.to_le_bytes());
    assert!(validate_blob(&c, &corrupt).is_err());
    let mut v = StrictVector {
        token_count: 2048,
        encoding_digest: sha256_hex(b"encoding"),
        vector_hash: sha256_hex(&bytes),
        embedding: bytes,
    };
    v.validate(&c).unwrap();
    v.token_count = 2049;
    assert!(v.validate(&c).is_err());
    v.token_count = 1;
    v.vector_hash = sha256_hex(b"wrong");
    assert!(v.validate(&c).is_err());
}
#[test]
fn encoding_binds_raw_text_even_when_normalized_ids_match() {
    let c = current();
    let ids = [101, 12, 102];
    let mask = [1, 1, 1];
    let a = encoding_digest(&c, &sha256_hex(b"abc"), &ids, &mask).unwrap();
    let b = encoding_digest(&c, &sha256_hex("a\u{200b}bc".as_bytes()), &ids, &mask).unwrap();
    assert_ne!(a, b);
    assert_ne!(
        a,
        encoding_digest(&c, &sha256_hex(b"abc"), &[101, 13, 102], &mask).unwrap()
    );
    assert_ne!(
        a,
        encoding_digest(&c, &sha256_hex(b"abc"), &ids, &[1, 0, 1]).unwrap()
    );
    assert!(encoding_digest(&c, &sha256_hex(b"abc"), &ids, &[1]).is_err());
}
