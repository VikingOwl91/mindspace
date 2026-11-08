//! Repository grants (M7.3a, ADR-030): the grant store, the request log and the
//! validation that turns stored grants into allowed roots for `mind mcp --grants`.
//!
//! What this is and is not (ADR-030, assurance level A1):
//!  - A **request** is only a line in a log. It never grants anything. The
//!    sandboxed MCP server may append requests; it cannot create grants.
//!  - A **grant** is created by the human's CLI (`mind grant add|approve`) in a
//!    store the confined server cannot write (the state directory, outside the
//!    database directory). Grants are used only at the START of `mind mcp
//!    --grants`; Landlock cannot be widened afterwards, so there is no
//!    mid-session grant and revocation takes effect at the next start.
//!  - Nothing here protects against an agent that can run `mind grant` under the
//!    human's own account (ADR-030, tier T2). The hash chain detects accidental
//!    damage and naive edits; a same-user attacker can rewrite it.
//!
//! The store is an append-only event log, one JSON object per line, each line
//! chained to the previous one by SHA-256. Any failure to verify the whole log
//! means **no grant is used** (fail closed).

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::core::{Error, Result};
use crate::mcp::Access;

pub const STORE_FILE: &str = "grants.jsonl";
pub const REQUEST_FILE: &str = "grant-requests.jsonl";

const ZERO_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_STORE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 4096;
const MAX_REQUEST_FILE_BYTES: u64 = 256 * 1024;
const MAX_PENDING_REQUESTS: usize = 50;
const MAX_REASON_CHARS: usize = 300;
const MAX_PATH_CHARS: usize = 1024;
/// Longest accepted time to live.
pub const MAX_TTL: Duration = Duration::from_secs(365 * 24 * 3600);

pub fn access_label(a: Access) -> &'static str {
    match a {
        Access::ReadOnly => "ro",
        Access::ReadWrite => "rw",
    }
}

pub fn parse_access(s: &str) -> std::result::Result<Access, String> {
    match s {
        "ro" => Ok(Access::ReadOnly),
        "rw" => Ok(Access::ReadWrite),
        other => Err(format!("access must be `ro` or `rw`, not `{other}`")),
    }
}

/// `90s`, `30m`, `12h`, `7d`; at most 365 days, at least one second.
pub fn parse_ttl(raw: &str) -> std::result::Result<Duration, String> {
    let raw = raw.trim();
    let (num, unit) = raw.split_at(raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| format!("'{raw}' is not a duration like 30m, 12h or 7d"))?;
    let secs = match unit {
        "s" => n,
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" => n.saturating_mul(86400),
        _ => return Err(format!("'{raw}' is not a duration like 30m, 12h or 7d")),
    };
    let d = Duration::from_secs(secs);
    if d.is_zero() || d > MAX_TTL {
        return Err(format!("'{raw}' is out of range; use 1s to 365d"));
    }
    Ok(d)
}

fn grant_err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Grant(msg.into()))
}

pub fn fmt_time(t: OffsetDateTime) -> String {
    t.format(&Rfc3339)
        .unwrap_or_else(|_| t.unix_timestamp().to_string())
}

fn parse_time(s: &str) -> std::result::Result<OffsetDateTime, String> {
    OffsetDateTime::parse(s, &Rfc3339).map_err(|_| format!("`{s}` is not an RFC 3339 time"))
}

/// Text that came from an agent or a file, made safe to print: control
/// characters (also escape sequences and newlines) and bidirectional overrides
/// become `?`, and the length is bounded.
pub fn sanitize_display(s: &str, max_chars: usize) -> String {
    let mut out: String = s
        .chars()
        .map(|c| {
            if c.is_control()
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c)
            {
                '?'
            } else {
                c
            }
        })
        .take(max_chars)
        .collect();
    if s.chars().count() > max_chars {
        out.push('…');
    }
    out
}

fn new_id(prefix: &str) -> String {
    let u = uuid::Uuid::now_v7().simple().to_string();
    // the last 12 hex digits are random (the first ones are the timestamp)
    format!("{prefix}-{}", &u[u.len() - 12..])
}

fn valid_id(prefix: &str, s: &str) -> bool {
    s.strip_prefix(prefix)
        .and_then(|r| r.strip_prefix('-'))
        .is_some_and(|h| {
            h.len() == 12
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

// ---------------------------------------------------------------- data model

/// One grant as recorded. `path` is canonical; `dev`/`ino` identify the directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: String,
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub access: Access,
    pub created_at: OffsetDateTime,
    pub expires_at: Option<OffsetDateTime>,
    /// The pending request this grant answers, if any.
    pub request_id: Option<String>,
    /// `cli-add` or `cli-approve`.
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    Expired,
    Revoked,
}

impl Status {
    pub fn label(&self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::Expired => "expired",
            Status::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Record {
    pub grant: Grant,
    pub revoked_at: Option<OffsetDateTime>,
    pub revoke_reason: Option<String>,
    /// The last reason a start refused to use this grant, if any.
    pub last_skip: Option<String>,
}

impl Record {
    pub fn status(&self, now: OffsetDateTime) -> Status {
        if self.revoked_at.is_some() {
            Status::Revoked
        } else if self.grant.expires_at.is_some_and(|e| now >= e) {
            Status::Expired
        } else {
            Status::Active
        }
    }
}

/// The verified result of reading the whole store.
#[derive(Debug, Clone, Default)]
pub struct Replay {
    pub records: Vec<Record>,
    pub denied_requests: BTreeSet<String>,
    pub events: u64,
    last_hash: String,
}

impl Replay {
    pub fn find(&self, id: &str) -> Option<&Record> {
        self.records.iter().find(|r| r.grant.id == id)
    }

    pub fn request_is_closed(&self, request_id: &str) -> bool {
        self.denied_requests.contains(request_id)
            || self
                .records
                .iter()
                .any(|r| r.grant.request_id.as_deref() == Some(request_id))
    }
}

// ------------------------------------------------------------ event encoding

fn canonical_json(m: &Map<String, Value>) -> String {
    let sorted: BTreeMap<&String, &Value> = m.iter().collect();
    serde_json::to_string(&sorted).unwrap_or_default()
}

fn hash_of(m: &Map<String, Value>) -> String {
    let mut h = Sha256::new();
    h.update(canonical_json(m).as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn str_field<'a>(m: &'a Map<String, Value>, k: &str) -> std::result::Result<&'a str, String> {
    m.get(k)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("field `{k}` is missing or not a string"))
}

fn opt_str_field<'a>(
    m: &'a Map<String, Value>,
    k: &str,
) -> std::result::Result<Option<&'a str>, String> {
    match m.get(k) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("field `{k}` is not a string")),
    }
}

fn u64_field(m: &Map<String, Value>, k: &str) -> std::result::Result<u64, String> {
    m.get(k)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("field `{k}` is missing or not an unsigned integer"))
}

fn only_keys(m: &Map<String, Value>, allowed: &[&str]) -> std::result::Result<(), String> {
    for k in m.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(format!("unexpected field `{k}`"));
        }
    }
    Ok(())
}

fn short_reason(m: &Map<String, Value>) -> std::result::Result<Option<String>, String> {
    match opt_str_field(m, "reason")? {
        Some(r) if r.chars().count() > 200 => Err("reason is longer than 200 characters".into()),
        other => Ok(other.map(str::to_string)),
    }
}

/// A path that may be a root: absolute, no NUL, no `:` (it would be read as a
/// mode separator in `--allow-root`), bounded.
fn plausible_root_text(p: &str) -> std::result::Result<(), String> {
    if !p.starts_with('/') {
        Err("path is not absolute".into())
    } else if p.contains(':') || p.contains('\0') {
        Err("path contains `:` or NUL".into())
    } else if p.chars().count() > MAX_PATH_CHARS {
        Err("path is too long".into())
    } else {
        Ok(())
    }
}

