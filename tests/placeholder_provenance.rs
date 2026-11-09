//! M7.3b.3 phase 1: reuse placeholders have explicit, parent-checked provenance.
//!
//! Invariant: a worker can never introduce a "reused" derivation the parent did not announce, for
//! this operation, under this identity. Hostile workers are modelled as producers whose
//! `ExtractionContext` lies, or whose sink rewrites records; the parent side is the real assembler
//! (`consume`) and the real `check_result`. Rejected operations must leave the database unchanged.
//!
//! Separate (and not claimed here): a fresh derivation the worker invents for an *unannounced* blob
//! is structurally valid and cannot be told from a parser result without parsing it again. Test
//! `residual_fabricated_fresh_derivation_is_not_detectable` pins that documented limit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use mindspace::core::{sha256_hex, CodeLanguage, CodeParserIdentity, Result, Uuid};
use mindspace::ingest::code::{self, ParsePolicy, SkipReason};
use mindspace::ingest::confined::Anchor;
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{
    persist, Extracted, ExtractedFiles, ExtractionContext, StoreContext,
};
use mindspace::ingest::stream::{
    consume, stream_directory, FrameWriter, Kind, ProducerLimits, Record, RecordSink, StreamLimits,
};
use mindspace::mcp::McpConfig;
use mindspace::storage::Store;
use mindspace::worker::context::{
    answer, check_result, Context, KnownSet, Manifest, ManifestEntry,
};
use mindspace::worker::PROTOCOL;
use mindspace::{initialize, open_existing, SqliteStore};

const KNOWN: &str = "fn known() {}\n";
const OTHER: &str = "fn other() { let x = 1; }\n";
const EMPTY: &str = "";

struct Db {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn db() -> Db {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mind.db");
    initialize(&path).unwrap();
    open_existing(&path).unwrap().create_space("p", "").unwrap();
    Db { _dir: dir, path }
}

impl Db {
    fn store(&self) -> SqliteStore {
        open_existing(&self.path).unwrap()
    }
    fn space(&self) -> Uuid {
        self.store().get_space("p").unwrap().id
    }
    fn rows(&self) -> BTreeMap<String, i64> {
        let c = rusqlite::Connection::open_with_flags(
            &self.path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let tables: Vec<String> = c
            .prepare(
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .flatten()
            .collect();
        tables
            .into_iter()
            .map(|t| {
                let n = c
                    .query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get(0))
                    .unwrap();
                (t, n)
            })
            .collect()
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    std::fs::write(root.join(rel), content).unwrap();
}

struct Tree {
    _d: tempfile::TempDir,
    root: PathBuf,
    cfg: McpConfig,
}

fn tree(files: &[(&str, &str)]) -> Tree {
    let d = tempfile::tempdir().unwrap();
    for (n, c) in files {
        write(d.path(), n, c);
    }
    let root = std::fs::canonicalize(d.path()).unwrap();
    let cfg = McpConfig::new(&[&root]).unwrap();
    Tree { _d: d, root, cfg }
}

fn anchor(t: &Tree) -> Anchor {
    Anchor::open(&t.root, Some(&t.root), &t.cfg, false).unwrap()
}

fn manifest(op: Uuid, contents: &[&str]) -> Manifest {
    let mut entries: Vec<ManifestEntry> = contents
        .iter()
        .map(|c| ManifestEntry {
            language: CodeLanguage::Rust,
            hash: sha256_hex(c.as_bytes()),
        })
        .collect();
    entries.sort();
    entries.dedup();
    Manifest {
        v: PROTOCOL,
        op,
        entries,
    }
}

/// Persists `KNOWN` so the parent legitimately knows its derivation.
fn db_knowing_known() -> Db {
    let db = db();
    let t = tree(&[("seed.rs", KNOWN)]);
    let store = db.store();
    let f = mindspace::ingest::confined::extract_directory(
        &StoreContext(&store),
        &anchor(&t),
        &t.cfg,
        &[db.space()],
        &Deadline::none(),
    )
    .unwrap();
    persist(&store, Extracted::Files(f), &Deadline::none()).unwrap();
    db
}

/// The context the parent sends for an operation over `contents`.
fn sent_context(db: &Db, op: Uuid, contents: &[&str]) -> Context {
    let store = db.store();
    answer(
        &StoreContext(&store),
        op,
        &ParsePolicy::default(),
        &manifest(op, contents),
    )
    .unwrap()
}

/// A producer's view of "known". `Honest` is the worker's real `KnownSet`.
type KnownFn = Box<dyn Fn(&str, &CodeParserIdentity) -> bool>;
struct Lying(KnownFn);
impl ExtractionContext for Lying {
    fn space_id(&self, _: &str) -> Result<Uuid> {
        unreachable!()
    }
    fn git_refs_current(&self, _: Uuid) -> Result<Vec<mindspace::core::GitRefObservation>> {
        Ok(vec![])
    }
    fn has_code_derivation(
        &self,
        _: CodeLanguage,
        h: &str,
        _: Option<Uuid>,
        _: Option<&str>,
        id: &CodeParserIdentity,
    ) -> Result<bool> {
        Ok((self.0)(h, id))
    }
}

/// A sink that may rewrite code-input records on their way to the parent.
struct Rewrite<'a, W: std::io::Write> {
    inner: FrameWriter<'a, W>,
    f: Box<dyn Fn(&mut Record)>,
    /// Sends every code-input record twice (a duplicated placeholder).
    twice: bool,
}
impl<W: std::io::Write> RecordSink for Rewrite<'_, W> {
    fn emit_encoded(&mut self, name: &'static str, json: Vec<u8>) -> Result<()> {
        self.inner.emit_encoded(name, json)
    }
    fn emit(&mut self, record: &Record) -> Result<()> {
        let mut r = record.clone();
        (self.f)(&mut r);
        self.inner.emit(&r)?;
        if self.twice && matches!(r, Record::CodeInput { .. }) {
            self.inner.emit(&r)?;
        }
        Ok(())
    }
    fn emit_chunk(&mut self, piece: &str) -> Result<()> {
        self.inner.emit_chunk(piece)
    }
}

fn stream(
    t: &Tree,
    op: Uuid,
    space: Uuid,
    ctx: &dyn ExtractionContext,
    rewrite: impl Fn(&mut Record) + 'static,
) -> Vec<u8> {
    stream_with(t, op, space, ctx, rewrite, false)
}

fn stream_with(
    t: &Tree,
    op: Uuid,
    space: Uuid,
    ctx: &dyn ExtractionContext,
    rewrite: impl Fn(&mut Record) + 'static,
    twice: bool,
) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = Rewrite {
            inner: FrameWriter::new(&mut out, op, None),
            f: Box::new(rewrite),
            twice,
        };
        stream_directory(
            ctx,
            &anchor(t),
            &t.cfg,
            &[space],
            &Deadline::none(),
            &ProducerLimits::default(),
            &mut w,
        )
        .unwrap();
        w.inner.finish().unwrap();
    }
    out
}

