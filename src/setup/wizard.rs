//! The guided session of `mind setup` (terminal only).

use std::path::PathBuf;

use super::{client, roots, text, Client, Context, Request, Scope, Ui, ALL_CLIENTS};
use crate::core::{Error, Result};
use crate::mcp::Access;

fn cancelled() -> Error {
    Error::Setup("cancelled; nothing was changed".into())
}

/// `~` and `~/x` mean the home directory in a shell, but a typed answer does not go through
/// one. Only a leading `~` is expanded, and only when the home directory is known.
fn expand_tilde(answer: &str, home: Option<&std::path::Path>) -> PathBuf {
    match (answer, home) {
        ("~", Some(h)) => h.to_path_buf(),
        (a, Some(h)) if a.starts_with("~/") => h.join(&a[2..]),
        _ => PathBuf::from(answer),
    }
}

/// Asks for folders until an empty answer. Each answer is validated at once so a
/// typo is caught while the person is still there.
fn ask_folders(ui: &mut Ui, ctx: &Context, access: Access) -> Result<Vec<PathBuf>> {
    let flag = if access == Access::ReadOnly {
        "read-only"
    } else {
        "READ-WRITE"
    };
    let mut out: Vec<PathBuf> = Vec::new();
    loop {
        let prompt = format!("Folder to share {flag} (full path, empty to continue):");
        let Some(answer) = ui.ask(&prompt) else {
            return Err(cancelled());
        };
        if answer.is_empty() {
            return Ok(out);
        }
        let candidate = expand_tilde(&answer, ctx.home.as_deref());
        let db_dir = std::fs::canonicalize(&ctx.db)
            .ok()
            .and_then(|d| d.parent().map(PathBuf::from))
            .unwrap_or_default();
        let (ro, rw) = if access == Access::ReadOnly {
            (vec![candidate.clone()], vec![])
        } else {
            (vec![], vec![candidate.clone()])
        };
        match roots::validate(&ro, &rw, &db_dir, ctx.home.as_deref()) {
            Ok(v) => {
                for r in v {
                    ui.say(&format!("  accepted: {}", r.canonical.display()));
                    for n in &r.notes {
                        ui.say(&format!("  note: {n}"));
                    }
                }
                out.push(candidate);
            }
            Err(e) => ui.say(&format!("  not accepted: {e}")),
        }
    }
}

/// The guided session. Ends in the same preview and confirmation as `--apply`.
pub fn run(ctx: &Context, ui: &mut Ui) -> Result<()> {
    ui.say(text::INTRO);
    ui.say("");
    ui.say(text::ZERO_PERMISSION);
    ui.say("");

    let found: Vec<(Client, client::Detection)> = ALL_CLIENTS
        .iter()
        .map(|c| (*c, client::detect(*c, ctx.path_env.as_deref())))
        .filter(|(_, d)| d.first().is_some())
        .collect();
    if found.is_empty() {
        ui.say("No supported client was found on PATH (claude, codex, opencode).");
        ui.say("Install one and run `mind setup` again, or preview a specific one with `mind setup <client>`.");
        return Ok(());
    }
    ui.say("Clients found:");
    for (i, (c, d)) in found.iter().enumerate() {
        let p = d.first().expect("filtered");
        let note = if d.ambiguous() {
            "  (AMBIGUOUS: several on PATH)"
        } else if !c.can_apply() {
            "  (preview only)"
        } else {
            ""
        };
        ui.say(&format!("  {}) {} {}{note}", i + 1, c.label(), p.display()));
    }
    let chosen = if found.len() == 1 {
        match ui.ask(&format!("Set up {}? [Y/n]", found[0].0.label())) {
            Some(a) if a.is_empty() || matches!(a.to_lowercase().as_str(), "y" | "yes") => {
                found[0].0
            }
            _ => return Err(cancelled()),
        }
    } else {
        let Some(a) = ui.ask("Which one? (number, empty to cancel):") else {
            return Err(cancelled());
        };
        match a.parse::<usize>() {
            Ok(n) if (1..=found.len()).contains(&n) => found[n - 1].0,
            _ => return Err(cancelled()),
        }
    };

    // Sharing folders is an optional extra, and the default answer is "no".
    ui.say("");
    ui.say("Optional: let Mindspace read repository folders. Most people start without; the memory tools work fully without any folder.");
    let (mut ro, mut rw) = (Vec::new(), Vec::new());
    match ui.ask("Share repository folders? [y/N]") {
        Some(a) if matches!(a.to_lowercase().as_str(), "y" | "yes") => {
            ui.say("");
            for l in text::PERMISSIONS.lines() {
                ui.say(l);
            }
            ui.say(text::EXAMPLE_NOTE);
            ui.say("");
            ro = ask_folders(ui, ctx, Access::ReadOnly)?;
            ui.say("Read-write is NOT recommended unless you need Mindspace to change repository files.");
            match ui.ask("Share any folder read-WRITE? [y/N]") {
                Some(a) if matches!(a.to_lowercase().as_str(), "y" | "yes") => {
                    rw = ask_folders(ui, ctx, Access::ReadWrite)?
                }
                Some(_) => {}
                None => return Err(cancelled()),
            }
        }
        Some(_) => {}
        None => return Err(cancelled()),
    }

    let mut scope = Scope::User;
    if chosen == Client::ClaudeCode {
        ui.say("");
        ui.say("Claude Code scope: 'user' = all your sessions (recommended), 'local' = only sessions started in the current directory.");
        match ui.ask("Scope [user/local, default user]:") {
            Some(a) if a.is_empty() || a == "user" => {}
            Some(a) if a == "local" => scope = Scope::Local,
            _ => return Err(cancelled()),
        }
    }
    ui.say("");
    let req = Request {
        client: chosen,
        scope,
        ro,
        rw,
        apply: chosen.can_apply(),
        yes: false,
    };
    super::run(&req, ctx, ui)
}