/// Verifies the chain and every event and returns the resulting state.
/// Strict: every byte must belong to a complete, verifying event.
pub fn replay(text: &str) -> std::result::Result<Replay, String> {
    let mut out = Replay {
        last_hash: ZERO_HASH.to_string(),
        ..Default::default()
    };
    if text.is_empty() {
        return Ok(out);
    }
    if !text.ends_with('\n') {
        return Err("the last line is incomplete (no newline)".into());
    }
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        let fail = |why: String| format!("line {n}: {why}");
        if line.len() > MAX_LINE_BYTES {
            return Err(fail("line is too long".into()));
        }
        let v: Value = serde_json::from_str(line).map_err(|e| fail(format!("not JSON ({e})")))?;
        let Value::Object(mut m) = v else {
            return Err(fail("not an object".into()));
        };
        let claimed = match m.remove("hash") {
            Some(Value::String(h)) => h,
            _ => return Err(fail("missing hash".into())),
        };
        if hash_of(&m) != claimed {
            return Err(fail("hash mismatch (the line was changed)".into()));
        }
        if str_field(&m, "prev").map_err(&fail)? != out.last_hash {
            return Err(fail(
                "chain broken (a line was removed, inserted or reordered)".into(),
            ));
        }
        if u64_field(&m, "seq").map_err(&fail)? != n as u64 {
            return Err(fail("sequence number out of order".into()));
        }
        let ts = parse_time(str_field(&m, "ts").map_err(&fail)?).map_err(&fail)?;
        let kind = str_field(&m, "kind").map_err(&fail)?.to_string();
        let base = ["seq", "ts", "prev", "kind"];
        match kind.as_str() {
            "granted" => {
                only_keys(
                    &m,
                    &[
                        &base[..],
                        &[
                            "id",
                            "path",
                            "dev",
                            "ino",
                            "access",
                            "created_at",
                            "expires_at",
                            "request_id",
                            "source",
                        ],
                    ]
                    .concat(),
                )
                .map_err(&fail)?;
                let id = str_field(&m, "id").map_err(&fail)?;
                if !valid_id("g", id) {
                    return Err(fail(format!("bad grant id `{id}`")));
                }
                if out.find(id).is_some() {
                    return Err(fail(format!("duplicate grant id `{id}`")));
                }
                let path = str_field(&m, "path").map_err(&fail)?;
                plausible_root_text(path).map_err(&fail)?;
                let access =
                    parse_access(str_field(&m, "access").map_err(&fail)?).map_err(&fail)?;
                let created_at =
                    parse_time(str_field(&m, "created_at").map_err(&fail)?).map_err(&fail)?;
                let expires_at = match opt_str_field(&m, "expires_at").map_err(&fail)? {
                    Some(e) => {
                        let e = parse_time(e).map_err(&fail)?;
                        if e <= created_at {
                            return Err(fail("expiry is not after creation".into()));
                        }
                        Some(e)
                    }
                    None => None,
                };
                let request_id = match opt_str_field(&m, "request_id").map_err(&fail)? {
                    Some(r) if valid_id("r", r) => Some(r.to_string()),
                    Some(r) => return Err(fail(format!("bad request id `{r}`"))),
                    None => None,
                };
                let source = str_field(&m, "source").map_err(&fail)?;
                if source != "cli-add" && source != "cli-approve" {
                    return Err(fail(format!("unknown source `{source}`")));
                }
                out.records.push(Record {
                    grant: Grant {
                        id: id.to_string(),
                        path: PathBuf::from(path),
                        dev: u64_field(&m, "dev").map_err(&fail)?,
                        ino: u64_field(&m, "ino").map_err(&fail)?,
                        access,
                        created_at,
                        expires_at,
                        request_id,
                        source: source.to_string(),
                    },
                    revoked_at: None,
                    revoke_reason: None,
                    last_skip: None,
                });
            }
            "revoked" => {
                only_keys(&m, &[&base[..], &["id", "reason"]].concat()).map_err(&fail)?;
                let id = str_field(&m, "id").map_err(&fail)?;
                let reason = short_reason(&m).map_err(&fail)?;
                let rec = out
                    .records
                    .iter_mut()
                    .find(|r| r.grant.id == id)
                    .ok_or_else(|| fail(format!("revokes the unknown grant `{id}`")))?;
                if rec.revoked_at.is_some() {
                    return Err(fail(format!("grant `{id}` is revoked twice")));
                }
                rec.revoked_at = Some(ts);
                rec.revoke_reason = reason;
            }
            "request_denied" => {
                only_keys(&m, &[&base[..], &["request_id", "reason"]].concat()).map_err(&fail)?;
                let r = str_field(&m, "request_id").map_err(&fail)?;
                if !valid_id("r", r) {
                    return Err(fail(format!("bad request id `{r}`")));
                }
                short_reason(&m).map_err(&fail)?;
                out.denied_requests.insert(r.to_string());
            }
            "skipped" => {
                only_keys(&m, &[&base[..], &["id", "reason"]].concat()).map_err(&fail)?;
                let id = str_field(&m, "id").map_err(&fail)?;
                let reason = short_reason(&m).map_err(&fail)?;
                let rec = out
                    .records
                    .iter_mut()
                    .find(|r| r.grant.id == id)
                    .ok_or_else(|| fail(format!("refers to the unknown grant `{id}`")))?;
                rec.last_skip = reason;
            }
            other => return Err(fail(format!("unknown event kind `{other}`"))),
        }
        out.events += 1;
        out.last_hash = claimed;
    }
    Ok(out)
}

/// Reads a log that may end in a line a crashed writer left unfinished (no
/// final newline). That tail is not an event, it never completed, so it is
/// ignored: the result is the verified state before the interrupted write.
/// Everything before it must verify. Returns the state, the byte length of the
/// verified prefix and whether a tail was ignored. A complete line that does not
/// verify is NOT a torn tail: it is an error.
pub fn replay_tolerant(text: &str) -> std::result::Result<(Replay, usize, bool), String> {
    let (good, torn) = match text.rfind('\n') {
        Some(i) if i + 1 < text.len() => (&text[..=i], true),
        Some(_) => (text, false),
        None if text.is_empty() => ("", false),
        None => ("", true),
    };
    Ok((replay(good)?, good.len(), torn))
}

// -------------------------------------------------------------------- store

/// The grant store: `<dir>/grants.jsonl`, mode 0600, directory 0700.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

fn io_err(path: &Path, what: &str, e: std::io::Error) -> Error {
    Error::Grant(format!("{what} {}: {e}", path.display()))
}

fn euid() -> u32 {
    // SAFETY: geteuid has no arguments and cannot fail.
    unsafe { libc::geteuid() }
}

/// Opens the store DIRECTORY without following a symlink at its last component.
fn open_dir_nofollow(dir: &Path) -> std::io::Result<std::os::fd::OwnedFd> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: open with a valid NUL-terminated path.
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: just returned by the kernel.
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
}

