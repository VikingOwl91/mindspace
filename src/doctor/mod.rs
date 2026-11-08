//! `mind doctor`: local, deterministic diagnosis of an installation.
//!
//! Read-only by design: it never initializes, migrates or writes the
//! production database (see `storage::sqlite::inspect_database`), makes no
//! network connection, and needs no model or external API. The one thing it
//! does beyond reading is the sandbox self-test (`selftest`), which runs the
//! real server against a throw-away database and root under the system temp
//! directory.
//!
//! Output contract (documented in `docs/doctor.md`):
//!  - statuses `pass`, `warn`, `fail`, `skip`;
//!  - exit code 0 = no blocking failure (warnings allowed), 1 = at least one
//!    blocking `fail`, 2 = the diagnosis itself could not be carried out
//!    (also: invalid command line).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use crate::core::Result;
use crate::paths::Located;
use crate::storage::sqlite::{inspect_database, DatabaseInspection};

#[cfg(target_os = "linux")]
pub mod approval;
#[cfg(target_os = "linux")]
pub mod capabilities;
#[cfg(target_os = "linux")]
pub mod selftest;

/// Format tag of the JSON report; bumped when the structure changes.
pub const REPORT_FORMAT: &str = "mind-doctor/1";

/// Upper bound for the sandbox self-test.
pub const SELFTEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Status {
    fn tag(&self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
            Status::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub title: &'static str,
    pub status: Status,
    pub summary: String,
    pub details: Value,
    pub remedy: Option<String>,
    /// The check could not be carried out (not a finding about the system);
    /// makes the exit code 2. Serialized only when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub diagnostic_error: bool,
}

impl Check {
    fn new(
        id: &'static str,
        title: &'static str,
        status: Status,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id,
            title,
            status,
            summary: summary.into(),
            details: Value::Null,
            remedy: None,
            diagnostic_error: false,
        }
    }
    fn details(mut self, d: Value) -> Self {
        self.details = d;
        self
    }
    fn remedy(mut self, r: impl Into<String>) -> Self {
        self.remedy = Some(r.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub skip: usize,
}

impl Report {
    pub fn counts(&self) -> Counts {
        let n = |s: Status| self.checks.iter().filter(|c| c.status == s).count();
        Counts {
            pass: n(Status::Pass),
            warn: n(Status::Warn),
            fail: n(Status::Fail),
            skip: n(Status::Skip),
        }
    }

    /// 2: a check could not be carried out; 1: a blocking failure; 0 otherwise.
    pub fn exit_code(&self) -> i32 {
        if self.checks.iter().any(|c| c.diagnostic_error) {
            2
        } else if self.checks.iter().any(|c| c.status == Status::Fail) {
            1
        } else {
            0
        }
    }

    pub fn to_json(&self) -> Value {
        let c = self.counts();
        json!({
            "format": REPORT_FORMAT,
            "mind_version": env!("CARGO_PKG_VERSION"),
            "summary": {"pass": c.pass, "warn": c.warn, "fail": c.fail, "skip": c.skip,
                        "exit_code": self.exit_code()},
            "checks": self.checks,
        })
    }

    pub fn render_text(&self) -> String {
        let mut out = format!("mind doctor (mind {})\n\n", env!("CARGO_PKG_VERSION"));
        for c in &self.checks {
            out.push_str(&format!("{}  {:<11} {}\n", c.status.tag(), c.id, c.summary));
            let mut lines = Vec::new();
            flatten("", &c.details, &mut lines);
            for (k, v) in lines {
                out.push_str(&format!("      {k}: {v}\n"));
            }
            if let Some(r) = &c.remedy {
                out.push_str(&format!("      hint: {r}\n"));
            }
        }
        let n = self.counts();
        out.push_str(&format!(
            "\nSummary: {} pass, {} warn, {} fail, {} skip (exit code {})\n",
            n.pass,
            n.warn,
            n.fail,
            n.skip,
            self.exit_code()
        ));
        out
    }
}

/// One `key: value` line per scalar; nested objects use dotted keys, arrays stay compact JSON.
fn flatten(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Null => {}
        Value::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, v, out);
            }
        }
        Value::String(s) => out.push((prefix.to_string(), s.clone())),
        other => out.push((prefix.to_string(), other.to_string())),
    }
}

