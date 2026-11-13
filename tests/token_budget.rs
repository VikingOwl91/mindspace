#![cfg(feature = "embeddings")]

use mindspace::embeddings::{budget::TokenBudget, manifest::NOMIC_V1_5, CandleRuntime};
use std::path::Path;

fn budget() -> TokenBudget {
    TokenBudget::from_file(
        Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/nomic-tokenizer.json"
        )),
        &NOMIC_V1_5,
    )
    .unwrap()
}

#[test]
fn complete_input_counts_include_prefix_and_special_tokens() {
    let b = budget();
    assert_eq!(b.count_document("").unwrap(), 6);
    assert_eq!(b.count_query("").unwrap(), 6);
    assert_eq!(b.count_document("hello").unwrap(), 7);
    assert_eq!(b.limit(), 2048);
    assert_eq!(b.identity(), budget().identity());
}

#[test]
fn below_at_and_above_budget_never_truncate() {
    let b = budget();
    for (words, count) in [(2041, 2047), (2042, 2048), (2043, 2049), (4000, 4006)] {
        let text = "hello ".repeat(words);
        assert_eq!(b.count_document(&text).unwrap(), count);
        if count <= b.limit() {
            assert_eq!(b.validate_document(&text).unwrap(), count);
        } else {
            let err = b.validate_document(&text).unwrap_err().to_string();
            assert!(err.contains("embedding token budget exceeded"));
            assert!(err.contains(&format!("{count} tokens")));
        }
    }
}

#[test]
fn unicode_controls_and_long_words_follow_real_model_behavior() {
    let b = budget();
    for s in [
        "漢字かな",
        "🙂👩🏽‍💻",
        "e\u{301}",
        "العربية",
        "\u{200b}\u{202e}hello\u{202c}",
        "\r\n\t ",
    ] {
        assert!(b.validate_document(s).unwrap() >= 6);
    }
    // WordPiece has max_input_chars_per_word=100: this is one [UNK], not 10000 tokens.
    assert_eq!(b.count_document(&"z".repeat(10_000)).unwrap(), 7);
    assert_eq!(
        b.count_document("hello\u{200b}").unwrap(),
        b.count_document("hello").unwrap()
    );
}

#[test]
fn forged_tokenizer_or_manifest_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("tokenizer.json");
    std::fs::write(&p, b"{}").unwrap();
    assert!(TokenBudget::from_file(&p, &NOMIC_V1_5)
        .err()
        .unwrap()
        .to_string()
        .contains("pinned manifest"));
    let mut e = NOMIC_V1_5;
    e.max_seq_len = 999999;
    assert!(TokenBudget::from_file(Path::new("/nonexistent"), &e)
        .err()
        .unwrap()
        .to_string()
        .contains("unsupported"));
}

#[test]
fn strict_embedding_refuses_oversize_before_loading_weights() {
    let d = tempfile::tempdir().unwrap();
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/nomic-tokenizer.json"
        ),
        d.path().join("tokenizer.json"),
    )
    .unwrap();
    let rt = CandleRuntime::new_in(&NOMIC_V1_5, d.path().to_owned());
    assert!(rt
        .embed_document_validated(&"hello ".repeat(2043))
        .unwrap_err()
        .to_string()
        .contains("embedding token budget exceeded"));
    assert!(rt
        .embed_document_validated("hello")
        .unwrap_err()
        .to_string()
        .contains("missing model artifacts"));
}

/// Opt-in measurement only. Uses local, manifest-pinned weights; never downloads artifacts.
#[test]
#[ignore = "requires MINDSPACE_S3_MODEL_DIR with the pinned local model artifacts"]
fn measure_actual_inference_on_representative_inputs() {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::time::Instant;
    let model_dir = std::env::var_os("MINDSPACE_S3_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .expect("set MINDSPACE_S3_MODEL_DIR to the pinned local model directory");
    // Stream the large artifact: verification must not add another full model-sized allocation.
    for artifact in NOMIC_V1_5.artifacts {
        let mut f = std::fs::File::open(model_dir.join(artifact.filename)).unwrap();
        assert_eq!(f.metadata().unwrap().len(), artifact.size);
        let mut hash = Sha256::new();
        let mut buf = [0u8; 65536];
        loop {
            let n = f.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        let digest = hash.finalize();
        let actual: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(actual, artifact.sha256);
    }
    let b = TokenBudget::from_file(&model_dir.join("tokenizer.json"), &NOMIC_V1_5).unwrap();
    let rt = CandleRuntime::new_in(&NOMIC_V1_5, model_dir);
    let inputs = [
        ("prose", "# Project notes\n\nDeterministic document chunking preserves all source bytes and precise citations.\n".repeat(8)),
        ("code", "```rust\nfn main() { println!(\"hello world\"); }\n```\n".repeat(8)),
        ("unicode", "漢字かな العربية café e\u{301} 🙂👩🏽‍💻\n".repeat(8)),
        ("controls", "alpha\u{200b}beta \u{202e}display\u{202c}\r\n".repeat(8)),
    ];
    for round in 0..2 {
        for (name, text) in &inputs {
            let tokens = b.validate_document(text).unwrap();
            let started = Instant::now();
            let vector = rt.embed_document_validated(text).unwrap();
            let elapsed = started.elapsed();
            assert_eq!(vector.len(), 768);
            assert!(vector.iter().all(|v| v.is_finite()));
            eprintln!("S3 inference round={round} input={name} tokens={tokens} dimension={} elapsed_ms={:.3} includes_first_load={}", vector.len(), elapsed.as_secs_f64()*1000.0, round == 0 && *name == "prose");
        }
    }
}
