//! The three supported agent clients: detection (by looking at PATH only: the client is
//! never started for a preview, because even `--version` or a first `mcp get` can create files
//! in the client's own directories) and, for Claude Code and Codex
//! CLI, registration through the client's OWN command (`<client> mcp add`),
//! verified afterwards through its `mcp get`. No client configuration file is
//! written or parsed by Mindspace; the only file this module touches is a backup
//! copy of Codex's `config.toml` (see [`backup_codex_config`]).
//!
//! What was observed (Claude Code 2.1.319, Codex CLI 0.167.0, OpenCode 2.2.5;
//! see docs/setup.md): Claude Code's `mcp add` refuses an existing name; Codex's
//! and OpenCode's overwrite it silently, so the existence check below is not
//! optional. Codex also rewrites other servers' entries when it adds one
//! (comments inside them are lost), hence the backup. OpenCode has no
//! verification that works without a background service and gave inconsistent
//! answers, so it is preview-only.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::{exec, Client, Plan, Scope};
use crate::core::{Error, Result};

/// The name under which Mindspace is registered.
pub const SERVER_NAME: &str = "mindspace";

impl Client {
    pub fn program(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude",
            Client::Codex => "codex",
            Client::OpenCode => "opencode",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::Codex => "Codex CLI",
            Client::OpenCode => "OpenCode",
        }
    }
    pub fn slug(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code",
            Client::Codex => "codex",
            Client::OpenCode => "opencode",
        }
    }
    /// Whether the client's `mcp add` replaces an entry of the same name without asking.
    pub fn add_overwrites(self) -> bool {
        !matches!(self, Client::ClaudeCode)
    }
    /// Whether `mind setup --apply` can register this client safely.
    pub fn can_apply(self) -> bool {
        !matches!(self, Client::OpenCode)
    }
}

/// Where a client's program was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    /// Distinct executables named like the client, in `PATH` order (the first is
    /// the one a shell would run).
    pub paths: Vec<PathBuf>,
}

impl Detection {
    pub fn first(&self) -> Option<&Path> {
        self.paths.first().map(PathBuf::as_path)
    }
    pub fn ambiguous(&self) -> bool {
        self.paths.len() > 1
    }
}

pub fn detect(client: Client, path_env: Option<&OsStr>) -> Detection {
    Detection {
        paths: crate::pathenv::executables(client.program(), path_env),
    }
}

/// The argument vector (after the program name) that registers Mindspace.
/// Used both to run it and to show it, so the preview cannot drift from the action.
pub fn add_args(plan: &Plan) -> Vec<String> {
    let mut a: Vec<String> = vec!["mcp".into(), "add".into()];
    match plan.client {
        Client::ClaudeCode => {
            a.push("-s".into());
            a.push(plan.scope.as_str().into());
        }
        Client::Codex => {}
        Client::OpenCode => a.push("--global".into()),
    }
    a.push(SERVER_NAME.into());
    a.push("--".into());
    a.push(plan.exe.display().to_string());
    a.extend(plan.server_args());
    a
}

/// The command a person can run to undo the registration.
pub fn remove_command(plan: &Plan) -> String {
    match plan.client {
        Client::ClaudeCode => format!("claude mcp remove {SERVER_NAME} -s {}", plan.scope.as_str()),
        Client::Codex => format!("codex mcp remove {SERVER_NAME}"),
        Client::OpenCode => "edit the \"mindspace\" entry out of your OpenCode config (opencode.json), or see `opencode mcp --help`".into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Existing {
    Missing,
    /// An entry named `mindspace` exists (in any scope); what the client says about it.
    Present(String),
}

fn run(program: &Path, args: &[&str], timeout: Duration) -> Result<exec::Out> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    exec::run(program, &args, timeout)
        .map_err(|e| Error::Setup(format!("could not run {}: {e}", program.display())))
}

const NO_SUCH: &str = "No MCP server named";

/// Asks the client whether an entry named `mindspace` already exists. Anything
/// that is neither a clear "yes" nor a clear "no" is an error, never "no".
pub fn existing(client: Client, program: &Path, timeout: Duration) -> Result<Existing> {
    let out = match client {
        Client::ClaudeCode => run(program, &["mcp", "get", SERVER_NAME], timeout)?,
        Client::Codex => run(program, &["mcp", "get", SERVER_NAME, "--json"], timeout)?,
        Client::OpenCode => {
            return Err(Error::Setup(
                "OpenCode has no reliable existence check".into(),
            ))
        }
    };
    if out.timed_out {
        return Err(Error::Setup(format!(
            "`{} mcp get` did not finish in time; nothing was changed",
            client.program()
        )));
    }
    if out.success() {
        let detail = match client {
            Client::ClaudeCode => out
                .stdout
                .lines()
                .map(str::trim)
                .filter(|l| {
                    l.starts_with("Scope:") || l.starts_with("Command:") || l.starts_with("Args:")
                })
                .collect::<Vec<_>>()
                .join("; "),
            // Codex prints the whole entry, environment variables included: report only
            // what is needed to recognize it, never values that may be secrets.
            _ => serde_json::from_str::<Value>(&out.stdout)
                .ok()
                .map(|v| {
                    let args: Vec<&str> = v["transport"]["args"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).collect())
                        .unwrap_or_default();
                    format!(
                        "command {} {}",
                        v["transport"]["command"].as_str().unwrap_or("?"),
                        args.join(" ")
                    )
                })
                .unwrap_or_else(|| "details withheld".to_string()),
        };
        return Ok(Existing::Present(detail));
    }
    if out.code == Some(1) && out.text().contains(NO_SUCH) {
        return Ok(Existing::Missing);
    }
    Err(Error::Setup(format!(
        "could not tell whether {} already has a `{SERVER_NAME}` entry (`{} mcp get` exited with {:?}: {}); nothing was changed",
        client.label(),
        client.program(),
        out.code,
        out.text().trim().lines().next().unwrap_or("no output")
    )))
}