/// Everything the diagnosis reads from its surroundings, so tests can supply it.
pub struct Inputs {
    pub db: Result<Located>,
    pub data: Result<Located>,
    pub exe: Option<PathBuf>,
    pub path_env: Option<OsString>,
    pub allow_roots_env: Option<OsString>,
    pub ingest_timeout_env: Option<OsString>,
    /// Run the sandbox self-test (a child `mind mcp`).
    pub run_selftest: bool,
}

impl Inputs {
    /// The real process environment.
    pub fn from_process(db: Result<Located>, data: Result<Located>, run_selftest: bool) -> Self {
        Self {
            db,
            data,
            exe: std::env::current_exe().ok(),
            path_env: std::env::var_os("PATH"),
            allow_roots_env: std::env::var_os("MINDSPACE_MCP_ALLOW_ROOTS"),
            ingest_timeout_env: std::env::var_os("MINDSPACE_MCP_INGEST_TIMEOUT"),
            run_selftest,
        }
    }
}

pub fn run(inputs: &Inputs) -> Report {
    let (database, inspection) = check_database(&inputs.db);
    let checks = vec![
        check_binary(inputs.exe.as_deref(), inputs.path_env.as_deref()),
        database,
        check_config(inputs),
        check_git(),
        check_embeddings(&inputs.data, &inputs.db, inspection.as_ref()),
        check_sandbox(inputs),
    ];
    Report { checks }
}

// ---------------------------------------------------------------- binary

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn check_binary(exe: Option<&Path>, path_env: Option<&std::ffi::OsStr>) -> Check {
    let version = env!("CARGO_PKG_VERSION");
    let Some(exe) = exe else {
        return Check::new(
            "binary",
            "Binary",
            Status::Warn,
            format!("mind {version}; the path of the running executable is unknown"),
        );
    };
    let exe = canonical(exe);
    let on_path = crate::pathenv::executables("mind", path_env)
        .into_iter()
        .next()
        .map(|p| canonical(&p));
    let mut c = Check::new("binary", "Binary", Status::Pass, format!("mind {version}"));
    let mut d = json!({"version": version, "executable": exe.display().to_string()});
    match &on_path {
        None => {
            d["on_path"] = json!("no `mind` found on PATH");
        }
        Some(p) if *p == exe => {
            d["on_path"] = json!("this binary is the first `mind` on PATH");
        }
        Some(p) => {
            c.status = Status::Warn;
            c.summary = format!(
                "mind {version}; a different `mind` comes first on PATH: {}",
                p.display()
            );
            d["on_path"] = json!(p.display().to_string());
            c = c.remedy(
                "run `which -a mind`; remove or update the older binary so that scripts and MCP clients start the version you expect",
            );
        }
    }
    c.details(d)
}

// -------------------------------------------------------------- database

