//! Owned, authenticated document inference for isolated staging. Never the generic trait path.
use super::budget::TokenBudget;
use super::manifest::{self, Artifact, ModelManifestEntry};
use super::runtime::{forward_model, nomic_config};
use super::strict_contract::{encoding_digest, StrictContract, StrictVector};
use crate::core::{encode_f32_le, sha256_hex, Error, Result};
use candle_core::{DType, Device};
use candle_transformers::models::nomic_bert::NomicBertModel;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) struct StrictInput<'a> {
    pub content: &'a str,
    pub expected_tokens: u32,
    pub payload_hash: &'a str,
}
fn bad(detail: impl Into<String>) -> Error {
    Error::InvalidVector {
        detail: detail.into(),
    }
}
fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() > deadline {
        Err(Error::Storage(
            "semantic staging cooperative deadline exceeded".into(),
        ))
    } else {
        Ok(())
    }
}
fn protected_file(path: &Path, size: u64) -> Result<File> {
    if !path.is_absolute() {
        return Err(bad("strict artifacts require an absolute local path"));
    }
    for parent in path.ancestors().skip(1) {
        let m = std::fs::symlink_metadata(parent)
            .map_err(|e| bad(format!("strict artifact directory unavailable: {e}")))?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(bad(
                "strict artifact ancestor must be a directory without symlinks",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let uid = unsafe { libc::geteuid() };
            let sticky_root = m.uid() == 0 && m.mode() & 0o1000 != 0;
            if (m.uid() != 0 && m.uid() != uid) || (m.mode() & 0o022 != 0 && !sticky_root) {
                return Err(bad(
                    "strict artifact ancestor is not protected local storage",
                ));
            }
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| {
        bad(format!(
            "strict artifact {} unavailable: {e}",
            path.display()
        ))
    })?;
    let m = file.metadata().map_err(|e| bad(e.to_string()))?;
    if !m.is_file() || m.len() != size {
        return Err(bad(
            "strict artifact type or exact size differs from pinned manifest",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { libc::geteuid() };
        if (m.uid() != uid && m.uid() != 0) || m.mode() & 0o022 != 0 || m.nlink() != 1 {
            return Err(bad("strict artifact is not a protected regular file"));
        }
    }
    Ok(file)
}
/// The very bytes authenticated here, not a subsequent path open, are used by the loader.
fn read_verified(path: &Path, artifact: &Artifact, deadline: Instant) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    check_deadline(deadline)?;
    let mut file = protected_file(path, artifact.size)?;
    let capacity =
        usize::try_from(artifact.size).map_err(|_| bad("artifact size cannot fit this build"))?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        check_deadline(deadline)?;
        let remaining = (artifact.size + 1).saturating_sub(bytes.len() as u64);
        if remaining == 0 {
            return Err(bad("strict artifact exceeded pinned byte bound"));
        }
        let read_len = buffer.len().min(remaining as usize);
        let n = file
            .read(&mut buffer[..read_len])
            .map_err(|e| bad(format!("strict artifact read failed: {e}")))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
        hash.update(&buffer[..n]);
    }
    check_deadline(deadline)?;
    let actual = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if bytes.len() as u64 != artifact.size || actual != artifact.sha256 {
        return Err(bad("strict artifact bytes do not match pinned manifest"));
    }
    Ok(bytes)
}
fn artifact(entry: &ModelManifestEntry, name: &str) -> Result<Artifact> {
    entry
        .artifacts
        .iter()
        .find(|a| a.filename == name)
        .copied()
        .ok_or_else(|| bad("official strict artifact missing"))
}
fn model_dir(model_id: &str) -> Result<PathBuf> {
    #[cfg(feature = "spawn-gate")]
    if let Some(path) = std::env::var_os("MINDSPACE_SEMANTIC_STAGE_MODEL_DIR") {
        return Ok(PathBuf::from(path));
    }
    super::manager::model_dir(model_id)
}
fn load_budget(dir: &Path, entry: &ModelManifestEntry, deadline: Instant) -> Result<TokenBudget> {
    let path = dir.join("tokenizer.json");
    #[cfg(feature = "spawn-gate")]
    let path = std::env::var_os("MINDSPACE_SEMANTIC_STAGE_TOKENIZER")
        .map(PathBuf::from)
        .unwrap_or(path);
    let bytes = read_verified(&path, &artifact(entry, "tokenizer.json")?, deadline)?;
    TokenBudget::from_verified_bytes(&bytes, entry)
}
fn preflight(
    contract: &StrictContract,
    budget: &TokenBudget,
    inputs: &[StrictInput<'_>],
    deadline: Instant,
) -> Result<Vec<String>> {
    if budget.identity() != contract.budget_identity {
        return Err(bad("strict token budget differs from selected chunk set"));
    }
    if inputs.len() > 8192 {
        return Err(bad("strict entry count exceeds 8192"));
    }
    let mut out = Vec::with_capacity(inputs.len());
    let mut total_bytes = 0usize;
    let mut total_tokens = 0u64;
    for input in inputs {
        check_deadline(deadline)?;
        total_bytes = total_bytes
            .checked_add(input.content.len())
            .ok_or_else(|| bad("strict input byte overflow"))?;
        if total_bytes > 16 * 1024 * 1024
            || sha256_hex(input.content.as_bytes()) != input.payload_hash
        {
            return Err(bad("strict source payload hash or byte bound differs"));
        }
        let encoded = budget.encode_document(input.content)?;
        if encoded.len() != input.expected_tokens as usize {
            return Err(bad(format!(
                "actual complete token count {} differs from stored chunk count {}",
                encoded.len(),
                input.expected_tokens
            )));
        }
        total_tokens += encoded.len() as u64;
        if total_tokens > 16_777_216 {
            return Err(bad("strict total token work bound exceeded"));
        }
        out.push(encoding_digest(
            contract,
            input.payload_hash,
            encoded.get_ids(),
            encoded.get_attention_mask(),
        )?);
    }
    Ok(out)
}
fn load_model(dir: &Path, entry: &ModelManifestEntry, deadline: Instant) -> Result<NomicBertModel> {
    let bytes = read_verified(
        &dir.join("model.safetensors"),
        &artifact(entry, "model.safetensors")?,
        deadline,
    )?;
    let device = Device::Cpu;
    let tensors =
        candle_core::safetensors::load_buffer(&bytes, &device).map_err(|e| bad(e.to_string()))?;
    // Tensor storage is owned, so the verified source buffer need not outlive construction.
    drop(bytes);
    let builder = candle_nn::VarBuilder::from_tensors(tensors, DType::F32, &device);
    let model = NomicBertModel::load(builder, &nomic_config(entry.dimension as usize)?)
        .map_err(|e| bad(e.to_string()))?;
    check_deadline(deadline)?;
    Ok(model)
}
fn checked_encoding(
    contract: &StrictContract,
    budget: &TokenBudget,
    input: &StrictInput<'_>,
    expected_digest: &str,
) -> Result<tokenizers::Encoding> {
    if sha256_hex(input.content.as_bytes()) != input.payload_hash {
        return Err(bad("strict source changed after preflight"));
    }
    let encoding = budget.encode_document(input.content)?;
    if encoding.len() != input.expected_tokens as usize
        || encoding.len() > contract.max_tokens as usize
        || encoding_digest(
            contract,
            input.payload_hash,
            encoding.get_ids(),
            encoding.get_attention_mask(),
        )? != expected_digest
    {
        return Err(bad(
            "strict encoding differs from checked source/token contract",
        ));
    }
    Ok(encoding)
}
fn result(
    contract: &StrictContract,
    count: usize,
    digest: String,
    vector: &[f32],
) -> Result<StrictVector> {
    let embedding = encode_f32_le(vector)?;
    let vector_hash = sha256_hex(&embedding);
    let value = StrictVector {
        token_count: count as u32,
        encoding_digest: digest,
        embedding,
        vector_hash,
    };
    value.validate(contract)?;
    Ok(value)
}

/// All token preflight precedes weight access; every forward uses its exact checked IDs/mask.
pub(crate) fn encode_all(
    contract: &StrictContract,
    inputs: &[StrictInput<'_>],
    deadline: Instant,
) -> Result<Vec<StrictVector>> {
    let dir = model_dir(&contract.model_id)?;
    encode_in(contract, inputs, deadline, &dir)
}
fn encode_in(
    contract: &StrictContract,
    inputs: &[StrictInput<'_>],
    deadline: Instant,
    dir: &Path,
) -> Result<Vec<StrictVector>> {
    contract.validate_current()?;
    check_deadline(deadline)?;
    let entry =
        manifest::find_model(&contract.model_id).ok_or_else(|| bad("unsupported strict model"))?;
    let budget = load_budget(dir, entry, deadline)?;
    let digests = preflight(contract, &budget, inputs, deadline)?;
    #[cfg(feature = "spawn-gate")]
    if let Ok(mode) = std::env::var("MINDSPACE_SEMANTIC_STAGE_FAKE_ENCODER") {
        return fake_encode(contract, &budget, inputs, &digests, deadline, &mode);
    }
    let model = load_model(dir, entry, deadline)?;
    #[cfg(feature = "spawn-gate")]
    test_pause("after_snapshot", deadline)?;
    let mut vectors = Vec::with_capacity(inputs.len());
    for (input, digest) in inputs.iter().zip(digests) {
        check_deadline(deadline)?;
        let encoding = checked_encoding(contract, &budget, input, &digest)?;
        let vector = forward_model(
            &model,
            encoding.get_ids(),
            encoding.get_attention_mask(),
            contract.dimension as usize,
        )?;
        check_deadline(deadline)?;
        vectors.push(result(contract, encoding.len(), digest, &vector)?);
    }
    Ok(vectors)
}
#[cfg(feature = "spawn-gate")]
fn fake_encode(
    contract: &StrictContract,
    budget: &TokenBudget,
    inputs: &[StrictInput<'_>],
    digests: &[String],
    deadline: Instant,
    mode: &str,
) -> Result<Vec<StrictVector>> {
    let fail = mode
        .strip_prefix("fail:")
        .map(|n| {
            n.parse::<usize>()
                .map_err(|_| bad("invalid test-only failure index"))
        })
        .transpose()?;
    if fail == Some(0) || (mode != "unit" && mode != "alternate" && fail.is_none()) {
        return Err(bad("invalid test-only fake encoder mode"));
    }
    let mut out = Vec::new();
    for (i, (input, digest)) in inputs.iter().zip(digests).enumerate() {
        check_deadline(deadline)?;
        if fail == Some(i + 1) {
            return Err(bad("injected test-only semantic forward failure"));
        }
        let encoding = checked_encoding(contract, budget, input, digest)?;
        let mut vector = vec![0.; contract.dimension as usize];
        vector[usize::from(mode == "alternate")] = 1.;
        out.push(result(contract, encoding.len(), digest.clone(), &vector)?);
    }
    Ok(out)
}
#[cfg(feature = "spawn-gate")]
fn test_pause(phase: &str, deadline: Instant) -> Result<()> {
    if std::env::var("MINDSPACE_SEMANTIC_ENCODER_PAUSE").as_deref() != Ok(phase) {
        return Ok(());
    }
    let Some(prefix) = std::env::var_os("MINDSPACE_SEMANTIC_ENCODER_GATE_PATH") else {
        return Ok(());
    };
    let marker = |ext: &str| {
        let mut path = prefix.clone();
        path.push(ext);
        PathBuf::from(path)
    };
    std::fs::write(marker(".reached"), b"").map_err(|e| bad(e.to_string()))?;
    while !marker(".go").exists() {
        check_deadline(deadline)?;
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    Ok(())
}
#[cfg(test)]
#[path = "strict_tests.rs"]
mod tests;