/// Opens `name` RELATIVE to the already opened directory, never following a
/// symlink, so a swap of the path after the directory was checked changes nothing.
fn openat_nofollow(
    dirfd: &std::os::fd::OwnedFd,
    name: &str,
    flags: i32,
    mode: u32,
) -> std::io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let c = std::ffi::CString::new(name)
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: openat on a descriptor we own with a valid NUL-terminated name.
    let fd = unsafe {
        libc::openat(
            dirfd.as_raw_fd(),
            c.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            mode,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: just returned by the kernel.
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl Store {
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn file(&self) -> PathBuf {
        self.dir.join(STORE_FILE)
    }

    /// The directory descriptor, checked on the descriptor itself: owned by us,
    /// not writable by group or others, not a symlink. `None`: it does not exist.
    fn open_dir(&self, create: bool) -> Result<Option<std::os::fd::OwnedFd>> {
        use std::os::unix::fs::MetadataExt;
        if create && std::fs::symlink_metadata(&self.dir).is_err() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&self.dir)
                .map_err(|e| io_err(&self.dir, "cannot create", e))?;
        }
        let fd = match open_dir_nofollow(&self.dir) {
            Ok(fd) => fd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return grant_err(format!(
                    "cannot open the grant store directory {} (is it a symlink?): {e}",
                    self.dir.display()
                ))
            }
        };
        let meta = File::from(
            fd.try_clone()
                .map_err(|e| io_err(&self.dir, "cannot use", e))?,
        )
        .metadata()
        .map_err(|e| io_err(&self.dir, "cannot inspect", e))?;
        if meta.uid() != euid() {
            return grant_err(format!(
                "the grant store directory {} is not owned by you; it is not used",
                self.dir.display()
            ));
        }
        if meta.mode() & 0o022 != 0 {
            return grant_err(format!(
                "the grant store directory {} is writable by group or others; it is not used (chmod 700)",
                self.dir.display()
            ));
        }
        Ok(Some(fd))
    }

    /// Checks the OPENED file, not the path: a regular file, owned by us, one
    /// link (a second name would be a way to reach it from elsewhere), not
    /// writable by group or others, bounded.
    fn check_open_file(&self, f: &File) -> Result<()> {
        use std::os::unix::fs::MetadataExt;
        let path = self.file();
        let meta = f
            .metadata()
            .map_err(|e| io_err(&path, "cannot inspect", e))?;
        if !meta.file_type().is_file() {
            return grant_err(format!(
                "{} is not a regular file; it is not used",
                path.display()
            ));
        }
        if meta.uid() != euid() {
            return grant_err(format!(
                "{} is not owned by you; it is not used",
                path.display()
            ));
        }
        if meta.nlink() != 1 {
            return grant_err(format!(
                "{} has more than one hard link; it is not used",
                path.display()
            ));
        }
        if meta.mode() & 0o022 != 0 {
            return grant_err(format!(
                "{} is writable by group or others; it is not used (chmod 600)",
                path.display()
            ));
        }
        if meta.len() > MAX_STORE_BYTES {
            return grant_err(format!(
                "{} is larger than {MAX_STORE_BYTES} bytes; it is not used",
                path.display()
            ));
        }
        Ok(())
    }

    /// Reads and verifies the store under a shared lock. A missing file is an
    /// empty store; an unfinished last line (a crashed writer) is ignored. A
    /// store that does not verify, is a symlink, has several hard links, is not
    /// yours or is writable by group/others is an ERROR: no grant from it may be used.
    pub fn load(&self) -> Result<Replay> {
        let path = self.file();
        let Some(dirfd) = self.open_dir(false)? else {
            return Ok(Replay::default());
        };
        let mut file = match openat_nofollow(&dirfd, STORE_FILE, libc::O_RDONLY, 0) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Replay::default()),
            Err(e) => return Err(io_err(&path, "cannot open the grant store", e)),
        };
        self.check_open_file(&file)?;
        file.lock_shared()
            .map_err(|e| io_err(&path, "cannot lock", e))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| io_err(&path, "cannot read", e))?;
        let text = String::from_utf8(bytes).map_err(|_| {
            Error::Grant(format!(
                "the grant store {} is not valid UTF-8; no grant from it is used",
                path.display()
            ))
        })?;
        replay_tolerant(&text).map(|(r, _, _)| r).map_err(|why| {
            Error::Grant(format!(
                "the grant store {} does not verify ({why}); no grant from it is used. \
                 If you did not edit it, treat this as tampering; move the file away to start a new store",
                path.display()
            ))
        })
    }

    /// Appends one event under an exclusive lock. `check` sees the verified
    /// state and may refuse. A tail left by a crashed writer is removed first; a
    /// failed write removes its own partial line. Returns the new chain head.
    pub fn append(
        &self,
        kind: &str,
        fields: &[(&str, Value)],
        now: OffsetDateTime,
        check: impl FnOnce(&Replay) -> Result<()>,
    ) -> Result<String> {
        let path = self.file();
        let dirfd = self
            .open_dir(true)?
            .ok_or_else(|| Error::Grant("the grant store directory vanished".into()))?;
        let mut file = openat_nofollow(
            &dirfd,
            STORE_FILE,
            libc::O_RDWR | libc::O_APPEND | libc::O_CREAT,
            0o600,
        )
        .map_err(|e| io_err(&path, "cannot open", e))?;
        self.check_open_file(&file)?;
        file.lock().map_err(|e| io_err(&path, "cannot lock", e))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| io_err(&path, "cannot read", e))?;
        let text = String::from_utf8(bytes).map_err(|_| {
            Error::Grant(format!(
                "{} is not valid UTF-8; nothing was written",
                path.display()
            ))
        })?;
        let (state, good_len, torn) = replay_tolerant(&text).map_err(|why| {
            Error::Grant(format!(
                "the grant store {} does not verify ({why}); nothing was written",
                path.display()
            ))
        })?;
        if torn {
            file.set_len(good_len as u64)
                .map_err(|e| io_err(&path, "cannot repair the unfinished last line of", e))?;
        }
        check(&state)?;
        let mut m = Map::new();
        m.insert("seq".into(), json!(state.events + 1));
        m.insert("ts".into(), json!(fmt_time(now)));
        m.insert("prev".into(), json!(state.last_hash));
        m.insert("kind".into(), json!(kind));
        for (k, v) in fields {
            m.insert((*k).to_string(), v.clone());
        }
        let hash = hash_of(&m);
        m.insert("hash".into(), json!(hash));
        let mut line = serde_json::to_string(&Value::Object(m))
            .map_err(|e| Error::Grant(format!("cannot encode the event: {e}")))?;
        if line.len() >= MAX_LINE_BYTES {
            return grant_err("the event is too large");
        }
        line.push('\n');
        let written = file
            .write_all(line.as_bytes())
            .and_then(|_| file.sync_all());
        if let Err(e) = written {
            // Do not leave half a line behind (a full disk, a file size limit).
            let _ = file.set_len(good_len as u64);
            return Err(io_err(&path, "cannot write", e));
        }
        Ok(hash)
    }
}

// ------------------------------------------------------------ path validation