fn check_database(db: &Result<Located>) -> (Check, Option<DatabaseInspection>) {
    let located = match db {
        Ok(l) => l,
        Err(e) => {
            return (
                Check::new(
                    "database",
                    "Database",
                    Status::Fail,
                    "the database path cannot be determined",
                )
                .details(json!({"error": e.to_string()}))
                .remedy("set MINDSPACE_DB or pass --db <path>"),
                None,
            )
        }
    };
    let path = &located.path;
    let mut d = json!({
        "path": path.display().to_string(),
        "source": located.source.label(),
        "default": located.is_default,
    });
    let fail = |summary: String, d: Value, remedy: Option<String>| {
        let mut c = Check::new("database", "Database", Status::Fail, summary).details(d);
        if let Some(r) = remedy {
            c = c.remedy(r);
        }
        c
    };
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (
                fail(
                    "no database at this path (not initialized)".into(),
                    d,
                    Some("run: mind init".into()),
                ),
                None,
            )
        }
        Err(e) => {
            return (
                fail(format!("cannot access the database file: {e}"), d, None),
                None,
            )
        }
    };
    if !meta.is_file() {
        return (fail("the path is not a regular file".into(), d, None), None);
    }
    d["size_bytes"] = json!(meta.len());
    if meta.len() == 0 {
        return (
            fail(
                "the file is empty (not initialized)".into(),
                d,
                Some("run: mind init".into()),
            ),
            None,
        );
    }
    let inspection = match inspect_database(path) {
        Ok(i) => i,
        Err(e) => {
            d["error"] = json!(e.to_string());
            let mut wal = path.as_os_str().to_owned();
            wal.push("-wal");
            if Path::new(&wal).exists() {
                // A write-ahead log means a server may be writing, or a crash left
                // one behind; neither is evidence of a broken database.
                return (
                    Check::new(
                        "database",
                        "Database",
                        Status::Warn,
                        "could not be inspected read-only while a write-ahead log exists",
                    )
                    .details(d)
                    .remedy("run any mind command (it recovers the log), or stop the running server, and try again"),
                    None,
                );
            }
            return (
                fail(
                    "the file cannot be read as a SQLite database".into(),
                    d,
                    Some("check the path; a different or damaged file was found".into()),
                ),
                None,
            );
        }
    };
    let versions = &inspection.schema_versions;
    let supported = inspection.supported_version;
    d["schema_version"] = json!(versions.last());
    d["supported_schema_version"] = json!(supported);
    d["write_ahead_log_present"] = json!(inspection.saw_wal);
    if versions.is_empty() {
        return (
            fail(
                "a SQLite file without a Mindspace schema (not a Mindspace database)".into(),
                d,
                Some(
                    "use another --db path; `mind init` would add Mindspace tables to this file"
                        .into(),
                ),
            ),
            Some(inspection),
        );
    }
    let newest = *versions.last().expect("non-empty");
    // Same order as the migration code: a newer schema is reported before a gap.
    if newest <= supported
        && versions
            .iter()
            .zip(1_i64..)
            .any(|(v, expected)| *v != expected)
    {
        return (
            fail(
                "the migration history is not a contiguous prefix (damaged or edited database)"
                    .into(),
                d,
                Some("restore from a backup; do not run commands that write".into()),
            ),
            Some(inspection),
        );
    }
    let mut c = if newest > supported {
        fail(
            format!("schema version {newest} is newer than this binary supports ({supported})"),
            d,
            Some("use the mind version that created this database, or a newer one".into()),
        )
    } else if newest < supported {
        Check::new(
            "database",
            "Database",
            Status::Warn,
            format!("schema version {newest}; this binary will migrate it to {supported} on the next command"),
        )
        .details(d)
        .remedy("copy the database file first if you want a way back; migrations are one-way")
    } else {
        Check::new(
            "database",
            "Database",
            Status::Pass,
            format!("schema version {newest} (current)"),
        )
        .details(d)
    };
    if c.status == Status::Pass && meta.permissions().readonly() {
        c.status = Status::Warn;
        c.summary
            .push_str("; the file is read-only, commands that write will fail");
    }
    (c, Some(inspection))
}

// ----------------------------------------------------------------- config

