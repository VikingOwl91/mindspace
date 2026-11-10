use super::*;
use std::time::Duration;
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(120)
}
fn contract() -> StrictContract {
    StrictContract::current("nomic-embed-text-v1.5").unwrap()
}
fn budget() -> TokenBudget {
    TokenBudget::from_file(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
        manifest::default_model(),
    )
    .unwrap()
}
#[test]
fn exact_ceiling_and_overflow_preflight_before_weights() {
    let c = contract();
    let b = budget();
    let exact = "hello ".repeat(2042);
    let over = "hello ".repeat(2043);
    assert_eq!(b.count_document(&exact).unwrap(), 2048);
    assert_eq!(b.count_document(&over).unwrap(), 2049);
    let hash = sha256_hex(exact.as_bytes());
    let inputs = [StrictInput {
        content: &exact,
        expected_tokens: 2048,
        payload_hash: &hash,
    }];
    assert_eq!(preflight(&c, &b, &inputs, deadline()).unwrap().len(), 1);
    let hash = sha256_hex(over.as_bytes());
    let inputs = [StrictInput {
        content: &over,
        expected_tokens: 2049,
        payload_hash: &hash,
    }];
    assert!(preflight(&c, &b, &inputs, deadline())
        .unwrap_err()
        .to_string()
        .contains("2049"));
}
#[test]
fn preflight_rejects_false_counts_hashes_contract_and_deadline() {
    let c = contract();
    let b = budget();
    let hash = sha256_hex(b"hello");
    let input = [StrictInput {
        content: "hello",
        expected_tokens: 1,
        payload_hash: &hash,
    }];
    assert!(preflight(&c, &b, &input, deadline()).is_err());
    let input = [StrictInput {
        content: "hello",
        expected_tokens: 7,
        payload_hash: &sha256_hex(b"other"),
    }];
    assert!(preflight(&c, &b, &input, deadline()).is_err());
    let mut wrong = c.clone();
    wrong.budget_identity = sha256_hex(b"different");
    assert!(preflight(&wrong, &b, &[], deadline()).is_err());
    assert!(preflight(&c, &b, &input, Instant::now() - Duration::from_secs(1)).is_err());
}
#[test]
fn actual_unicode_and_controls_keep_raw_payload_binding() {
    let c = contract();
    let b = budget();
    for text in [
        "",
        " \r\n",
        "漢字かな한글",
        "a\u{200b}b\u{202e}c\u{2069}",
        "🦉👨‍👩‍👧‍👦e\u{301}",
        "abc",
    ] {
        let hash = sha256_hex(text.as_bytes());
        let tokens = b.count_document(text).unwrap() as u32;
        let input = StrictInput {
            content: text,
            expected_tokens: tokens,
            payload_hash: &hash,
        };
        let digests = preflight(&c, &b, std::slice::from_ref(&input), deadline()).unwrap();
        let e = checked_encoding(&c, &b, &input, &digests[0]).unwrap();
        assert_eq!(e.get_ids()[0], 101);
        assert_eq!(*e.get_ids().last().unwrap(), 102);
        assert!(e.get_attention_mask().iter().all(|v| *v == 1));
    }
    let a = b.encode_document("abc").unwrap();
    let z = b.encode_document("a\u{200b}bc").unwrap();
    assert_eq!(a.get_ids(), z.get_ids());
    assert_ne!(
        encoding_digest(&c, &sha256_hex(b"abc"), a.get_ids(), a.get_attention_mask()).unwrap(),
        encoding_digest(
            &c,
            &sha256_hex("a\u{200b}bc".as_bytes()),
            z.get_ids(),
            z.get_attention_mask()
        )
        .unwrap()
    );
}
#[test]
fn shared_config_retains_all_legacy_effective_fields() {
    let c = nomic_config(768).unwrap();
    assert_eq!(c.vocab_size, 30528);
    assert_eq!(c.n_embd, 768);
    assert_eq!(c.n_head, 12);
    assert_eq!(c.n_layer, 12);
    assert_eq!(c.n_inner, 3072);
    assert_eq!(c.n_positions, 8192);
    assert_eq!(c.type_vocab_size, 2);
    assert_eq!(c.layer_norm_epsilon, 1e-12);
    assert_eq!(c.rotary_emb_fraction, 1.0);
    assert_eq!(c.rotary_emb_base, 1000.0);
    assert!(!c.rotary_emb_interleaved);
    assert!(!c.qkv_proj_bias);
    assert!(!c.mlp_fc1_bias);
    assert!(!c.mlp_fc2_bias);
    assert!(!c.prenorm);
    assert_eq!(c.activation_function, "swiglu");
    assert_eq!(c.model_type.as_deref(), Some("nomic_bert"));
    let original = candle_transformers::models::nomic_bert::Config {
        vocab_size: 30528,
        n_embd: 768,
        n_head: 12,
        n_layer: 12,
        n_inner: 3072,
        n_positions: 8192,
        type_vocab_size: 2,
        layer_norm_epsilon: 1e-12,
        rotary_emb_fraction: 1.0,
        rotary_emb_base: 1000.0,
        rotary_emb_interleaved: false,
        qkv_proj_bias: false,
        mlp_fc1_bias: false,
        mlp_fc2_bias: false,
        activation_function: "swiglu".into(),
        prenorm: false,
        model_type: Some("nomic_bert".into()),
    };
    assert_eq!(c, original);
}
#[test]
fn tokenizer_owned_bytes_reject_forgery_and_survive_replacement() {
    let bytes = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
    )
    .unwrap();
    let mut changed = bytes.clone();
    changed[100] ^= 1;
    assert!(TokenBudget::from_verified_bytes(&changed, manifest::default_model()).is_err());
    let b = TokenBudget::from_verified_bytes(&bytes, manifest::default_model()).unwrap();
    drop(bytes);
    assert_eq!(b.count_document("hello").unwrap(), 7);
}
#[test]
fn owned_verified_buffer_survives_path_replacement_and_rejects_changed_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("asset");
    std::fs::write(&path, b"original").unwrap();
    let hash = sha256_hex(b"original");
    let hash: &'static str = Box::leak(hash.into_boxed_str());
    let a = Artifact {
        filename: "asset",
        url: "unused",
        sha256: hash,
        size: 8,
    };
    let bytes = read_verified(&path, &a, deadline()).unwrap();
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, b"modified").unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(bytes, b"original");
    assert!(read_verified(&path, &a, deadline()).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let link = dir.path().join("symlink");
        symlink(&path, &link).unwrap();
        assert!(read_verified(&link, &a, deadline()).is_err());
    }
}
#[test]
#[ignore = "requires official model assets; run with MINDSPACE_S3_MODEL_DIR"]
fn real_owned_model_inference_and_timings() {
    let dir =
        PathBuf::from(std::env::var_os("MINDSPACE_S3_MODEL_DIR").expect("MINDSPACE_S3_MODEL_DIR"));
    let c = contract();
    let e = manifest::default_model();
    let start = Instant::now();
    let b = load_budget(&dir, e, deadline()).unwrap();
    let tokenizer_ms = start.elapsed().as_secs_f64() * 1000.0;
    let texts = [
        "Document ownership and exact citations.",
        "漢字かな한글 🦉e\u{301}",
        "# Heading\r\n\r\n```rust\r\nfn main() {}\r\n```",
        "",
    ];
    let hashes = texts
        .iter()
        .map(|s| sha256_hex(s.as_bytes()))
        .collect::<Vec<_>>();
    let inputs = texts
        .iter()
        .zip(&hashes)
        .map(|(s, h)| StrictInput {
            content: s,
            expected_tokens: b.count_document(s).unwrap() as u32,
            payload_hash: h,
        })
        .collect::<Vec<_>>();
    let start = Instant::now();
    let digests = preflight(&c, &b, &inputs, deadline()).unwrap();
    let tokenization_ms = start.elapsed().as_secs_f64() * 1000.0;
    let start = Instant::now();
    let model = load_model(&dir, e, deadline()).unwrap();
    let model_load_ms = start.elapsed().as_secs_f64() * 1000.0;
    let mut inference_ms = 0.0;
    let mut serialization_ms = 0.0;
    for (input, digest) in inputs.iter().zip(digests) {
        let encoded = checked_encoding(&c, &b, input, &digest).unwrap();
        let start = Instant::now();
        let vector =
            forward_model(&model, encoded.get_ids(), encoded.get_attention_mask(), 768).unwrap();
        inference_ms += start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let stored = result(&c, encoded.len(), digest, &vector).unwrap();
        serialization_ms += start.elapsed().as_secs_f64() * 1000.0;
        stored.validate(&c).unwrap();
    }
    eprintln!("strict phases: chunks={} tokenizer_ms={tokenizer_ms:.3} preflight_ms={tokenization_ms:.3} model_load_ms={model_load_ms:.3} inference_ms={inference_ms:.3} serialization_ms={serialization_ms:.3}",inputs.len());
}

