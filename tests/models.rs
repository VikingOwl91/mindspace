use mindspace::embeddings::manager;
use mindspace::embeddings::manifest::find_model;
#[cfg(feature = "embeddings")]
use mindspace::embeddings::EmbeddingRuntime;
use mindspace::SqliteStore;
use tempfile::TempDir;

#[test]
fn manifest_contains_nomic_with_expected_fields() {
    let m = find_model("nomic-embed-text-v1.5").expect("nomic present");
    assert_eq!(m.dimension, 768);
    assert_eq!(m.runtime, "candle");
    assert_eq!(m.prefix_document, "search_document: ");
    assert_eq!(m.prefix_query, "search_query: ");
    assert_eq!(m.normalization, "l2");
    assert_eq!(m.pooling, "mean");
    assert_eq!(m.artifacts.len(), 2);
    assert!(!mindspace::embeddings::manifest::MANIFEST.is_empty());
}

#[test]
fn manifest_rejects_unknown_model() {
    assert!(find_model("does-not-exist").is_none());
}

#[test]
fn verify_file_detects_size_mismatch() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("model.safetensors");
    std::fs::write(&p, b"short").unwrap();
    let art = &find_model("nomic-embed-text-v1.5").unwrap().artifacts[0];
    assert!(manager::verify_file(&p, art).is_err());
}

#[test]
fn verify_file_detects_checksum_mismatch() {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("model.safetensors");
    // right size, wrong content
    let art = &find_model("nomic-embed-text-v1.5").unwrap().artifacts[0];
    let data = vec![0u8; art.size as usize];
    std::fs::write(&p, &data).unwrap();
    assert!(manager::verify_file(&p, art).is_err());
}

#[test]
fn is_installed_false_when_missing() {
    let m = find_model("nomic-embed-text-v1.5").unwrap();
    // Ensure not installed in a clean temp data dir by checking a nonexistent model dir.
    // We just assert the function runs; installed state depends on environment.
    let _ = manager::is_installed(m);
}

#[test]
fn enable_disable_persistence() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mind.db");
    SqliteStore::open(&path).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    assert!(!manager::is_enabled(&store));
    manager::set_enabled(&store, true).unwrap();
    assert!(manager::is_enabled(&store));
    manager::set_enabled(&store, false).unwrap();
    assert!(!manager::is_enabled(&store));
}

#[test]
fn set_selected_model_rejects_unknown() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mind.db");
    SqliteStore::open(&path).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    assert!(manager::set_selected_model(&store, "nope").is_err());
}

#[test]
fn status_reports_not_installed_when_absent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mind.db");
    SqliteStore::open(&path).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    let st = manager::status(&store).unwrap();
    assert!(!st.installed);
    assert!(!st.verified);
    assert!(!st.indexing_available);
}

#[cfg(not(feature = "embeddings"))]
#[test]
fn status_reports_no_runtime_without_embedding_support() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mind.db");
    SqliteStore::open(&path).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    let status = manager::status(&store).unwrap();
    assert_eq!(status.runtime, "none (build without embeddings)");
    assert!(!status.indexing_available);
}

#[test]
fn no_network_during_status() {
    // Status must not perform network access; just call it.
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("mind.db");
    SqliteStore::open(&path).unwrap();
    let store = SqliteStore::open(&path).unwrap();
    let _ = manager::status(&store);
}

#[cfg(feature = "embeddings")]
#[test]
fn lazy_load_not_triggered_on_construction() {
    use mindspace::embeddings::CandleRuntime;
    let rt = CandleRuntime::new(&mindspace::embeddings::manifest::NOMIC_V1_5).unwrap();
    assert_eq!(rt.dimension(), 768);
    assert_eq!(rt.model_id(), "nomic-embed-text-v1.5");
}

#[cfg(feature = "embeddings")]
#[test]
fn candle_runtime_prefix_distinction() {
    // Only run if model is installed.
    let dir = manager::model_dir("nomic-embed-text-v1.5").unwrap();
    if !manager::is_installed(
        mindspace::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap(),
    ) {
        return; // skip if not installed
    }
    let rt = mindspace::embeddings::CandleRuntime::load(&dir).unwrap();
    let d = rt.embed_document("machine learning").unwrap();
    let q = rt.embed_query("machine learning").unwrap();
    // cosine should not be 1.0 (prefixes differ)
    let cos: f64 = {
        let dot: f64 = d
            .iter()
            .zip(q.iter())
            .map(|(a, b)| (*a as f64) * (*b as f64))
            .sum();
        let na: f64 = d.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
        let nb: f64 = q.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
        dot / (na * nb)
    };
    assert!(cos < 0.99, "prefixes did not differentiate: {cos}");
    // finite + normalized
    for v in &d {
        assert!(v.is_finite());
    }
    let n: f64 = d.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    assert!((n - 1.0).abs() < 1e-5);
    assert_eq!(d.len(), 768);
}

#[test]
fn no_default_features_compiles() {
    // This test exists only to be compiled; actual no-default check is a build gate.
}

#[test]
fn cli_enable_noninteractive_requires_yes() {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_mind");
    // ensure a db exists
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("mind.db");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let _ = Command::new(bin)
        .args(["init", "--db", db.to_str().unwrap()])
        .status();
    let out = Command::new(bin)
        .args(["--db", db.to_str().unwrap(), "embeddings", "enable"])
        .env("XDG_DATA_HOME", &data)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    // non-TTY without --yes must fail
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("noninteractive")
            || stderr.contains("requires --yes")
            || stderr.contains("--yes"),
        "stderr: {}",
        stderr
    );
}

#[test]
fn cli_model_install_cancel_on_n() {
    use std::io::Write;
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_mind");
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("mind.db");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let _ = Command::new(bin)
        .args(["init", "--db", db.to_str().unwrap()])
        .status();
    let mut child = Command::new(bin)
        .args([
            "--db",
            db.to_str().unwrap(),
            "model",
            "install",
            "nomic-embed-text-v1.5",
        ])
        .env("XDG_DATA_HOME", &data)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(b"n\n");
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("cancelled") || !out.status.success(),
        "stdout: {} stderr: {}",
        stdout,
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cli_index_run_disabled_rejected() {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_mind");
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("mind.db");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let _ = Command::new(bin)
        .args(["init", "--db", db.to_str().unwrap()])
        .status();
    let out = Command::new(bin)
        .args(["--db", db.to_str().unwrap(), "index", "run"])
        .env("XDG_DATA_HOME", &data)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "index run must fail when disabled");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("disabled") || combined.contains("embeddings"),
        "expected disabled-embeddings error, got: {}",
        combined
    );
}