/// Directories a granted (or any) root must not contain or lie inside: the
/// confined server can WRITE a read-write root, so a root over the grant store
/// or the database directory would let it forge grants or damage its own state.
#[derive(Debug, Clone, Default)]
pub struct PathPolicy {
    /// (directory, label): the root may neither contain nor lie inside it.
    pub overlap_forbidden: Vec<(PathBuf, &'static str)>,
    /// The root may not equal or contain these (the home directory).
    pub containment_forbidden: Vec<(PathBuf, &'static str)>,
}

impl PathPolicy {
    pub fn standard(store_dir: &Path, db_dir: Option<&Path>, home: Option<&Path>) -> Self {
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let mut overlap = vec![(canon(store_dir), "the grant store directory")];
        if let Some(d) = db_dir {
            overlap.push((canon(d), "the database directory"));
        }
        let contain = home
            .map(|h| vec![(canon(h), "your home directory")])
            .unwrap_or_default();
        Self {
            overlap_forbidden: overlap,
            containment_forbidden: contain,
        }
    }
}

/// A directory that passed validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidRoot {
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
}

/// Canonicalizes `path` and applies every rule. The error text is for the human.
pub fn validate_root(path: &Path, policy: &PathPolicy) -> std::result::Result<ValidRoot, String> {
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = canonical
        .to_str()
        .ok_or_else(|| "the path is not valid UTF-8".to_string())?;
    plausible_root_text(text)?;
    let meta =
        std::fs::metadata(&canonical).map_err(|e| format!("{}: {e}", canonical.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    if canonical.parent().is_none() {
        return Err("refusing the filesystem root; name a specific directory".into());
    }
    for (dir, label) in &policy.overlap_forbidden {
        if canonical.starts_with(dir) || dir.starts_with(&canonical) {
            return Err(format!(
                "{} overlaps {label} ({}); a root there would let the sandboxed server change it",
                canonical.display(),
                dir.display()
            ));
        }
    }
    for (dir, label) in &policy.containment_forbidden {
        if dir.starts_with(&canonical) {
            return Err(format!(
                "{} equals or contains {label} ({}); name a specific project directory",
                canonical.display(),
                dir.display()
            ));
        }
    }
    Ok(ValidRoot {
        dev: meta.dev(),
        ino: meta.ino(),
        path: canonical,
    })
}

// ------------------------------------------------------------- operations

pub struct NewGrant<'a> {
    pub path: &'a Path,
    pub access: Access,
    pub ttl: Option<Duration>,
    pub request_id: Option<&'a str>,
    pub source: &'a str,
}

/// Validates and records a grant. Returns it as stored.
pub fn add_grant(
    store: &Store,
    policy: &PathPolicy,
    new: &NewGrant<'_>,
    now: OffsetDateTime,
) -> Result<Grant> {
    let root = validate_root(new.path, policy).map_err(Error::Grant)?;
    let expires_at = match new.ttl {
        Some(t) if t.is_zero() || t > MAX_TTL => {
            return grant_err("the time to live must be 1s to 365d")
        }
        Some(t) => Some(now + t),
        None => None,
    };
    let id = new_id("g");
    let grant = Grant {
        id: id.clone(),
        path: root.path.clone(),
        dev: root.dev,
        ino: root.ino,
        access: new.access,
        created_at: now,
        expires_at,
        request_id: new.request_id.map(str::to_string),
        source: new.source.to_string(),
    };
    let mut fields = vec![
        ("id", json!(grant.id)),
        ("path", json!(root.path.to_string_lossy())),
        ("dev", json!(root.dev)),
        ("ino", json!(root.ino)),
        ("access", json!(access_label(grant.access))),
        ("created_at", json!(fmt_time(now))),
        ("source", json!(grant.source)),
    ];
    if let Some(e) = expires_at {
        fields.push(("expires_at", json!(fmt_time(e))));
    }
    if let Some(r) = new.request_id {
        fields.push(("request_id", json!(r)));
    }
    let req = new.request_id.map(str::to_string);
    store.append("granted", &fields, now, |state| {
        if let Some(r) = &req {
            if state.request_is_closed(r) {
                return grant_err(format!("request {r} was already answered"));
            }
        }
        if state.find(&id).is_some() {
            return grant_err("grant id collision; try again");
        }
        Ok(())
    })?;
    Ok(grant)
}

pub fn revoke_grant(
    store: &Store,
    id: &str,
    reason: Option<&str>,
    now: OffsetDateTime,
) -> Result<()> {
    if !valid_id("g", id) {
        return grant_err(format!("`{id}` is not a grant id (like g-0123456789ab)"));
    }
    let mut fields = vec![("id", json!(id))];
    if let Some(r) = reason {
        if r.chars().count() > 200 {
            return grant_err("the reason is longer than 200 characters");
        }
        fields.push(("reason", json!(r)));
    }
    store
        .append("revoked", &fields, now, |state| match state.find(id) {
            None => grant_err(format!("no grant {id}")),
            Some(r) if r.revoked_at.is_some() => {
                grant_err(format!("grant {id} is already revoked"))
            }
            Some(_) => Ok(()),
        })
        .map(|_| ())
}

pub fn deny_request(store: &Store, request_id: &str, now: OffsetDateTime) -> Result<()> {
    if !valid_id("r", request_id) {
        return grant_err(format!(
            "`{request_id}` is not a request id (like r-0123456789ab)"
        ));
    }
    store
        .append(
            "request_denied",
            &[("request_id", json!(request_id))],
            now,
            |state| {
                if state.request_is_closed(request_id) {
                    grant_err(format!("request {request_id} was already answered"))
                } else {
                    Ok(())
                }
            },
        )
        .map(|_| ())
}

// -------------------------------------------------------- use at server start

/// What `mind mcp --grants` gets from the store.
#[derive(Debug, Default)]
pub struct Resolution {
    /// Validated roots to add to the allowed roots.
    pub roots: Vec<(PathBuf, Access)>,
    /// The device and inode that were APPROVED for each root. The sandbox
    /// creates its rule from a descriptor checked against these.
    pub identities: BTreeMap<PathBuf, (u64, u64)>,
    pub applied: Vec<String>,
    /// Active grants that failed validation now, with the reason.
    pub skipped: Vec<(String, String)>,
    /// The store could not be verified: no grant is used.
    pub store_error: Option<String>,
}

/// Re-validates every active grant against the file system as it is NOW.
pub fn resolve_for_start(store: &Store, policy: &PathPolicy, now: OffsetDateTime) -> Resolution {
    let state = match store.load() {
        Ok(s) => s,
        Err(e) => {
            return Resolution {
                store_error: Some(e.to_string()),
                ..Default::default()
            }
        }
    };
    let mut res = Resolution::default();
    for rec in &state.records {
        if rec.status(now) != Status::Active {
            continue;
        }
        let g = &rec.grant;
        match validate_root(&g.path, policy) {
            Err(why) => res.skipped.push((g.id.clone(), why)),
            Ok(v) if v.path != g.path => res.skipped.push((
                g.id.clone(),
                format!(
                    "the path now resolves to {} (a symlink or mount changed)",
                    v.path.display()
                ),
            )),
            Ok(v) if v.dev != g.dev || v.ino != g.ino => res.skipped.push((
                g.id.clone(),
                "the directory is not the one that was approved (device/inode changed)".into(),
            )),
            Ok(v) => {
                res.identities.insert(v.path.clone(), (g.dev, g.ino));
                res.roots.push((v.path, g.access));
                res.applied.push(g.id.clone());
            }
        }
    }
    res
}

/// Records, once per distinct reason, that a start refused an active grant.
/// Best effort: a failure here never changes what is used.
pub fn record_skips(store: &Store, res: &Resolution, now: OffsetDateTime) {
    for (id, why) in &res.skipped {
        let why = sanitize_display(why, 190);
        let _ = store.append(
            "skipped",
            &[("id", json!(id)), ("reason", json!(why))],
            now,
            |state| match state.find(id).and_then(|r| r.last_skip.as_deref()) {
                Some(last) if last == why => grant_err("already recorded"),
                _ => Ok(()),
            },
        );
    }
}

// ----------------------------------------------------------- request log

/// A pending access request as written by the (untrusted) server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub ts: String,
    pub path: String,
    pub access: Access,
    pub reason: Option<String>,
}

fn parse_request(line: &str) -> Option<Request> {
    let v: Value = serde_json::from_str(line).ok()?;
    let m = v.as_object()?;
    only_keys(m, &["id", "ts", "path", "access", "reason"]).ok()?;
    let id = m.get("id")?.as_str()?;
    if !valid_id("r", id) {
        return None;
    }
    let path = m.get("path")?.as_str()?;
    if path.chars().count() > MAX_PATH_CHARS {
        return None;
    }
    Some(Request {
        id: id.to_string(),
        ts: m.get("ts")?.as_str()?.chars().take(40).collect(),
        path: path.to_string(),
        access: parse_access(m.get("access")?.as_str()?).ok()?,
        reason: m
            .get("reason")
            .and_then(Value::as_str)
            .map(|r| r.chars().take(MAX_REASON_CHARS).collect()),
    })
}

/// Reads the request log. Unreadable or malformed lines are counted, not trusted.
pub fn read_requests(file: &Path) -> (Vec<Request>, usize) {
    let Ok(meta) = std::fs::symlink_metadata(file) else {
        return (Vec::new(), 0);
    };
    if !meta.file_type().is_file() || meta.len() > MAX_REQUEST_FILE_BYTES * 4 {
        return (Vec::new(), 1);
    }
    let Ok(bytes) = std::fs::read(file) else {
        return (Vec::new(), 1);
    };
    // Damaged bytes cost the lines they touch, not the whole log.
    let text = String::from_utf8_lossy(&bytes);
    let (mut ok, mut bad) = (Vec::new(), 0);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match parse_request(line) {
            Some(r) if !ok.iter().any(|x: &Request| x.id == r.id) => ok.push(r),
            _ => bad += 1,
        }
    }
    (ok, bad)
}

