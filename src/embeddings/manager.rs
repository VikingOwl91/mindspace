//! Model management: install, verify, select, status. Filesystem + mind_meta.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
#[cfg(feature = "embeddings")]
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::core::{Error, Result};
use crate::embeddings::manifest::{find_model, ModelManifestEntry, MANIFEST};
use crate::storage::Store;

const ENABLED_KEY: &str = "embedding.enabled";
const MODEL_KEY: &str = "embedding.model";

pub fn data_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("mindspace");
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h).join(".local/share/mindspace");
    }
    PathBuf::from(".mindspace")
}

pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

pub fn model_dir(id: &str) -> PathBuf {
    models_dir().join(id)
}

pub fn model_dir_in(base: &Path, id: &str) -> PathBuf {
    base.join("models").join(id)
}

/// Validate a model id against the trusted manifest and reject path traversal.
pub fn validate_model_id(
    id: &str,
) -> Result<&'static crate::embeddings::manifest::ModelManifestEntry> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains('\0')
    {
        return Err(Error::Storage(format!("invalid model id: {id}")));
    }
    crate::embeddings::manifest::find_model(id)
        .ok_or_else(|| Error::Storage(format!("unknown model: {id}")))
}

/// Reject symlinked paths (defense against local filesystem redirection).
///
/// NOTE: these checks are non-atomic (check-then-use). Under Mindspace's
/// local-first threat model the model directory lives in the user's own XDG
/// data dir and is assumed not to be concurrently modified by an attacker;
/// this is defense-in-depth, not TOCTOU-proof. `create_new` is not used for
/// the destination because installation must be able to replace a corrupt
/// existing artifact atomically via rename.
pub(crate) fn reject_symlink(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(Error::Storage(format!(
            "refusing symlinked path: {}",
            path.display()
        ))),
        _ => Ok(()),
    }
}

pub fn is_installed(entry: &ModelManifestEntry) -> bool {
    is_installed_in(&data_dir(), entry)
}

pub fn is_installed_in(base: &Path, entry: &ModelManifestEntry) -> bool {
    entry.artifacts.iter().all(|a| {
        let p = model_dir_in(base, entry.id).join(a.filename);
        if !p.is_file() {
            return false;
        }
        match fs::metadata(&p) {
            Ok(m) if m.len() == a.size => verify_file(&p, a).is_ok(),
            _ => false,
        }
    })
}