fn check_config(i: &Inputs) -> Check {
    let mut d = serde_json::Map::new();
    let mut status = Status::Pass;
    let mut problems: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut remedy = None;

    match &i.db {
        Ok(l) => {
            d.insert(
                "database".into(),
                json!({"path": l.path.display().to_string(), "source": l.source.label()}),
            );
            if !l.path.is_absolute() {
                warnings.push(format!(
                    "the database path {} is relative and resolves against the current directory",
                    l.path.display()
                ));
            }
        }
        Err(e) => {
            problems.push(format!("database path: {e}"));
            remedy = Some("set MINDSPACE_DB or pass --db <path>".to_string());
        }
    }
    match &i.data {
        Ok(l) => {
            d.insert(
                "data_dir".into(),
                json!({"path": l.path.display().to_string(), "source": l.source.label(),
                       "models": l.path.join("models").display().to_string()}),
            );
            for n in &l.notes {
                warnings.push(n.clone());
            }
        }
        Err(e) => {
            if i.db.is_ok() {
                warnings.push(format!("models cannot be located: {e}"));
            } else {
                problems.push(format!("data directory: {e}"));
            }
        }
    }
    d.insert("model_directory_follows_database".into(), json!(false));
    match &i.ingest_timeout_env {
        None => {
            d.insert("ingest_timeout".into(), json!("default (30 s)"));
        }
        Some(v) => match v.to_str().map(crate::mcp::parse_ingest_timeout) {
            Some(Ok(t)) => {
                d.insert(
                    "ingest_timeout".into(),
                    json!({"value": format!("{} s", t.as_secs_f64()), "source": "environment variable MINDSPACE_MCP_INGEST_TIMEOUT"}),
                );
            }
            Some(Err(e)) => problems.push(format!("MINDSPACE_MCP_INGEST_TIMEOUT: {e}")),
            None => problems.push("MINDSPACE_MCP_INGEST_TIMEOUT is not valid UTF-8".into()),
        },
    }
    match &i.allow_roots_env {
        None => {
            d.insert(
                "allowed_roots".into(),
                json!("none from the environment (repository tools stay off unless --allow-root is given)"),
            );
        }
        Some(v) => match v.to_str().map(|s| crate::mcp::McpConfig::from_specs(&[s])) {
            Some(Ok(cfg)) => {
                let roots: Vec<Value> = cfg
                    .roots()
                    .iter()
                    .map(|r| json!({"path": r.path.display().to_string(),
                                    "mode": if r.access == crate::mcp::Access::ReadWrite {"rw"} else {"ro"}}))
                    .collect();
                d.insert(
                    "allowed_roots".into(),
                    json!({"source": "environment variable MINDSPACE_MCP_ALLOW_ROOTS", "roots": roots}),
                );
                #[cfg(target_os = "linux")]
                if let Ok(db) = &i.db {
                    if let Some(dir) = db.path.parent().and_then(|p| std::fs::canonicalize(p).ok())
                    {
                        let plan = crate::sandbox::Plan {
                            roots: cfg
                                .roots()
                                .iter()
                                .map(|r| {
                                    (r.path.clone(), r.access == crate::mcp::Access::ReadWrite)
                                })
                                .collect(),
                            db_dir: dir,
                            models_dir: PathBuf::new(),
                            exe: PathBuf::new(),
                            identities: Default::default(),
                        };
                        if let Err(e) = plan.validate_layout() {
                            warnings.push(format!(
                                "`mind mcp` would turn the repository tools off: {e}"
                            ));
                        }
                    }
                }
            }
            Some(Err(e)) => {
                problems.push(format!("MINDSPACE_MCP_ALLOW_ROOTS: {e}"));
                remedy.get_or_insert_with(|| {
                    "write each root as DIR:ro or DIR:rw, joined with ':' (existing directories only)".to_string()
                });
            }
            None => problems.push("MINDSPACE_MCP_ALLOW_ROOTS is not valid UTF-8".into()),
        },
    }
    let summary = if !problems.is_empty() {
        status = Status::Fail;
        problems.join("; ")
    } else if !warnings.is_empty() {
        status = Status::Warn;
        warnings.join("; ")
    } else {
        "effective values and their sources are listed below".to_string()
    };
    let mut c = Check::new("config", "Configuration", status, summary).details(Value::Object(d));
    if let Some(r) = remedy {
        c = c.remedy(r);
    }
    c
}

// -------------------------------------------------------------------- git

fn check_git() -> Check {
    use std::process::{Command, Stdio};
    let fail = |why: String| {
        Check::new("git", "Git", Status::Warn, why).remedy(
            "install git to ingest repositories; documents and manual memories work without it",
        )
    };
    let mut child = match Command::new("git")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return fail(format!("git was not found or cannot be started: {e}")),
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return fail("`git --version` did not finish within 5 s".into());
            }
            Err(e) => return fail(format!("waiting for git failed: {e}")),
        }
    }
    let out = child.wait_with_output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout).trim().to_string();
            Check::new("git", "Git", Status::Pass, text.clone()).details(json!({"version": text}))
        }
        Ok(o) => fail(format!("`git --version` exited with {}", o.status)),
        Err(e) => fail(format!("reading git output failed: {e}")),
    }
}

// ------------------------------------------------------------- embeddings