/// Runs the client's `mcp add`. A non-zero exit is an error with the client's message.
pub fn register(program: &Path, plan: &Plan, timeout: Duration) -> Result<()> {
    let args = add_args(plan);
    let out = exec::run(program, &args, timeout)
        .map_err(|e| Error::Setup(format!("could not run {}: {e}", program.display())))?;
    if out.timed_out {
        return Err(Error::Setup(format!(
            "`{} mcp add` did not finish in time",
            plan.client.program()
        )));
    }
    if !out.success() {
        return Err(Error::Setup(format!(
            "`{} mcp add` failed (exit {:?}): {}",
            plan.client.program(),
            out.code,
            out.text().trim()
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The client reports exactly what was registered. `connected` is the client's
    /// own start-up check where it has one.
    Verified {
        connected: Option<bool>,
        detail: String,
    },
    /// The client reports something else (different command, scope, or arguments).
    Mismatch(String),
    /// The answer could not be read.
    Unreadable(String),
}

/// Reads the registration back through the client and compares it with the plan.
pub fn verify(program: &Path, plan: &Plan, timeout: Duration) -> Verdict {
    let exe = plan.exe.display().to_string();
    let args = plan.server_args();
    match plan.client {
        Client::ClaudeCode => {
            let out = match run(program, &["mcp", "get", SERVER_NAME], timeout) {
                Ok(o) => o,
                Err(e) => return Verdict::Unreadable(e.to_string()),
            };
            if !out.success() {
                return Verdict::Unreadable(format!(
                    "`claude mcp get` exited with {:?}: {}",
                    out.code,
                    out.text().trim()
                ));
            }
            let field = |key: &str| {
                out.stdout
                    .lines()
                    .map(str::trim)
                    .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
            };
            let want_scope = match plan.scope {
                Scope::User => "User config",
                Scope::Local => "Local config",
            };
            let (scope, command, got_args, status) = (
                field("Scope:"),
                field("Command:"),
                field("Args:"),
                field("Status:"),
            );
            if !scope.as_deref().is_some_and(|s| s.contains(want_scope)) {
                return Verdict::Mismatch(format!("scope is {scope:?}, expected {want_scope}"));
            }
            if command.as_deref() != Some(exe.as_str()) {
                return Verdict::Mismatch(format!("command is {command:?}, expected {exe}"));
            }
            if got_args.as_deref().unwrap_or("") != args.join(" ") {
                return Verdict::Mismatch(format!(
                    "arguments are {got_args:?}, expected {:?}",
                    args.join(" ")
                ));
            }
            let connected = status.as_deref().map(|s| {
                if s.contains("Connected") {
                    Some(true)
                } else if s.contains("Failed") {
                    Some(false)
                } else {
                    None
                }
            });
            Verdict::Verified {
                connected: connected.flatten(),
                detail: status.unwrap_or_else(|| "no status line".into()),
            }
        }
        Client::Codex => {
            let out = match run(program, &["mcp", "get", SERVER_NAME, "--json"], timeout) {
                Ok(o) => o,
                Err(e) => return Verdict::Unreadable(e.to_string()),
            };
            if !out.success() {
                return Verdict::Unreadable(format!(
                    "`codex mcp get` exited with {:?}: {}",
                    out.code,
                    out.text().trim()
                ));
            }
            let Ok(v) = serde_json::from_str::<Value>(&out.stdout) else {
                return Verdict::Unreadable("`codex mcp get --json` did not print JSON".into());
            };
            let t = &v["transport"];
            let got: Vec<String> = t["args"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if t["type"] != "stdio" || t["command"] != exe.as_str() || got != args {
                return Verdict::Mismatch(format!(
                    "Codex reports command {:?} with arguments {:?}",
                    t["command"], got
                ));
            }
            if v["enabled"] == false {
                return Verdict::Mismatch("the entry is registered but disabled".into());
            }
            Verdict::Verified {
                connected: None,
                detail: "Codex does not start the server to check it; `mind doctor` and your first Codex session do".into(),
            }
        }
        Client::OpenCode => {
            Verdict::Unreadable("OpenCode registration is not verified by Mindspace".into())
        }
    }
}

/// Where Codex keeps its configuration: `$CODEX_HOME/config.toml`, else `~/.codex/config.toml`.
pub fn codex_config_path(codex_home: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    match codex_home.filter(|v| !v.is_empty()) {
        Some(h) => Some(PathBuf::from(h).join("config.toml")),
        None => home.map(|h| h.join(".codex").join("config.toml")),
    }
}

/// Largest config file that is backed up (a config is a few kilobytes).
const MAX_BACKUP_BYTES: u64 = 4 * 1024 * 1024;

/// Codex rewrites other servers' entries when it adds one (comments inside them
/// are dropped), so its config is copied first. Returns the copy's path, or `None`
/// if there is no config file yet. A copy that cannot be made stops the setup.
///
/// The copy is made carefully because the directory may be shared with other
/// processes:
///  - the config is read from ONE open file handle (a symbolic link to a config kept by a
///    dotfile tool is followed on purpose: its content is what Codex edits), and size and
///    modification time are compared before and after, so a file that changes while it is
///    read is read again instead of copied torn;
///  - the backup is created with `O_CREAT|O_EXCL`: an existing file, an older backup or a
///    planted symbolic link (also a dangling one) at that name is never opened or written
///    through; the name gets a numeric suffix instead;
///  - it is created `0600` and given the config's permissions only after the content is
///    written, then flushed to disk.
pub fn backup_codex_config(config: &Path) -> Result<Option<PathBuf>> {
    backup_codex_config_at(config, unix_seconds())
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_stable(config: &Path) -> Result<Option<(Vec<u8>, u32)>> {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    let fail = |what: &str, e: &dyn std::fmt::Display| {
        Error::Setup(format!(
            "{what} {}: {e}; nothing was changed",
            config.display()
        ))
    };
    for _ in 0..3 {
        let mut f = match std::fs::File::open(config) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(fail("cannot read", &e)),
        };
        let before = f.metadata().map_err(|e| fail("cannot inspect", &e))?;
        if !before.is_file() {
            return Err(Error::Setup(format!(
                "{} is not a regular file; nothing was changed",
                config.display()
            )));
        }
        if before.len() > MAX_BACKUP_BYTES {
            return Err(Error::Setup(format!(
                "{} is larger than {MAX_BACKUP_BYTES} bytes, which a Codex config never is; nothing was changed",
                config.display()
            )));
        }
        let mut data = Vec::with_capacity(before.len() as usize);
        (&mut f)
            .take(MAX_BACKUP_BYTES + 1)
            .read_to_end(&mut data)
            .map_err(|e| fail("cannot read", &e))?;
        let after = f.metadata().map_err(|e| fail("cannot inspect", &e))?;
        if before.len() == after.len()
            && before.modified().ok() == after.modified().ok()
            && data.len() as u64 == after.len()
        {
            return Ok(Some((data, before.permissions().mode() & 0o777)));
        }
    }
    Err(Error::Setup(format!(
        "{} kept changing while it was read (is Codex running?); nothing was changed, try again",
        config.display()
    )))
}

pub fn backup_codex_config_at(config: &Path, stamp: u64) -> Result<Option<PathBuf>> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let Some((data, mode)) = read_stable(config)? else {
        return Ok(None);
    };
    let base = config.file_name().unwrap_or_default().to_os_string();
    for attempt in 0..100u32 {
        let mut name = base.clone();
        name.push(format!(".mindspace-backup-{stamp}"));
        if attempt > 0 {
            name.push(format!("-{attempt}"));
        }
        let target = config.with_file_name(name);
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&target)
        {
            Ok(f) => f,
            // Taken (a file, an older backup, or a symbolic link): never touched, try the next name.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(Error::Setup(format!(
                    "cannot create a backup next to {}: {e}; nothing was changed",
                    config.display()
                )))
            }
        };
        let written = f
            .write_all(&data)
            .and_then(|_| f.set_permissions(std::fs::Permissions::from_mode(mode)))
            .and_then(|_| f.sync_all());
        if let Err(e) = written {
            drop(f);
            let _ = std::fs::remove_file(&target); // ours: created exclusively just now
            return Err(Error::Setup(format!(
                "cannot write the backup {}: {e}; nothing was changed",
                target.display()
            )));
        }
        return Ok(Some(target));
    }
    Err(Error::Setup(format!(
        "could not find a free backup name next to {}; nothing was changed",
        config.display()
    )))
}