pub fn verify_file(path: &Path, art: &crate::embeddings::manifest::Artifact) -> Result<()> {
    reject_symlink(path)?;
    let meta = fs::metadata(path).map_err(|e| Error::Storage(e.to_string()))?;
    if meta.len() != art.size {
        return Err(Error::Storage(format!(
            "size mismatch for {}: got {}, expected {}",
            path.display(),
            meta.len(),
            art.size
        )));
    }
    let mut f = fs::File::open(path).map_err(|e| Error::Storage(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| Error::Storage(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = hex(hasher.finalize());
    if got != art.sha256 {
        return Err(Error::Storage(format!(
            "checksum mismatch for {}: got {}, expected {}",
            path.display(),
            got,
            art.sha256
        )));
    }
    Ok(())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let b = bytes.as_ref();
    let mut out = String::with_capacity(b.len() * 2);
    for &x in b {
        out.push(HEX[(x >> 4) as usize] as char);
        out.push(HEX[(x & 0x0f) as usize] as char);
    }
    out
}

pub fn install_model(entry: &ModelManifestEntry) -> Result<()> {
    install_model_opts(entry, &data_dir(), None)
}

pub fn install_model_opts(
    entry: &ModelManifestEntry,
    base: &Path,
    base_url: Option<&str>,
) -> Result<()> {
    let dir = model_dir_in(base, entry.id);
    // Reject symlinked install roots before any write.
    reject_symlink(&dir)?;
    if let Some(parent) = dir.parent() {
        reject_symlink(parent)?;
    }
    fs::create_dir_all(&dir).map_err(|e| Error::Storage(e.to_string()))?;
    reject_symlink(&dir)?;
    for art in entry.artifacts {
        let dest = dir.join(art.filename);
        reject_symlink(&dest)?;
        if dest.is_file() && verify_file(&dest, art).is_ok() {
            continue;
        }
        let url = match base_url {
            Some(base) => format!("{}/{}", base.trim_end_matches('/'), art.filename),
            None => art.url.to_string(),
        };
        if base_url.is_none() && !url.starts_with("https://") {
            return Err(Error::Storage(
                "refusing non-HTTPS download URL".to_string(),
            ));
        }
        // unique temp file to avoid concurrent-install races
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp = dir.join(format!(
            ".{}.{}.{}.tmp",
            art.filename,
            std::process::id(),
            nanos
        ));
        // download + verify + atomic publish; clean up tmp on any failure
        let result = (|| -> Result<()> {
            reject_symlink(&tmp)?;
            if base_url.is_some() {
                download_with(&url, &tmp, art.size, false)?;
            } else {
                download(&url, &tmp, art.size)?;
            }
            verify_file(&tmp, art)?;
            fs::rename(&tmp, &dest).map_err(|e| Error::Storage(e.to_string()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
    }
    Ok(())
}

pub(crate) fn download(url: &str, dest: &Path, max_size: u64) -> Result<()> {
    download_with(url, dest, max_size, true)
}

pub(crate) fn download_with(url: &str, dest: &Path, max_size: u64, https_only: bool) -> Result<()> {
    #[cfg(feature = "embeddings")]
    {
        use std::io::Write;
        let mut builder = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(30))
            .timeout(Duration::from_secs(300));
        if https_only {
            builder = builder.https_only(true);
        }
        let agent = builder.build();
        let resp = agent
            .get(url)
            .call()
            .map_err(|e| Error::Storage(format!("download failed: {e}")))?;
        let mut reader = resp.into_reader();
        let mut file = fs::File::create(dest).map_err(|e| Error::Storage(e.to_string()))?;
        let mut buf = [0u8; 65536];
        let mut total: u64 = 0;
        let res = loop {
            match reader.read(&mut buf) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    total += n as u64;
                    if total > max_size {
                        break Err(Error::Storage(format!(
                            "download exceeded expected size {} (got {})",
                            max_size, total
                        )));
                    }
                    if let Err(e) = file.write_all(&buf[..n]) {
                        break Err(Error::Storage(e.to_string()));
                    }
                }
                Err(e) => break Err(Error::Storage(e.to_string())),
            }
        };
        if res.is_err() {
            let _ = fs::remove_file(dest); // F9: clean up on read failure
        } else {
            let _ = file.flush();
        }
        res
    }
    #[cfg(not(feature = "embeddings"))]
    {
        let _ = (url, dest, max_size, https_only);
        Err(Error::Storage(
            "embeddings feature disabled; cannot download".to_string(),
        ))
    }
}

pub fn remove_model(id: &str) -> Result<()> {
    let entry = validate_model_id(id)?;
    let dir = model_dir(entry.id);
    reject_symlink(&dir)?;
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| Error::Storage(e.to_string()))?;
    }
    Ok(())
}

// ---- persistence via mind_meta ----

pub fn is_enabled(store: &dyn Store) -> bool {
    store
        .get_meta(ENABLED_KEY)
        .ok()
        .flatten()
        .map(|v| v == "true")
        .unwrap_or(false)
}

pub fn selected_model(store: &dyn Store) -> Option<String> {
    store.get_meta(MODEL_KEY).ok().flatten()
}

pub fn set_enabled(store: &dyn Store, enabled: bool) -> Result<()> {
    store.set_meta(ENABLED_KEY, if enabled { "true" } else { "false" })
}

pub fn set_selected_model(store: &dyn Store, id: &str) -> Result<()> {
    if find_model(id).is_none() {
        return Err(Error::Storage(format!("unknown model: {id}")));
    }
    if !is_installed(find_model(id).unwrap()) {
        return Err(Error::Storage(format!(
            "model '{id}' is not installed; install it first"
        )));
    }
    store.set_meta(MODEL_KEY, id)
}

pub struct EmbeddingsStatus {
    pub enabled: bool,
    pub selected_model: Option<String>,
    pub installed: bool,
    pub verified: bool,
    pub dimension: Option<u32>,
    pub runtime: &'static str,
    pub indexing_available: bool,
}

pub fn status(store: &dyn Store) -> EmbeddingsStatus {
    status_in(&data_dir(), store)
}

pub fn status_in(base: &Path, store: &dyn Store) -> EmbeddingsStatus {
    let enabled = is_enabled(store);
    let selected = selected_model(store);
    let (installed, verified, dimension) = match selected.as_deref().and_then(find_model) {
        Some(e) => {
            let inst = is_installed_in(base, e);
            let ver = if inst {
                e.artifacts
                    .iter()
                    .all(|a| verify_file(&model_dir_in(base, e.id).join(a.filename), a).is_ok())
            } else {
                false
            };
            (inst, ver, Some(e.dimension))
        }
        None => (false, false, None),
    };
    EmbeddingsStatus {
        enabled,
        selected_model: selected,
        installed,
        verified,
        dimension,
        runtime: "candle (cpu)",
        indexing_available: false, // M3 not yet implemented
    }
}

pub fn model_info(id: &str) -> Result<&'static ModelManifestEntry> {
    find_model(id).ok_or_else(|| Error::Storage(format!("unknown model: {id}")))
}

pub fn list_models() -> Vec<(&'static ModelManifestEntry, bool)> {
    MANIFEST.iter().map(|m| (m, is_installed(m))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn mock_server(
        status: u16,
        body: Vec<u8>,
        redirect_to: Option<String>,
    ) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            for _ in 0..8 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        use std::io::{Read, Write};
                        let mut buf = [0u8; 4096];
                        let _ = stream.read(&mut buf);
                        if let Some(loc) = &redirect_to {
                            let resp = format!(
                                "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                loc
                            );
                            let _ = stream.write_all(resp.as_bytes());
                        } else {
                            let resp = format!(
                                "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                status,
                                body.len()
                            );
                            let _ = stream.write_all(resp.as_bytes());
                            let _ = stream.write_all(&body);
                        }
                        let _ = stream.flush();
                    }
                    Err(_) => break,
                }
            }
        });
        (port, handle)
    }

    #[test]
    fn validate_model_id_rejects_traversal_and_unknown() {
        assert!(validate_model_id("../etc").is_err());
        assert!(validate_model_id("nomic-embed-text-v1.5/../x").is_err());
        assert!(validate_model_id("a/b").is_err());
        assert!(validate_model_id("a\\b").is_err());
        assert!(validate_model_id("").is_err());
        assert!(validate_model_id("nope").is_err());
        assert!(validate_model_id("nomic-embed-text-v1.5").is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn reject_symlink_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        let _ = std::os::unix::fs::symlink(&real, &link);
        assert!(reject_symlink(&link).is_err());
        assert!(reject_symlink(&real).is_ok());
    }

    #[test]
    fn download_rejects_oversized() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("x.tmp");
        let body = vec![0u8; 100];
        let (port, _h) = mock_server(200, body, None);
        let url = format!("http://127.0.0.1:{}/f", port);
        let res = download_with(&url, &tmp, 10, false);
        assert!(res.is_err());
        assert!(!tmp.exists());
    }

    #[test]
    fn download_rejects_http_url_via_https_only() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("x.tmp");
        let body = vec![0u8; 10];
        let (port, _h) = mock_server(200, body, None);
        let url = format!("http://127.0.0.1:{}/f", port);
        let res = download(&url, &tmp, 1000);
        assert!(res.is_err(), "plain http must be rejected by https_only");
    }

    #[test]
    fn install_atomic_no_partial_on_wrong_checksum() {
        let entry = crate::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap();
        let base_tmp = tempfile::tempdir().unwrap();
        let dir = model_dir_in(base_tmp.path(), entry.id);
        let body = vec![0u8; 7];
        let (port, _h) = mock_server(200, body, None);
        let base_url = format!("http://127.0.0.1:{}", port);
        let res = install_model_opts(entry, base_tmp.path(), Some(&base_url));
        assert!(res.is_err());
        for art in entry.artifacts {
            let dest = dir.join(art.filename);
            assert!(
                !dest.exists(),
                "partial artifact published: {}",
                dest.display()
            );
        }
        if dir.exists() {
            for e in fs::read_dir(&dir).unwrap() {
                let n = e.unwrap().file_name().to_string_lossy().to_string();
                assert!(!n.ends_with(".tmp"), "stray tmp file: {}", n);
            }
        }
    }

    #[test]
    fn download_network_failure_cleans_tmp() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                drop(stream);
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join("netfail.tmp");
        let url = format!("http://127.0.0.1:{}/f", port);
        let res = download_with(&url, &tmp, 1000, false);
        assert!(res.is_err());
        assert!(!tmp.exists(), "tmp must be cleaned up on network failure");
        let _ = handle.join();
    }

    #[test]
    fn concurrency_two_installers() {
        let entry = crate::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap();
        let base_tmp = tempfile::tempdir().unwrap();
        let body = vec![0u8; 10];
        let (port, _h) = mock_server(200, body, None);
        let base_url = format!("http://127.0.0.1:{}", port);
        let e1 = entry;
        let e2 = entry;
        let b1 = base_url.clone();
        let b2 = base_url.clone();
        let base1 = base_tmp.path().to_path_buf();
        let base2 = base_tmp.path().to_path_buf();
        let h1 = thread::spawn(move || install_model_opts(e1, &base1, Some(&b1)));
        let h2 = thread::spawn(move || install_model_opts(e2, &base2, Some(&b2)));
        let r1 = h1.join().unwrap();
        let r2 = h2.join().unwrap();
        assert!(r1.is_err() || r2.is_err());
    }

    #[test]
    fn install_idempotent_offline_reuse() {
        let entry = crate::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap();
        if !is_installed(entry) {
            return;
        }
        assert!(install_model(entry).is_ok());
        assert!(is_installed(entry));
    }

    #[test]
    #[cfg(unix)]
    fn install_rejects_symlinked_dir() {
        let entry = crate::embeddings::manifest::find_model("nomic-embed-text-v1.5").unwrap();
        let base_tmp = tempfile::tempdir().unwrap();
        let dir = model_dir_in(base_tmp.path(), entry.id);
        let target = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        let _ = std::os::unix::fs::symlink(target.path(), &dir);
        let res = install_model_opts(entry, base_tmp.path(), None);
        assert!(res.is_err(), "symlinked dir must be rejected");
    }

    #[test]
    fn validate_model_id_rejects_path_traversal() {
        assert!(validate_model_id("../etc").is_err());
        assert!(validate_model_id("a/b").is_err());
        assert!(validate_model_id("a\\b").is_err());
        assert!(validate_model_id("").is_err());
        assert!(validate_model_id("nope").is_err());
        assert!(validate_model_id("nomic-embed-text-v1.5").is_ok());
    }
}