#[test]
fn protected_artifacts_reject_size_permissions_and_symlink_ancestors() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("asset");
    std::fs::write(&path, b"data").unwrap();
    assert!(protected_file(&path, 3).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(protected_file(&path, 4).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("alias");
        symlink(dir.path(), &link).unwrap();
        assert!(protected_file(&link.join("asset"), 4).is_err());
        let hard = dir.path().join("hard");
        std::fs::hard_link(&path, hard).unwrap();
        assert!(protected_file(&path, 4).is_err());
    }
}

#[test]
#[ignore = "requires official weights; copies verified files into a disposable protected directory"]
fn real_inference_uses_owned_model_and_tokenizer_after_path_replacement() {
    let source =
        PathBuf::from(std::env::var_os("MINDSPACE_S3_MODEL_DIR").expect("MINDSPACE_S3_MODEL_DIR"));
    let dir = tempfile::tempdir().unwrap();
    for name in ["model.safetensors", "tokenizer.json"] {
        std::fs::copy(source.join(name), dir.path().join(name)).unwrap();
    }
    let c = contract();
    let entry = manifest::default_model();
    let b = load_budget(dir.path(), entry, deadline()).unwrap();
    let model = load_model(dir.path(), entry, deadline()).unwrap();
    for name in ["model.safetensors", "tokenizer.json"] {
        let replacement = dir.path().join(format!("{name}.replacement"));
        std::fs::write(&replacement, b"corrupt replacement").unwrap();
        std::fs::rename(&replacement, dir.path().join(name)).unwrap();
    }
    let text = "Already verified owned model and tokenizer survive path replacement.";
    let hash = sha256_hex(text.as_bytes());
    let input = StrictInput {
        content: text,
        expected_tokens: b.count_document(text).unwrap() as u32,
        payload_hash: &hash,
    };
    let digests = preflight(&c, &b, std::slice::from_ref(&input), deadline()).unwrap();
    let encoded = checked_encoding(&c, &b, &input, &digests[0]).unwrap();
    let output =
        forward_model(&model, encoded.get_ids(), encoded.get_attention_mask(), 768).unwrap();
    result(&c, encoded.len(), digests[0].clone(), &output).unwrap();
    assert!(load_budget(dir.path(), entry, deadline()).is_err());
    assert!(load_model(dir.path(), entry, deadline()).is_err());
}