fn files(bytes: &[u8], op: Uuid, t: &Tree) -> Result<ExtractedFiles> {
    match consume(
        bytes,
        op,
        Kind::Files,
        Some(&t.root),
        &StreamLimits::default(),
        None,
    )?
    .0
    {
        Extracted::Files(f) => Ok(f),
        _ => panic!(),
    }
}

/// What the parent does: check, then persist only on success. Returns the verdict.
fn parent_accepts(
    db: &Db,
    f: ExtractedFiles,
    op: Uuid,
    space: Uuid,
    sent: &Context,
) -> std::result::Result<(), String> {
    check_result(&f, op, &[space], &ParsePolicy::default(), sent)?;
    persist(&db.store(), Extracted::Files(f), &Deadline::none()).map_err(|e| e.to_string())?;
    Ok(())
}

fn reject(db: &Db, f: ExtractedFiles, op: Uuid, sent: &Context, why: &str) {
    let before = db.rows();
    let e = parent_accepts(db, f, op, db.space(), sent).unwrap_err();
    assert!(e.contains(why), "expected `{why}` in `{e}`");
    assert_eq!(
        db.rows(),
        before,
        "a rejected operation changed the database"
    );
}

fn honest(db: &Db, t: &Tree, op: Uuid, contents: &[&str]) -> (Context, Vec<u8>) {
    let sent = sent_context(db, op, contents);
    let known = KnownSet::new(sent.clone());
    let bytes = stream(t, op, db.space(), &known, |_| {});
    (sent, bytes)
}

// ------------------------------------------------------------------ legitimate cases

