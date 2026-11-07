//! `mind setup`: registering Mindspace as an MCP server with Claude Code,
//! Codex CLI or OpenCode.
//!
//! Three modes (see `docs/setup.md`):
//!  - `mind setup` in a terminal: a guided session that ends in the same
//!    preview and confirmation as the others;
//!  - `mind setup <client> ...`: a preview, nothing is changed;
//!  - `mind setup <client> ... --apply`: registers after an explicit confirmation
//!    (`--yes` when there is no terminal), then READS THE REGISTRATION BACK
//!    through the client before it reports success.
//!
//! Registration always goes through the client's own `mcp add` command; no
//! client configuration file is edited here. Nothing is shared by default:
//! without `--ro`/`--rw` the agent only gets the memory tools. The generated
//! command never contains `--insecure-no-sandbox`.

pub mod client;
pub mod exec;
pub mod roots;
mod text;
pub use text::HELP_AFTER;
mod wizard;

use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use crate::core::{Error, Result};
use crate::mcp::Access;
use roots::Root;

pub use wizard::run as run_wizard;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Client {
    #[value(name = "claude-code")]
    ClaudeCode,
    #[value(name = "codex")]
    Codex,
    #[value(name = "opencode")]
    OpenCode,
}

pub const ALL_CLIENTS: [Client; 3] = [Client::ClaudeCode, Client::Codex, Client::OpenCode];

/// Where Claude Code stores the registration. (Codex and OpenCode have one place.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Scope {
    /// All your Claude Code sessions (the default).
    User,
    /// Only sessions started in the directory you run setup from.
    Local,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::User => "user",
            Scope::Local => "local",
        }
    }
}

/// What the person asked for.
#[derive(Debug, Clone)]
pub struct Request {
    pub client: Client,
    pub scope: Scope,
    pub ro: Vec<PathBuf>,
    pub rw: Vec<PathBuf>,
    pub apply: bool,
    pub yes: bool,
}

/// Everything setup reads from its surroundings (so tests can supply it).
#[derive(Debug, Clone)]
pub struct Context {
    /// The database the registration points at.
    pub db: PathBuf,
    /// The mind binary that will be registered.
    pub exe: PathBuf,
    pub path_env: Option<OsString>,
    pub home: Option<PathBuf>,
    pub codex_home: Option<OsString>,
    /// stdin and stdout are terminals.
    pub interactive: bool,
    /// Run the sandbox self-test when folders are shared.
    pub selftest: bool,
    /// Deadline for each call of the client's own command line.
    pub client_timeout: std::time::Duration,
}

/// Input and output of a session.
pub struct Ui<'a> {
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
}

impl Ui<'_> {
    pub fn say(&mut self, s: &str) {
        let _ = writeln!(self.out, "{s}");
    }
    /// Prints `prompt` and reads one trimmed line; `None` at end of input.
    pub fn ask(&mut self, prompt: &str) -> Option<String> {
        let _ = write!(self.out, "{prompt} ");
        let _ = self.out.flush();
        let mut line = String::new();
        match self.input.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim().to_string()),
        }
    }
}

/// A fully validated registration.
#[derive(Debug, Clone)]
pub struct Plan {
    pub client: Client,
    pub scope: Scope,
    pub exe: PathBuf,
    pub db: PathBuf,
    pub roots: Vec<Root>,
}

impl Plan {
    /// The arguments of the registered server: `--db <db> mcp --allow-root <dir>:<mode> ...`.
    /// Never contains the sandbox opt-out.
    pub fn server_args(&self) -> Vec<String> {
        let mut a = vec![
            "--db".to_string(),
            self.db.display().to_string(),
            "mcp".to_string(),
        ];
        for r in &self.roots {
            a.push("--allow-root".into());
            a.push(r.spec());
        }
        a
    }
    pub fn has_rw(&self) -> bool {
        self.roots.iter().any(|r| r.access == Access::ReadWrite)
    }
}