/// Appends a request. Called by the sandboxed MCP server: it validates the
/// input as untrusted, bounds the log and never reads or writes the grant store.
/// A request already on file for the same path and access is returned, not
/// duplicated.
pub fn append_request(
    file: &Path,
    path: &str,
    access: Access,
    reason: Option<&str>,
    now: OffsetDateTime,
) -> Result<(String, bool)> {
    if path.is_empty() || !path.starts_with('/') {
        return grant_err("path must be an absolute path");
    }
    if path.chars().count() > MAX_PATH_CHARS || path.chars().any(char::is_control) {
        return grant_err("path is too long or contains control characters");
    }
    let reason: Option<String> = reason
        .map(|r| {
            r.chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(MAX_REASON_CHARS)
                .collect::<String>()
        })
        .filter(|r| !r.trim().is_empty());
    if let Ok(m) = std::fs::symlink_metadata(file) {
        if !m.file_type().is_file() {
            return grant_err("the request log is not a regular file");
        }
        if m.len() > MAX_REQUEST_FILE_BYTES {
            return grant_err(
                "too many access requests are waiting; ask the user to run `mind grant requests`",
            );
        }
    }
    let (existing, _) = read_requests(file);
    if let Some(r) = existing
        .iter()
        .find(|r| r.path == path && r.access == access)
    {
        return Ok((r.id.clone(), false));
    }
    if existing.len() >= MAX_PENDING_REQUESTS {
        return grant_err(
            "too many access requests are waiting; ask the user to run `mind grant requests`",
        );
    }
    let id = new_id("r");
    let mut m = Map::new();
    m.insert("id".into(), json!(id));
    m.insert("ts".into(), json!(fmt_time(now)));
    m.insert("path".into(), json!(path));
    m.insert("access".into(), json!(access_label(access)));
    if let Some(r) = reason {
        m.insert("reason".into(), json!(r));
    }
    let mut line = serde_json::to_string(&Value::Object(m))
        .map_err(|e| Error::Grant(format!("cannot encode the request: {e}")))?;
    line.push('\n');
    let mut f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(file)
        .map_err(|e| Error::Grant(format!("cannot record the request: {e}")))?;
    // A crashed writer or damage may have left the last line unfinished: start
    // on a fresh line so this request is not glued to it.
    if let Ok(meta) = f.metadata() {
        if meta.len() > 0 {
            let mut last = [0u8; 1];
            if std::os::unix::fs::FileExt::read_at(&f, &mut last, meta.len() - 1).is_ok()
                && last[0] != b'\n'
            {
                line.insert(0, '\n');
            }
        }
    }
    f.write_all(line.as_bytes())
        .map_err(|e| Error::Grant(format!("cannot record the request: {e}")))?;
    Ok((id, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap()
    }

    fn policy_for(store: &Path) -> PathPolicy {
        PathPolicy::standard(store, None, None)
    }

    fn setup() -> (tempfile::TempDir, Store, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let store = Store::in_dir(t.path().join("state"));
        let repo = t.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        (t, store, repo)
    }

    fn add(store: &Store, repo: &Path, access: Access, ttl: Option<Duration>) -> Grant {
        add_grant(
            store,
            &policy_for(store.dir()),
            &NewGrant {
                path: repo,
                access,
                ttl,
                request_id: None,
                source: "cli-add",
            },
            now(),
        )
        .unwrap()
    }

    #[test]
    fn ttl_parsing() {
        assert_eq!(parse_ttl("90s").unwrap(), Duration::from_secs(90));
        assert_eq!(parse_ttl("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse_ttl("12h").unwrap(), Duration::from_secs(43200));
        assert_eq!(parse_ttl("7d").unwrap(), Duration::from_secs(604800));
        for bad in ["", "0s", "10", "d", "1y", "-5m", "366d", "1.5h", "5 m x"] {
            assert!(parse_ttl(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn add_list_revoke_roundtrip_with_a_verifying_chain() {
        let (_t, store, repo) = setup();
        let g = add(&store, &repo, Access::ReadWrite, None);
        assert!(valid_id("g", &g.id));
        let state = store.load().unwrap();
        assert_eq!(state.records.len(), 1);
        assert_eq!(state.records[0].status(now()), Status::Active);
        assert_eq!(state.records[0].grant.access, Access::ReadWrite);
        revoke_grant(&store, &g.id, Some("done"), now()).unwrap();
        let state = store.load().unwrap();
        assert_eq!(state.records[0].status(now()), Status::Revoked);
        assert_eq!(state.records[0].revoke_reason.as_deref(), Some("done"));
        assert!(revoke_grant(&store, &g.id, None, now()).is_err(), "twice");
        assert!(
            revoke_grant(&store, "g-000000000000", None, now()).is_err(),
            "unknown"
        );
        let mode = std::fs::metadata(store.file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let dmode = std::fs::metadata(store.dir()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dmode, 0o700);
    }

    #[test]
    fn ro_and_rw_are_kept_apart_and_a_ttl_expires() {
        let (_t, store, repo) = setup();
        let ro = add(
            &store,
            &repo,
            Access::ReadOnly,
            Some(Duration::from_secs(60)),
        );
        let state = store.load().unwrap();
        let rec = state.find(&ro.id).unwrap();
        assert_eq!(rec.grant.access, Access::ReadOnly);
        assert_eq!(rec.status(now()), Status::Active);
        assert_eq!(rec.status(now() + Duration::from_secs(59)), Status::Active);
        assert_eq!(rec.status(now() + Duration::from_secs(60)), Status::Expired);
        assert_eq!(
            rec.status(now() + Duration::from_secs(3600)),
            Status::Expired
        );
    }

    #[test]
    fn every_kind_of_manipulation_is_detected() {
        let (_t, store, repo) = setup();
        let g = add(&store, &repo, Access::ReadOnly, None);
        add(&store, &repo, Access::ReadOnly, None);
        revoke_grant(&store, &g.id, None, now()).unwrap();
        let text = std::fs::read_to_string(store.file()).unwrap();
        assert!(replay(&text).is_ok());
        let lines: Vec<&str> = text.lines().collect();
        let join = |l: &[&str]| l.iter().map(|s| format!("{s}\n")).collect::<String>();
        let cases: Vec<(&str, String)> = vec![
            (
                "edited access",
                text.replacen("\"access\":\"ro\"", "\"access\":\"rw\"", 1),
            ),
            ("edited path", text.replacen("/repo", "/rep0", 1)),
            ("removed middle line", join(&[lines[0], lines[2]])),
            ("removed first line", join(&lines[1..])),
            ("reordered", join(&[lines[1], lines[0], lines[2]])),
            (
                "duplicated line",
                join(&[lines[0], lines[0], lines[1], lines[2]]),
            ),
            ("truncated line", text[..text.len() - 10].to_string()),
            ("no final newline", text.trim_end().to_string()),
            ("garbage line", format!("{text}not json\n")),
            (
                "blank line",
                format!("{}\n\n{}", lines[0], join(&lines[1..])),
            ),
            (
                "extra field",
                text.replacen("\"kind\":\"granted\"", "\"kind\":\"granted\",\"x\":1", 1),
            ),
            (
                "unknown kind",
                text.replacen("\"kind\":\"revoked\"", "\"kind\":\"approved\"", 1),
            ),
        ];
        for (name, bad) in cases {
            assert!(replay(&bad).is_err(), "not detected: {name}");
        }
    }

    #[test]
    fn a_self_signed_forgery_without_the_chain_is_not_accepted() {
        // A line with a correct hash of its own content but a wrong `prev`.
        let mut m = Map::new();
        m.insert("seq".into(), json!(1));
        m.insert("ts".into(), json!("2026-11-08T10:00:00Z"));
        m.insert("prev".into(), json!("f".repeat(64)));
        m.insert("kind".into(), json!("granted"));
        m.insert("id".into(), json!("g-0123456789ab"));
        m.insert("path".into(), json!("/tmp"));
        m.insert("dev".into(), json!(1));
        m.insert("ino".into(), json!(2));
        m.insert("access".into(), json!("rw"));
        m.insert("created_at".into(), json!("2026-11-08T10:00:00Z"));
        m.insert("source".into(), json!("cli-add"));
        let h = hash_of(&m);
        m.insert("hash".into(), json!(h));
        let line = format!("{}\n", serde_json::to_string(&Value::Object(m)).unwrap());
        assert!(replay(&line).unwrap_err().contains("chain broken"));
    }

    #[test]
    fn invalid_fields_are_rejected_even_with_a_valid_hash() {
        // build correctly chained lines with bad content
        let mk = |fields: Vec<(&str, Value)>| -> String {
            let mut m = Map::new();
            m.insert("seq".into(), json!(1));
            m.insert("ts".into(), json!("2026-11-08T10:00:00Z"));
            m.insert("prev".into(), json!(ZERO_HASH));
            m.insert("kind".into(), json!("granted"));
            for (k, v) in fields {
                m.insert(k.into(), v);
            }
            let h = hash_of(&m);
            m.insert("hash".into(), json!(h));
            format!("{}\n", serde_json::to_string(&Value::Object(m)).unwrap())
        };
        let ok = || {
            vec![
                ("id", json!("g-0123456789ab")),
                ("path", json!("/tmp/x")),
                ("dev", json!(1)),
                ("ino", json!(2)),
                ("access", json!("ro")),
                ("created_at", json!("2026-11-08T10:00:00Z")),
                ("source", json!("cli-add")),
            ]
        };
        assert!(replay(&mk(ok())).is_ok());
        let with = |k: &str, v: Value| {
            let mut f = ok();
            f.retain(|(n, _)| *n != k);
            f.push((k, v));
            replay(&mk(f))
        };
        assert!(with("id", json!("G-0123456789AB")).is_err());
        assert!(with("id", json!("g-short")).is_err());
        assert!(with("path", json!("relative/dir")).is_err());
        assert!(
            with("path", json!("/a:rw:/etc")).is_err(),
            "`:` smuggles a second root"
        );
        assert!(with("access", json!("admin")).is_err());
        assert!(
            with("source", json!("mcp")).is_err(),
            "a grant can only come from the CLI"
        );
        assert!(with("created_at", json!("yesterday")).is_err());
        assert!(with("dev", json!("1")).is_err());
        assert!(
            with("expires_at", json!("2020-01-01T00:00:00Z")).is_err(),
            "before creation"
        );
    }

    #[test]
    fn paths_that_would_let_the_server_forge_grants_or_leak_the_home_are_refused() {
        let t = tempfile::tempdir().unwrap();
        let home = t.path().join("home");
        let state = home.join(".local/state/mindspace");
        let db_dir = home.join(".local/share/mindspace");
        let project = home.join("project");
        for d in [&state, &db_dir, &project] {
            std::fs::create_dir_all(d).unwrap();
        }
        let pol = PathPolicy::standard(&state, Some(&db_dir), Some(&home));
        assert!(validate_root(&project, &pol).is_ok());
        for (bad, what) in [
            (home.as_path(), "home"),
            (home.join(".local").as_path(), "contains the store"),
            (state.as_path(), "the store"),
            (state.join("sub").as_path(), "inside the store"),
            (db_dir.as_path(), "the database directory"),
            (Path::new("/"), "root"),
        ] {
            assert!(validate_root(bad, &pol).is_err(), "{what}");
        }
        // a plain file, a missing path, a ':' in the name
        let file = project.join("f");
        std::fs::write(&file, "x").unwrap();
        assert!(validate_root(&file, &pol).is_err());
        assert!(validate_root(&project.join("nope"), &pol).is_err());
        let colon = project.join("a:rw");
        std::fs::create_dir(&colon).unwrap();
        assert!(validate_root(&colon, &pol).is_err());
    }

    #[test]
    fn a_symlink_is_resolved_and_a_swap_after_approval_is_caught_at_start() {
        let (t, store, repo) = setup();
        let other = t.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&repo, &link).unwrap();
        let pol = policy_for(store.dir());
        // approved through the link: the canonical target is stored
        let g = add_grant(
            &store,
            &pol,
            &NewGrant {
                path: &link,
                access: Access::ReadOnly,
                ttl: None,
                request_id: None,
                source: "cli-add",
            },
            now(),
        )
        .unwrap();
        assert_eq!(g.path, std::fs::canonicalize(&repo).unwrap());
        let res = resolve_for_start(&store, &pol, now());
        assert_eq!(res.roots.len(), 1);
        // replace the approved directory by another one at the same path
        std::fs::remove_dir(&repo).unwrap();
        std::fs::create_dir(&repo).unwrap();
        let res = resolve_for_start(&store, &pol, now());
        assert!(res.roots.is_empty());
        assert!(
            res.skipped[0].1.contains("device/inode"),
            "{:?}",
            res.skipped
        );
        // repoint: the path now is a symlink to another directory
        std::fs::remove_dir(&repo).unwrap();
        std::os::unix::fs::symlink(&other, &repo).unwrap();
        let res = resolve_for_start(&store, &pol, now());
        assert!(res.roots.is_empty());
        assert!(
            res.skipped[0].1.contains("resolves to"),
            "{:?}",
            res.skipped
        );
        // the skip is recorded once per reason
        record_skips(&store, &res, now());
        record_skips(&store, &res, now());
        let state = store.load().unwrap();
        assert_eq!(state.events, 2, "granted + one skipped");
        assert!(state.find(&g.id).unwrap().last_skip.is_some());
    }

    #[test]
    fn start_uses_only_active_grants_and_nothing_from_a_broken_store() {
        let (_t, store, repo) = setup();
        let pol = policy_for(store.dir());
        let g1 = add(&store, &repo, Access::ReadWrite, None);
        let short = add(
            &store,
            &repo,
            Access::ReadOnly,
            Some(Duration::from_secs(10)),
        );
        let revoked = add(&store, &repo, Access::ReadOnly, None);
        revoke_grant(&store, &revoked.id, None, now()).unwrap();
        let res = resolve_for_start(&store, &pol, now());
        assert_eq!(res.applied.len(), 2);
        assert!(res.applied.contains(&g1.id) && res.applied.contains(&short.id));
        let later = resolve_for_start(&store, &pol, now() + Duration::from_secs(11));
        assert_eq!(later.applied, vec![g1.id.clone()]);
        // tamper: nothing at all is used
        let text = std::fs::read_to_string(store.file()).unwrap();
        std::fs::write(store.file(), text.replacen("\"rw\"", "\"ro\"", 1)).unwrap();
        let res = resolve_for_start(&store, &pol, now());
        assert!(res.roots.is_empty() && res.store_error.is_some());
        // group-writable store: not used
        std::fs::write(store.file(), &text).unwrap();
        std::fs::set_permissions(store.file(), std::fs::Permissions::from_mode(0o664)).unwrap();
        assert!(resolve_for_start(&store, &pol, now()).store_error.is_some());
        assert!(add_grant(
            &store,
            &pol,
            &NewGrant {
                path: &repo,
                access: Access::ReadOnly,
                ttl: None,
                request_id: None,
                source: "cli-add"
            },
            now()
        )
        .is_err());
    }

    #[test]
    fn a_missing_store_is_empty_and_an_unwritable_append_does_not_corrupt() {
        let (_t, store, _repo) = setup();
        assert!(store.load().unwrap().records.is_empty());
        assert!(resolve_for_start(&store, &policy_for(store.dir()), now())
            .roots
            .is_empty());
    }

    #[test]
    fn a_request_is_never_a_grant() {
        let (t, store, repo) = setup();
        let file = t.path().join(REQUEST_FILE);
        let (id, new) = append_request(
            &file,
            repo.to_str().unwrap(),
            Access::ReadWrite,
            Some("need it"),
            now(),
        )
        .unwrap();
        assert!(new && valid_id("r", &id));
        // the store is untouched and nothing resolves
        assert!(!store.file().exists());
        assert!(resolve_for_start(&store, &policy_for(store.dir()), now())
            .roots
            .is_empty());
        // the same request is not duplicated
        let (again, new) = append_request(
            &file,
            repo.to_str().unwrap(),
            Access::ReadWrite,
            None,
            now(),
        )
        .unwrap();
        assert_eq!((again, new), (id.clone(), false));
        let (reqs, bad) = read_requests(&file);
        assert_eq!((reqs.len(), bad), (1, 0));
        // a different access is a different request
        let (other, new) =
            append_request(&file, repo.to_str().unwrap(), Access::ReadOnly, None, now()).unwrap();
        assert!(new && other != id);
    }

    #[test]
    fn requests_are_validated_bounded_and_sanitized() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join(REQUEST_FILE);
        for bad in ["", "relative", "/a\nb", "/a\u{1b}[31m"] {
            assert!(
                append_request(&file, bad, Access::ReadOnly, None, now()).is_err(),
                "{bad:?}"
            );
        }
        assert!(append_request(
            &file,
            &format!("/{}", "x".repeat(2000)),
            Access::ReadOnly,
            None,
            now()
        )
        .is_err());
        let (id, _) =
            append_request(&file, "/p", Access::ReadOnly, Some("a\nb\u{1b}c"), now()).unwrap();
        let (reqs, _) = read_requests(&file);
        assert_eq!(reqs[0].id, id);
        assert!(!reqs[0]
            .reason
            .as_deref()
            .unwrap()
            .contains(['\n', '\u{1b}']));
        for i in 0..MAX_PENDING_REQUESTS {
            let _ = append_request(&file, &format!("/p{i}"), Access::ReadOnly, None, now());
        }
        assert!(
            append_request(&file, "/one-more", Access::ReadOnly, None, now()).is_err(),
            "bounded"
        );
        // a foreign or damaged line is counted, never trusted
        std::fs::write(&file, "{\"id\":\"r-0123456789ab\",\"path\":\"/x\",\"access\":\"rw\",\"ts\":\"t\",\"evil\":1}\nnope\n").unwrap();
        let (reqs, bad) = read_requests(&file);
        assert_eq!((reqs.len(), bad), (0, 2));
    }

    #[test]
    fn display_text_loses_escape_sequences_and_bidi_overrides() {
        assert_eq!(sanitize_display("a\u{1b}[2Jb\nc", 50), "a?[2Jb?c");
        assert_eq!(sanitize_display("x\u{202e}y", 50), "x?y");
        assert_eq!(sanitize_display("abcdef", 3), "abc…");
    }

    #[test]
    fn answering_a_request_closes_it() {
        let (t, store, repo) = setup();
        let file = t.path().join(REQUEST_FILE);
        let (rid, _) =
            append_request(&file, repo.to_str().unwrap(), Access::ReadOnly, None, now()).unwrap();
        let pol = policy_for(store.dir());
        let g = add_grant(
            &store,
            &pol,
            &NewGrant {
                path: &repo,
                access: Access::ReadOnly,
                ttl: None,
                request_id: Some(&rid),
                source: "cli-approve",
            },
            now(),
        )
        .unwrap();
        assert_eq!(g.request_id.as_deref(), Some(rid.as_str()));
        assert!(
            deny_request(&store, &rid, now()).is_err(),
            "already answered"
        );
        let (r2, _) = append_request(&file, "/other", Access::ReadOnly, None, now()).unwrap();
        deny_request(&store, &r2, now()).unwrap();
        assert!(store.load().unwrap().request_is_closed(&r2));
        assert!(add_grant(
            &store,
            &pol,
            &NewGrant {
                path: &repo,
                access: Access::ReadOnly,
                ttl: None,
                request_id: Some(&r2),
                source: "cli-approve"
            },
            now()
        )
        .is_err());
    }

    #[test]
    fn an_unfinished_last_line_is_ignored_and_repaired_by_the_next_writer() {
        let (_t, store, repo) = setup();
        let g1 = add(&store, &repo, Access::ReadOnly, None);
        let good = std::fs::read(store.file()).unwrap();
        // a crashed writer left half a line behind
        let mut torn = good.clone();
        torn.extend_from_slice(b"{\"seq\":2,\"ts\":\"2026-11");
        std::fs::write(store.file(), &torn).unwrap();
        let state = store.load().unwrap();
        assert_eq!(state.records.len(), 1, "the verified prefix is used");
        assert!(
            replay(std::str::from_utf8(&torn).unwrap()).is_err(),
            "the strict reader refuses it"
        );
        // the next writer removes the tail and carries on with a valid chain
        let g2 = add(&store, &repo, Access::ReadWrite, None);
        let text = std::fs::read_to_string(store.file()).unwrap();
        assert!(replay(&text).is_ok());
        let state = store.load().unwrap();
        assert_eq!(state.records.len(), 2);
        assert!(state.find(&g1.id).is_some() && state.find(&g2.id).is_some());
        // a COMPLETE line that does not verify is tampering, not a torn tail
        let mut bad = good.clone();
        bad.extend_from_slice(b"{\"seq\":2}\n");
        std::fs::write(store.file(), &bad).unwrap();
        assert!(store.load().is_err());
        // garbage without any newline at all
        std::fs::write(store.file(), b"garbage").unwrap();
        assert_eq!(
            store.load().unwrap().records.len(),
            0,
            "an unfinished first line"
        );
        // a verified prefix followed by a torn tail never hides a bad earlier line
        let mut worse = good.clone();
        worse[20] ^= 1;
        worse.extend_from_slice(b"{\"seq");
        std::fs::write(store.file(), &worse).unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn symlinks_hardlinks_and_foreign_permissions_make_the_store_unusable() {
        let (t, store, repo) = setup();
        add(&store, &repo, Access::ReadOnly, None);
        let real = std::fs::read(store.file()).unwrap();
        // 1. the store file is a symlink to a valid copy elsewhere
        let elsewhere = t.path().join("elsewhere.jsonl");
        std::fs::write(&elsewhere, &real).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::remove_file(store.file()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, store.file()).unwrap();
        assert!(store.load().is_err(), "a symlinked store is not read");
        assert!(add_grant(
            &store,
            &policy_for(store.dir()),
            &NewGrant {
                path: &repo,
                access: Access::ReadOnly,
                ttl: None,
                request_id: None,
                source: "cli-add"
            },
            now()
        )
        .is_err());
        assert_eq!(
            std::fs::read(&elsewhere).unwrap(),
            real,
            "nothing was written through the link"
        );
        // 2. a second hard link
        std::fs::remove_file(store.file()).unwrap();
        std::fs::hard_link(&elsewhere, store.file()).unwrap();
        let err = store.load().unwrap_err().to_string();
        assert!(err.contains("hard link"), "{err}");
        assert!(add_grant(
            &store,
            &policy_for(store.dir()),
            &NewGrant {
                path: &repo,
                access: Access::ReadOnly,
                ttl: None,
                request_id: None,
                source: "cli-add"
            },
            now()
        )
        .is_err());
        assert_eq!(std::fs::read(&elsewhere).unwrap(), real);
        std::fs::remove_file(store.file()).unwrap();
        std::fs::remove_file(&elsewhere).unwrap();
        // 3. a group/other-writable directory
        std::fs::write(store.file(), &real).unwrap();
        std::fs::set_permissions(store.file(), std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store.load().is_ok());
        std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(store.load().unwrap_err().to_string().contains("directory"));
        std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
        // 4. the store DIRECTORY is a symlink to a real one
        let moved = t.path().join("moved-state");
        std::fs::rename(store.dir(), &moved).unwrap();
        std::os::unix::fs::symlink(&moved, store.dir()).unwrap();
        assert!(
            store.load().is_err(),
            "a symlinked store directory is not used"
        );
        // 5. the store is a directory / a fifo
        std::fs::remove_file(store.dir()).unwrap();
        std::fs::create_dir(store.dir()).unwrap();
        std::fs::set_permissions(store.dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::create_dir(store.file()).unwrap();
        assert!(store.load().is_err());
    }

    #[test]
    fn concurrent_writers_keep_one_valid_chain() {
        let (_t, store, repo) = setup();
        let n = 16;
        let handles: Vec<_> = (0..n)
            .map(|_| {
                let store = store.clone();
                let repo = repo.clone();
                std::thread::spawn(move || {
                    add_grant(
                        &store,
                        &policy_for(store.dir()),
                        &NewGrant {
                            path: &repo,
                            access: Access::ReadOnly,
                            ttl: None,
                            request_id: None,
                            source: "cli-add",
                        },
                        now(),
                    )
                    .unwrap()
                    .id
                })
            })
            .collect();
        let ids: BTreeSet<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(ids.len(), n, "unique ids");
        let state = store.load().unwrap();
        assert_eq!(state.records.len(), n);
        assert_eq!(state.events, n as u64);
        // interleaved revokes and adds
        let to_revoke: Vec<String> = ids.iter().take(8).cloned().collect();
        let hs: Vec<_> = to_revoke
            .into_iter()
            .map(|id| {
                let store = store.clone();
                std::thread::spawn(move || revoke_grant(&store, &id, None, now()).unwrap())
            })
            .chain((0..4).map(|_| {
                let store = store.clone();
                let repo = repo.clone();
                std::thread::spawn(move || {
                    add_grant(
                        &store,
                        &policy_for(store.dir()),
                        &NewGrant {
                            path: &repo,
                            access: Access::ReadWrite,
                            ttl: None,
                            request_id: None,
                            source: "cli-add",
                        },
                        now(),
                    )
                    .unwrap();
                })
            }))
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        let state = store.load().unwrap();
        assert_eq!(state.records.len(), n + 4);
        assert_eq!(
            state
                .records
                .iter()
                .filter(|r| r.revoked_at.is_some())
                .count(),
            8
        );
        assert!(replay(&std::fs::read_to_string(store.file()).unwrap()).is_ok());
    }

    #[test]
    fn two_writers_cannot_answer_the_same_request_twice() {
        let (t, store, repo) = setup();
        let file = t.path().join(REQUEST_FILE);
        let (rid, _) =
            append_request(&file, repo.to_str().unwrap(), Access::ReadOnly, None, now()).unwrap();
        let results: Vec<bool> = (0..8)
            .map(|_| {
                let (store, repo, rid) = (store.clone(), repo.clone(), rid.clone());
                std::thread::spawn(move || {
                    add_grant(
                        &store,
                        &policy_for(store.dir()),
                        &NewGrant {
                            path: &repo,
                            access: Access::ReadOnly,
                            ttl: None,
                            request_id: Some(&rid),
                            source: "cli-approve",
                        },
                        now(),
                    )
                    .is_ok()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|ok| **ok).count(), 1, "{results:?}");
        assert_eq!(store.load().unwrap().records.len(), 1);
    }

    #[test]
    fn the_request_log_survives_damage_concurrency_and_its_limits() {
        let t = tempfile::tempdir().unwrap();
        let file = t.path().join(REQUEST_FILE);
        // concurrent requests: every line intact, every request present
        let hs: Vec<_> = (0..30)
            .map(|i| {
                let file = file.clone();
                std::thread::spawn(move || {
                    append_request(&file, &format!("/p{i}"), Access::ReadOnly, Some("r"), now())
                        .is_ok()
                })
            })
            .collect();
        let accepted = hs
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|b| *b)
            .count();
        let (reqs, bad) = read_requests(&file);
        assert_eq!(
            (reqs.len(), bad),
            (accepted, 0),
            "no torn or interleaved line"
        );
        assert!(accepted > 0);
        // damage: invalid UTF-8 and an unfinished last line, then a new request
        let mut damaged = std::fs::read(&file).unwrap();
        damaged.extend_from_slice(b"\xff\xfe not json\n{\"id\":\"r-0");
        std::fs::write(&file, &damaged).unwrap();
        let (reqs2, bad2) = read_requests(&file);
        assert_eq!(reqs2.len(), reqs.len(), "valid lines survive damage");
        assert_eq!(bad2, 2);
        let (id, _) =
            append_request(&file, "/after-damage", Access::ReadOnly, None, now()).unwrap();
        let (reqs3, _) = read_requests(&file);
        assert!(
            reqs3.iter().any(|r| r.id == id),
            "a new request is not glued to the damaged tail"
        );
        // the 256 KiB limit
        std::fs::write(&file, vec![b'x'; (MAX_REQUEST_FILE_BYTES + 1) as usize]).unwrap();
        let err = append_request(&file, "/p", Access::ReadOnly, None, now())
            .unwrap_err()
            .to_string();
        assert!(err.contains("too many"), "{err}");
        // a directory in place of the log, a symlink, an unwritable file
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(append_request(&file, "/p", Access::ReadOnly, None, now()).is_err());
        std::fs::remove_dir(&file).unwrap();
        let target = t.path().join("target");
        std::fs::write(&target, "").unwrap();
        std::os::unix::fs::symlink(&target, &file).unwrap();
        assert!(append_request(&file, "/p", Access::ReadOnly, None, now()).is_err());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"",
            "nothing was written through the link"
        );
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
        let denied = append_request(&file, "/p", Access::ReadOnly, None, now());
        // root ignores file modes; everyone else must get an error, never a panic
        if std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .is_err()
        {
            assert!(denied.is_err());
        }
    }
}