#[test]
fn legitimate_reuse_and_legitimate_empty_parse_are_accepted() {
    let db = db_knowing_known();
    let t = tree(&[
        ("known.rs", KNOWN),
        ("empty.rs", EMPTY),
        ("other.rs", OTHER),
    ]);
    let op = Uuid::now_v7();
    let (sent, bytes) = honest(&db, &t, op, &[KNOWN, EMPTY, OTHER]);
    assert_eq!(sent.known.len(), 1, "only KNOWN is announced");
    let f = files(&bytes, op, &t).unwrap();
    // exactly the announced blob is a placeholder; the genuinely empty parse is not
    assert_eq!(f.reused.len(), 1);
    let reused = &f.code_inputs[*f.reused.iter().next().unwrap()];
    assert_eq!(reused.content_hash, sha256_hex(KNOWN.as_bytes()));
    let empty = f
        .code_inputs
        .iter()
        .find(|i| i.content_hash == sha256_hex(EMPTY.as_bytes()))
        .unwrap();
    assert!(empty.derivation.entities.is_empty());
    assert!(!f.reused.contains(
        &f.code_inputs
            .iter()
            .position(|i| i.content_hash == empty.content_hash)
            .unwrap()
    ));
    parent_accepts(&db, f, op, db.space(), &sent).unwrap();
}

// ------------------------------------------------------------------ hostile cases

#[test]
fn placeholder_for_an_unsent_blob_is_refused() {
    let db = db_knowing_known();
    let t = tree(&[("known.rs", KNOWN), ("other.rs", OTHER)]);
    let op = Uuid::now_v7();
    let sent = sent_context(&db, op, &[KNOWN, OTHER]);
    let cur = code::identity();
    // compromised worker: claims everything is known under the current identity
    let liar = Lying(Box::new(move |_, id| *id == cur));
    let f = files(&stream(&t, op, db.space(), &liar, |_| {}), op, &t).unwrap();
    assert_eq!(f.reused.len(), 2);
    reject(&db, f, op, &sent, "did not announce");
}

#[test]
fn placeholder_with_another_parser_identity_is_refused() {
    let db = db_knowing_known();
    let t = tree(&[("known.rs", KNOWN)]);
    let op = Uuid::now_v7();
    let sent = sent_context(&db, op, &[KNOWN]);
    // the parent announced the current identity; the worker claims a skip identity instead
    let skip = code::skip_identity(SkipReason::EntityDepth {
        limit: ParsePolicy::default().max_entity_depth,
    });
    let liar = Lying(Box::new(move |_, id| *id == skip));
    let f = files(&stream(&t, op, db.space(), &liar, |_| {}), op, &t).unwrap();
    assert_eq!(f.reused.len(), 1);
    reject(&db, f, op, &sent, "did not announce");
}

#[test]
fn placeholder_replayed_from_another_operation_or_context_is_refused() {
    // operation A's context legitimately announces KNOWN ...
    let knowing = db_knowing_known();
    let t = tree(&[("known.rs", KNOWN)]);
    let op_a = Uuid::now_v7();
    let sent_a = sent_context(&knowing, op_a, &[KNOWN]);
    assert_eq!(sent_a.known.len(), 1);
    // ... but operation B (a database that does not know it) announced nothing
    let db_b = db();
    let op_b = Uuid::now_v7();
    let sent_b = sent_context(&db_b, op_b, &[KNOWN]);
    assert!(sent_b.known.is_empty());
    let replay = KnownSet::new(sent_a.clone());
    let f = files(&stream(&t, op_b, db_b.space(), &replay, |_| {}), op_b, &t).unwrap();
    assert_eq!(f.reused.len(), 1);
    reject(&db_b, f.clone(), op_b, &sent_b, "did not announce");
    // and checking B's result against A's context fails on the operation binding
    reject(&db_b, f, op_b, &sent_a, "another operation");
    // a stream framed for A is refused by the assembler when read as B
    let bytes = stream(&t, op_a, knowing.space(), &replay, |_| {});
    assert!(files(&bytes, op_b, &t).is_err());
}