fn check_embeddings(
    data: &Result<Located>,
    db: &Result<Located>,
    inspection: Option<&DatabaseInspection>,
) -> Check {
    let id = "embeddings";
    let title = "Embeddings";
    #[cfg(not(feature = "embeddings"))]
    {
        let _ = (data, db, inspection);
        Check::new(
            id,
            title,
            Status::Skip,
            "this build has no embeddings feature (lexical search only)",
        )
    }
    #[cfg(feature = "embeddings")]
    {
        let _ = db;
        let Some(ins) = inspection else {
            return Check::new(
                id,
                title,
                Status::Skip,
                "not checked: the database could not be inspected",
            );
        };
        let enabled = ins.embedding_enabled.as_deref() == Some("true");
        let model = ins.embedding_model.clone();
        let mut d = json!({"enabled": enabled, "selected_model": model});
        if !enabled {
            return Check::new(id, title, Status::Skip, "disabled (optional); search uses the lexical index")
                .details(d)
                .remedy("optional: `mind embeddings enable` asks first and then downloads the default model (about 550 MB) and selects it; `mind index run` builds the vectors");
        }
        let Some(model_id) = model else {
            return Check::new(
                id,
                title,
                Status::Warn,
                "enabled, but no model is selected; search falls back to lexical",
            )
            .details(d)
            .remedy("mind model list; mind model install <id> --yes; mind model use <id>");
        };
        let Some(entry) = crate::embeddings::manifest::find_model(&model_id) else {
            return Check::new(
                id,
                title,
                Status::Warn,
                format!("the selected model `{model_id}` is not in this build's manifest"),
            )
            .details(d)
            .remedy("mind model list; mind model use <id>");
        };
        let base = match data {
            Ok(l) => l.path.clone(),
            Err(e) => {
                d["error"] = json!(e.to_string());
                return Check::new(
                    id,
                    title,
                    Status::Warn,
                    "the model directory cannot be located; search falls back to lexical",
                )
                .details(d)
                .remedy("set XDG_DATA_HOME or HOME");
            }
        };
        let dir = crate::embeddings::manager::model_dir_in(&base, entry.id);
        d["model_directory"] = json!(dir.display().to_string());
        if crate::embeddings::manager::is_installed_in(&base, entry) {
            d["files_verified"] = json!(false);
            Check::new(
                id,
                title,
                Status::Pass,
                format!(
                    "enabled; model {} is installed (hashes not re-checked here)",
                    entry.id
                ),
            )
            .details(d)
            .remedy("`mind embeddings status` re-verifies the model files against the manifest")
        } else {
            Check::new(
                id,
                title,
                Status::Warn,
                format!(
                    "enabled; model {} is not installed; search falls back to lexical",
                    entry.id
                ),
            )
            .details(d)
            .remedy(format!("mind model install {} --yes", entry.id))
        }
    }
}

// ---------------------------------------------------------------- sandbox

/// The existing self-test check plus, additively, `details.capabilities`
/// (kernel support vs. what the product declares vs. proven active). The
/// evidence is a typed value set by the self-test code itself, not parsed back
/// out of the JSON text.
#[cfg(target_os = "linux")]
fn check_sandbox(i: &Inputs) -> Check {
    let mut proven = capabilities::Proven::default();
    let mut c = check_sandbox_selftest(i, &mut proven);
    if c.details.is_null() {
        c.details = json!({});
    }
    c.details["capabilities"] = capabilities::report(proven);
    // Who may create repository grants here, computed conservatively (M7.3b).
    c.details["approval"] = approval::report(proven);
    c
}