/// POSIX-shell quoting for DISPLAYING a command. (Commands are never run through
/// a shell: the client is started with an argument vector.)
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-=:,@+%".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub fn shell_line(program: &str, args: &[String]) -> String {
    let mut parts = vec![shell_quote(program)];
    parts.extend(args.iter().map(|a| shell_quote(a)));
    parts.join(" ")
}

/// Validates everything that does not need the client.
pub fn build_plan(req: &Request, ctx: &Context) -> Result<Plan> {
    let db = std::fs::canonicalize(&ctx.db).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::Setup(format!(
                "no database at {}; run: mind init (setup never creates it)",
                ctx.db.display()
            ))
        } else {
            Error::Setup(format!(
                "cannot read the database path {}: {e}",
                ctx.db.display()
            ))
        }
    })?;
    match crate::storage::sqlite::inspect_database(&db) {
        Ok(i) if !i.schema_versions.is_empty() => {}
        _ => {
            return Err(Error::Setup(format!(
                "{} is not an initialized Mindspace database; run: mind doctor",
                db.display()
            )))
        }
    }
    if db.to_str().is_none() {
        return Err(Error::Setup(format!(
            "the database path {} is not valid UTF-8 and could not be registered faithfully",
            db.display()
        )));
    }
    let db_dir = db.parent().map(PathBuf::from).unwrap_or_default();
    let roots = roots::validate(&req.ro, &req.rw, &db_dir, ctx.home.as_deref())?;
    let exe = std::fs::canonicalize(&ctx.exe).map_err(|e| {
        Error::Setup(format!(
            "cannot locate the mind binary {}: {e}",
            ctx.exe.display()
        ))
    })?;
    if exe.to_str().is_none() {
        return Err(Error::Setup(format!(
            "the mind binary path {} is not valid UTF-8 and could not be registered faithfully",
            exe.display()
        )));
    }
    Ok(Plan {
        client: req.client,
        scope: req.scope,
        exe,
        db,
        roots,
    })
}

/// What the sandbox self-test found, for the preview.
enum SandboxView {
    NotNeeded,
    Enforced,
    Unavailable(String),
    Unknown(String),
}