#[test]
fn conflicting_and_duplicate_placeholders_are_refused() {
    let db = db_knowing_known();
    let t = tree(&[("known.rs", KNOWN), ("other.rs", OTHER)]);
    let op = Uuid::now_v7();
    let (sent, bytes) = honest(&db, &t, op, &[KNOWN, OTHER]);
    let f = files(&bytes, op, &t).unwrap();
    // conflict: the same content once as placeholder, once as a "fresh" derivation
    let mut c = f.clone();
    let mut twin = c.code_inputs[*c.reused.iter().next().unwrap()].clone();
    twin.contexts[0].path = "/elsewhere/known.rs".into();
    c.code_inputs.push(twin);
    reject(&db, c, op, &sent, "announced as known");
    // conflict: same content, two identities
    let mut c = f.clone();
    let mut twin = c.code_inputs[*c.reused.iter().next().unwrap()].clone();
    twin.derivation.identity = code::skip_identity(SkipReason::OutputBytes {
        limit: ParsePolicy::default().max_output_bytes,
    });
    let n = c.code_inputs.len();
    c.code_inputs.push(twin);
    c.reused.insert(n);
    reject(&db, c, op, &sent, "did not announce");
    // duplicate: a reuse declaration pointing nowhere
    let mut c = f.clone();
    c.reused.insert(99);
    reject(&db, c, op, &sent, "no code input");
    // duplicate on the wire: the same placeholder record twice is refused by the assembler
    let dup = stream_with(
        &t,
        op,
        db.space(),
        &KnownSet::new(sent.clone()),
        |_| {},
        true,
    );
    let e = files(&dup, op, &t).unwrap_err().to_string();
    assert!(e.contains("duplicate"), "{e}");
}

#[test]
fn an_empty_complete_derivation_cannot_masquerade_as_reuse() {
    let db = db_knowing_known();
    let t = tree(&[("known.rs", KNOWN), ("other.rs", OTHER)]);
    let op = Uuid::now_v7();
    let (sent, bytes) = honest(&db, &t, op, &[KNOWN, OTHER]);
    let f = files(&bytes, op, &t).unwrap();
    // (a) the worker sends the placeholder shape for a *known* blob but drops the reuse flag:
    //     the shape alone is not accepted as a parse result for announced content
    let mut a = f.clone();
    a.reused.clear();
    reject(&db, a, op, &sent, "announced as known");
    // (b) the worker flags a reuse on a fresh parse that carries structure
    let mut b = f.clone();
    let other = b
        .code_inputs
        .iter()
        .position(|i| i.content_hash == sha256_hex(OTHER.as_bytes()))
        .unwrap();
    assert!(!b.code_inputs[other].derivation.entities.is_empty());
    b.reused.insert(other);
    reject(&db, b, op, &sent, "did not announce");
    // (c) on the wire: a record declared `reused` followed by structure parts is refused by the
    //     assembler itself
    let known = Lying(Box::new(|_, _| false));
    let wire = stream(&t, op, db.space(), &known, |r| {
        if let Record::CodeInput { reused, .. } = r {
            *reused = true;
        }
    });
    let e = files(&wire, op, &t).unwrap_err().to_string();
    assert!(e.contains("reused derivation"), "{e}");
}

#[test]
fn the_reuse_flag_is_explicit_and_a_missing_flag_means_not_reused() {
    let r = Record::CodeInput {
        input: mindspace::core::CodeInputDraft {
            language: CodeLanguage::Rust,
            content_hash: "0".repeat(64),
            content: String::new(),
            repository_id: None,
            blob_oid: None,
            contexts: vec![],
            derivation: code::skip_derivation(SkipReason::SourceBytes { limit: 1 }),
        },
        content: mindspace::ingest::stream::ContentSource::Shared,
        reused: true,
    };
    let j = serde_json::to_string(&r).unwrap();
    assert!(j.contains("\"reused\":true"));
    // a missing field means "not reused" (old producers), never "reused"
    let without = j.replace(",\"reused\":true", "");
    let back: Record = serde_json::from_str(&without).unwrap();
    assert!(matches!(back, Record::CodeInput { reused: false, .. }));
}

// ------------------------------------------------------------------ the residual trust boundary

#[test]
fn residual_fabricated_fresh_derivation_is_not_detectable() {
    // A compromised worker sends an *empty complete* derivation as a fresh parse of a blob the
    // parent never announced. Structurally it is indistinguishable from a legitimate parse of an
    // empty or declaration-free file; only re-parsing the content in the parent would tell. This
    // test pins that documented limit: placeholder provenance does not make worker output
    // trustworthy.
    let db = db_knowing_known();
    let t = tree(&[("other.rs", OTHER)]);
    let op = Uuid::now_v7();
    let (sent, bytes) = honest(&db, &t, op, &[OTHER]);
    let mut f = files(&bytes, op, &t).unwrap();
    assert!(f.reused.is_empty());
    f.code_inputs[0].derivation.entities.clear();
    f.code_inputs[0].derivation.imports.clear();
    f.code_inputs[0].derivation.relationships.clear();
    check_result(&f, op, &[db.space()], &ParsePolicy::default(), &sent)
        .expect("not detectable by provenance checks (residual trust boundary)");
}