#[test]
#[cfg(feature = "spawn-gate")]
fn fixture_encoder_is_structural_and_has_indexed_failure() {
    let c = contract();
    let b = budget();
    let hash = sha256_hex(b"hello");
    let inputs = [
        StrictInput {
            content: "hello",
            expected_tokens: 7,
            payload_hash: &hash,
        },
        StrictInput {
            content: "hello",
            expected_tokens: 7,
            payload_hash: &hash,
        },
    ];
    let digests = preflight(&c, &b, &inputs, deadline()).unwrap();
    let first = fake_encode(&c, &b, &inputs, &digests, deadline(), "unit").unwrap();
    let alternate = fake_encode(&c, &b, &inputs, &digests, deadline(), "alternate").unwrap();
    assert_ne!(first[0].vector_hash, alternate[0].vector_hash);
    for vector in first {
        vector.validate(&c).unwrap();
    }
    assert!(fake_encode(&c, &b, &inputs, &digests, deadline(), "fail:2").is_err());
    assert!(fake_encode(&c, &b, &inputs, &digests, deadline(), "unsupported").is_err());
}

#[test]
fn strict_entry_checks_ceiling_before_missing_weights() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nomic-tokenizer.json"),
        dir.path().join("tokenizer.json"),
    )
    .unwrap();
    let c = contract();
    let exact = "hello ".repeat(2042);
    let hash = sha256_hex(exact.as_bytes());
    let input = [StrictInput {
        content: &exact,
        expected_tokens: 2048,
        payload_hash: &hash,
    }];
    let err = encode_in(&c, &input, deadline(), dir.path())
        .unwrap_err()
        .to_string();
    assert!(err.contains("model.safetensors"), "{err}");
    let over = "hello ".repeat(2043);
    let hash = sha256_hex(over.as_bytes());
    let input = [StrictInput {
        content: &over,
        expected_tokens: 2049,
        payload_hash: &hash,
    }];
    let err = encode_in(&c, &input, deadline(), dir.path())
        .unwrap_err()
        .to_string();
    assert!(err.contains("2049"), "{err}");
    assert!(!err.contains("model.safetensors"), "{err}");
}

#[test]
#[ignore = "requires official weights; full 2048-token CPU forward is expensive"]
fn real_exact_2048_forward_and_2049_refusal() {
    let dir =
        PathBuf::from(std::env::var_os("MINDSPACE_S3_MODEL_DIR").expect("MINDSPACE_S3_MODEL_DIR"));
    let c = contract();
    let text = "hello ".repeat(2042);
    let hash = sha256_hex(text.as_bytes());
    let inputs = [StrictInput {
        content: &text,
        expected_tokens: 2048,
        payload_hash: &hash,
    }];
    let outputs = encode_in(&c, &inputs, Instant::now() + Duration::from_secs(600), &dir).unwrap();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].token_count, 2048);
    outputs[0].validate(&c).unwrap();
    let text = "hello ".repeat(2043);
    let hash = sha256_hex(text.as_bytes());
    let inputs = [StrictInput {
        content: &text,
        expected_tokens: 2049,
        payload_hash: &hash,
    }];
    let error = encode_in(&c, &inputs, deadline(), &dir)
        .unwrap_err()
        .to_string();
    assert!(error.contains("2049"), "{error}");
}