fn sandbox_view(plan: &Plan, ctx: &Context) -> SandboxView {
    if plan.roots.is_empty() {
        return SandboxView::NotNeeded;
    }
    #[cfg(target_os = "linux")]
    {
        use crate::doctor::selftest::{run, KernelEvidence};
        if !ctx.selftest {
            return SandboxView::Unknown("not checked".into());
        }
        match run(&plan.exe, std::time::Duration::from_secs(30)) {
            Ok(r) => match r.kernel {
                KernelEvidence::Enforced(_) => SandboxView::Enforced,
                KernelEvidence::Unavailable(why) => SandboxView::Unavailable(why),
                other => SandboxView::Unknown(format!("{other:?}")),
            },
            Err(e) => SandboxView::Unknown(e.to_string()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = ctx;
        SandboxView::Unavailable("the kernel sandbox exists only on Linux".into())
    }
}

fn describe_roots(plan: &Plan) -> Vec<String> {
    if plan.roots.is_empty() {
        return vec![
            "  (none)  zero-permission setup: the agent gets all the memory tools and Mindspace can read no repository files".into(),
            "          (to share folders later: undo this registration and run setup again with --ro)".into(),
        ];
    }
    let mut v = Vec::new();
    for r in &plan.roots {
        let tag = match r.access {
            Access::ReadOnly => "READ-ONLY ",
            Access::ReadWrite => "READ-WRITE",
        };
        v.push(format!("  {tag}  {}", r.canonical.display()));
        for n in &r.notes {
            v.push(format!("              note: {n}"));
        }
    }
    v
}

fn print_preview(
    ui: &mut Ui,
    req: &Request,
    plan: &Plan,
    detection: &client::Detection,
    existing: &Option<std::result::Result<client::Existing, String>>,
    sandbox: &SandboxView,
) {
    let c = plan.client;
    ui.say(&format!("Mindspace setup preview: {}", c.label()));
    ui.say(if req.apply {
        "Nothing has been changed yet."
    } else {
        "This is a preview. Nothing is changed."
    });
    ui.say("");
    ui.say("What would be registered");
    ui.say(&format!("  server name : {}", client::SERVER_NAME));
    if c == Client::ClaudeCode {
        ui.say(&format!(
            "  scope       : {} ({})",
            plan.scope.as_str(),
            match plan.scope {
                Scope::User => "all your Claude Code sessions",
                Scope::Local => "only sessions started in the directory you ran setup from",
            }
        ));
    }
    ui.say(&format!("  binary      : {}", plan.exe.display()));
    ui.say(&format!(
        "  database    : {} (a local file, exists)",
        plan.db.display()
    ));
    ui.say("");
    ui.say("Folders shared with the agent through Mindspace (absolute, links resolved)");
    for l in describe_roots(plan) {
        ui.say(&l);
    }
    ui.say("");
    match sandbox {
        SandboxView::NotNeeded => ui.say("Sandbox      : not needed, no folder is shared"),
        SandboxView::Enforced => ui.say("Sandbox      : the kernel sandbox is enforced on this machine (self-test passed)"),
        SandboxView::Unavailable(why) => ui.say(&format!(
            "Sandbox      : NOT available here ({why}). The repository tools will stay switched off; the memory tools work"
        )),
        SandboxView::Unknown(why) => ui.say(&format!("Sandbox      : could not be checked ({why}); run: mind doctor")),
    }
    match detection.paths.len() {
        0 => ui.say(&format!(
            "Client       : `{}` was not found on PATH",
            c.program()
        )),
        1 => ui.say(&format!("Client       : {}", detection.paths[0].display())),
        _ => {
            ui.say(&format!(
                "Client       : AMBIGUOUS, several `{}` on PATH:",
                c.program()
            ));
            for p in &detection.paths {
                ui.say(&format!("                 {}", p.display()));
            }
        }
    }
    match existing {
        None if !req.apply => {
            ui.say("Existing entry: checked when you apply (a preview never runs the client)")
        }
        None => {}
        Some(Ok(client::Existing::Missing)) => ui.say("Existing entry: none called `mindspace`"),
        Some(Ok(client::Existing::Present(d))) => ui.say(&format!(
            "Existing entry: `mindspace` is ALREADY registered ({d}). Setup will not overwrite it"
        )),
        Some(Err(e)) => ui.say(&format!("Existing entry: could not be checked ({e})")),
    }
    ui.say("");
    let program = detection
        .first()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| c.program().to_string());
    ui.say("The registration is this command (run by setup itself, never through a shell):");
    ui.say(&format!(
        "  {}",
        shell_line(&program, &client::add_args(plan))
    ));
    ui.say(&format!("To undo: {}", client::remove_command(plan)));
    ui.say("");
    for l in text::PERMISSIONS.lines().chain(text::LIMITS.lines()) {
        ui.say(l);
    }
}

fn confirm(
    ui: &mut Ui,
    req: &Request,
    ctx: &Context,
    plan: &Plan,
    sandbox: &SandboxView,
) -> Result<()> {
    if req.yes {
        return Ok(());
    }
    if !ctx.interactive {
        return Err(Error::Setup(
            "this is not a terminal, so nothing was changed. To register without a prompt, add --yes (with --rw folders, --yes means you accept write access)".into(),
        ));
    }
    if matches!(sandbox, SandboxView::Unavailable(_)) {
        ui.say("Reminder: the sandbox is not available here, the repository tools will stay off.");
    }
    let cancelled = || Error::Setup("cancelled; nothing was changed".into());
    if plan.has_rw() {
        ui.say("");
        ui.say(
            "You asked for READ-WRITE access. Mindspace will be allowed to change files inside:",
        );
        for r in plan.roots.iter().filter(|r| r.access == Access::ReadWrite) {
            ui.say(&format!("  {}", r.canonical.display()));
        }
        match ui.ask("Type 'yes' to allow write access (anything else cancels):") {
            Some(a) if a == "yes" => {}
            _ => return Err(cancelled()),
        }
    }
    match ui.ask(&format!(
        "Register Mindspace with {} now? [y/N]",
        plan.client.label()
    )) {
        Some(a) if matches!(a.to_lowercase().as_str(), "y" | "yes") => Ok(()),
        _ => Err(cancelled()),
    }
}

/// Runs one request: preview, and with `apply` the registration and its check.
pub fn run(req: &Request, ctx: &Context, ui: &mut Ui) -> Result<()> {
    let plan = build_plan(req, ctx)?;
    let c = req.client;
    let detection = client::detect(c, ctx.path_env.as_deref());
    // Existing entries are looked up only when applying: running the client can create
    // files in its own directories, which a preview must not do.
    let existing = match detection.first() {
        Some(p) if req.apply && c.can_apply() && !detection.ambiguous() => {
            Some(client::existing(c, p, ctx.client_timeout).map_err(|e| e.to_string()))
        }
        _ => None,
    };
    let sandbox = sandbox_view(&plan, ctx);
    print_preview(ui, req, &plan, &detection, &existing, &sandbox);
    ui.say("");

    if !req.apply {
        if c.can_apply() {
            ui.say("To register this, run the same command with --apply (and --yes when there is no terminal).");
        } else {
            ui.say(&format!(
                "{} is preview-only for now: `opencode mcp add` overwrites an existing entry without asking and there is no reliable way to verify the result, so setup will not apply it. Run the command above yourself if you want it (see docs/setup.md).",
                c.label()
            ));
        }
        return Ok(());
    }

    if !c.can_apply() {
        return Err(Error::Setup(format!(
            "{} cannot be applied by setup (preview only, see above); nothing was changed",
            c.label()
        )));
    }
    let Some(program) = detection.first() else {
        return Err(Error::Setup(format!(
            "`{}` was not found on PATH; install it or fix PATH. Nothing was changed",
            c.program()
        )));
    };
    if detection.ambiguous() {
        return Err(Error::Setup(format!(
            "several different `{}` programs are on PATH; put the one you use first on PATH and run setup again. Nothing was changed",
            c.program()
        )));
    }
    match existing {
        Some(Ok(client::Existing::Missing)) => {}
        Some(Ok(client::Existing::Present(d))) => {
            return Err(Error::Setup(format!(
                "{} already has a `mindspace` entry ({d}); it was not changed. To replace it, remove it first ({}) and run setup again",
                c.label(),
                client::remove_command(&plan)
            )))
        }
        Some(Err(e)) => return Err(Error::Setup(format!("{e}; nothing was changed"))),
        None => return Err(Error::Setup("internal: the existence check did not run; nothing was changed".into())),
    }
    confirm(ui, req, ctx, &plan, &sandbox)?;

    let mut backup = None;
    if c == Client::Codex {
        let cfg = client::codex_config_path(ctx.codex_home.as_deref(), ctx.home.as_deref());
        match cfg {
            Some(path) => backup = client::backup_codex_config(&path)?,
            None => {
                ui.say("note: could not locate Codex's config.toml, so no backup copy was made")
            }
        }
        if let Some(b) = &backup {
            ui.say(&format!(
                "Backup of your Codex configuration: {} (Codex rewrites other servers' entries when it adds one; comments inside them can be lost)",
                b.display()
            ));
        }
    }

    // Claude's `add` refuses a name that exists, so a race ends in its error. Codex's (and
    // OpenCode's) would silently replace an entry that another process registered since the
    // first check: look once more, as late as possible. A window of a few milliseconds remains
    // (the two steps are not atomic); the backup above holds the state from just before it.
    if c.add_overwrites() {
        if let client::Existing::Present(d) = client::existing(c, program, ctx.client_timeout)? {
            let b = backup
                .as_ref()
                .map(|b| format!(" Your configuration is saved at {}.", b.display()))
                .unwrap_or_default();
            return Err(Error::Setup(format!(
                "a `mindspace` entry appeared while setup was running ({d}); it was not changed and nothing was registered.{b}"
            )));
        }
    }

    if let Err(e) = client::register(program, &plan, ctx.client_timeout) {
        // A failed `add` may still have changed something: say what is there now.
        let state = match client::existing(c, program, ctx.client_timeout) {
            Ok(client::Existing::Missing) => "No `mindspace` entry exists now; the client's configuration is as before.".to_string(),
            Ok(client::Existing::Present(d)) => format!(
                "A `mindspace` entry EXISTS now ({d}). Inspect it, and remove it if it is not what you want: {}",
                client::remove_command(&plan)
            ),
            Err(_) => "The state afterwards could not be checked; run the client's `mcp get mindspace` yourself.".to_string(),
        };
        let b = backup
            .map(|b| format!(" Your previous config is saved at {}.", b.display()))
            .unwrap_or_default();
        return Err(Error::Setup(format!("{e}\n{state}{b}")));
    }

    let undo = client::remove_command(&plan);
    let backup_note = backup
        .map(|b| format!("\nBackup of the previous configuration: {}", b.display()))
        .unwrap_or_default();
    match client::verify(program, &plan, ctx.client_timeout) {
        client::Verdict::Verified {
            connected: Some(true),
            ..
        } => {
            ui.say(&format!(
                "Registered and verified: {} reports the `mindspace` server as connected.",
                c.label()
            ));
        }
        client::Verdict::Verified {
            connected: None,
            detail,
        } => {
            ui.say(&format!(
                "Registered and read back: {} reports exactly the command above. ({detail})",
                c.label()
            ));
        }
        client::Verdict::Verified {
            connected: Some(false),
            detail,
        } => {
            return Err(Error::Setup(format!(
                "registered, but {} could NOT start the server ({detail}). Run: mind doctor. To undo: {undo}{backup_note}",
                c.label()
            )));
        }
        client::Verdict::Mismatch(m) => {
            return Err(Error::Setup(format!(
                "registered, but {} reports something different: {m}. The registration is NOT verified. Inspect it with `{} mcp get mindspace`; to undo: {undo}{backup_note}",
                c.label(),
                c.program()
            )));
        }
        client::Verdict::Unreadable(m) => {
            return Err(Error::Setup(format!(
                "registered, but it could not be read back ({m}). The registration is NOT verified. To undo: {undo}{backup_note}"
            )));
        }
    }
    if plan.roots.is_empty() {
        ui.say("No folder is shared (zero-permission setup): the memory tools are available, the repository tools stay off.");
    }
    ui.say("Next:");
    ui.say("  1. Restart your client (or start a new session) so it loads the server.");
    ui.say("  2. Run: mind doctor");
    ui.say(&format!("  To undo: {undo}"));
    Ok(())
}

/// For `mind setup` without a client when there is no terminal: report what was
/// found and change nothing.
pub fn print_detection(ctx: &Context, ui: &mut Ui) {
    ui.say("mind setup registers Mindspace with a coding agent. Nothing is changed unless you pass --apply.");
    ui.say("");
    ui.say("Clients found on PATH:");
    for c in ALL_CLIENTS {
        let d = client::detect(c, ctx.path_env.as_deref());
        match d.first() {
            None => ui.say(&format!("  {:<12} not found", c.slug())),
            Some(p) => {
                let amb = if d.ambiguous() {
                    "  (AMBIGUOUS: several on PATH)"
                } else {
                    ""
                };
                ui.say(&format!("  {:<12} {}{amb}", c.slug(), p.display()));
            }
        }
    }
    ui.say("");
    ui.say("Preview a client:  mind setup <claude-code|codex|opencode> [--ro DIR] [--rw DIR]");
    ui.say("Guided setup:      run `mind setup` in a terminal");
    ui.say(text::EXAMPLE_NOTE);
}