/// `proven` is raised above its defaults only on the paths that establish it:
/// an enforced report whose application checks also pass, or an `unavailable` report.
#[cfg(target_os = "linux")]
fn check_sandbox_selftest(i: &Inputs, proven: &mut capabilities::Proven) -> Check {
    use selftest::KernelEvidence;
    let (id, title) = ("sandbox", "Sandbox");
    if !i.run_selftest {
        return Check::new(id, title, Status::Skip, "self-test not run");
    }
    let Some(exe) = i.exe.as_deref() else {
        let mut c = Check::new(
            id,
            title,
            Status::Fail,
            "cannot run the self-test: the path of this executable is unknown",
        );
        c.diagnostic_error = true;
        return c;
    };
    let result = match selftest::run(exe, SELFTEST_TIMEOUT) {
        Ok(r) => r,
        Err(e) => {
            let mut c = Check::new(id, title, Status::Fail, e.to_string())
                .remedy("run `mind doctor` again; if it persists, check that the temp directory is writable and the binary can start");
            c.diagnostic_error = true;
            return c;
        }
    };
    let kernel_note = "server self-report: the server restricts itself with Landlock, then verifies it with probes that must be denied (ADR-029); the doctor cannot observe the kernel state of another process from outside";
    let app_note = "application layer: the allowlist refuses the path before the kernel is involved; this does not by itself prove the kernel sandbox";
    let mut d = json!({
        "elapsed_ms": result.elapsed_ms,
        "kernel_evidence": {"basis": kernel_note},
        "application_validation": {
            "inside_root_allowed": result.inside_allowed,
            "outside_root_refused": result.outside_refused,
            "dotdot_escape_refused": result.traversal_refused,
            "refusal_message": result.outside_message,
            "basis": app_note,
        },
    });
    match &result.kernel {
        KernelEvidence::Enforced(desc) => {
            d["kernel_evidence"]["state"] = json!("enforced");
            d["kernel_evidence"]["server_report"] = json!(desc);
            let ok = result.inside_allowed == Some(true)
                && result.outside_refused == Some(true)
                && result.traversal_refused == Some(true);
            if ok {
                proven.sandbox = capabilities::Evidence::Active;
                proven.seccomp = capabilities::parse_seccomp_evidence(result.seccomp.as_ref());
                Check::new(id, title, Status::Pass, "kernel sandbox enforced (server-reported, probe-verified); access inside the root works, outside access is refused").details(d)
            } else {
                Check::new(id, title, Status::Fail, "inconsistent: the server reports an enforced sandbox but the tool behaviour does not match (an outside path was not refused, or the root was not usable)")
                    .details(d)
                    .remedy("do not use the repository tools; please report this with the output of `mind doctor --json`")
            }
        }
        KernelEvidence::Unavailable(reason) => {
            proven.sandbox = capabilities::Evidence::Unavailable;
            d["kernel_evidence"]["state"] = json!("unavailable");
            d["kernel_evidence"]["reason"] = json!(reason);
            Check::new(id, title, Status::Warn, "kernel sandbox NOT available: the repository tools (repo_outline, repo_ingest) are off; memory tools and search are unaffected")
                .details(d)
                .remedy("needs Linux 6.2+ with Landlock in the active LSM list (`cat /sys/kernel/security/lsm`); there is no safe way to turn the sandbox off and keep the repository tools, so none is suggested here")
        }
        KernelEvidence::Disabled(s) => {
            d["kernel_evidence"]["state"] = json!("disabled");
            d["kernel_evidence"]["server_report"] = json!(s);
            Check::new(id, title, Status::Fail, "inconsistent: the self-test never passes the opt-out flag, yet the server reports the sandbox as disabled")
                .details(d)
                .remedy("please report this with the output of `mind doctor --json`")
        }
        KernelEvidence::NotReported(why) => {
            d["kernel_evidence"]["state"] = json!("not reported");
            d["kernel_evidence"]["reason"] = json!(why);
            Check::new(
                id,
                title,
                Status::Fail,
                format!("the server gave no usable sandbox report: {why}"),
            )
            .details(d)
            .remedy("run `mind mcp --allow-root <dir>:ro` by hand and read the messages on stderr")
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn check_sandbox(_i: &Inputs) -> Check {
    Check::new(
        "sandbox",
        "Sandbox",
        Status::Skip,
        "the kernel sandbox exists only on Linux; on this platform the repository tools (repo_outline, repo_ingest) are always off",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &'static str, status: Status) -> Check {
        Check::new(id, "t", status, "s")
    }

    #[test]
    fn exit_codes_follow_the_contract() {
        let r = |cs: Vec<Check>| Report { checks: cs }.exit_code();
        assert_eq!(
            r(vec![
                check("a", Status::Pass),
                check("b", Status::Warn),
                check("c", Status::Skip)
            ]),
            0
        );
        assert_eq!(
            r(vec![check("a", Status::Pass), check("b", Status::Fail)]),
            1
        );
        let mut broken = check("c", Status::Fail);
        broken.diagnostic_error = true;
        assert_eq!(r(vec![check("a", Status::Fail), broken]), 2);
    }

    #[test]
    fn the_json_report_has_the_documented_shape() {
        let rep = Report {
            checks: vec![check("a", Status::Pass).remedy("x")],
        };
        let v = rep.to_json();
        assert_eq!(v["format"], REPORT_FORMAT);
        assert_eq!(v["checks"][0]["status"], "pass");
        assert_eq!(v["summary"]["pass"], 1);
        assert!(v["checks"][0].get("diagnostic_error").is_none());
    }
}
