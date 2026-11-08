//! `mind grant`: the human's side of repository grants (ADR-030, M7.3a, level A1).
//!
//! Everything here runs as the user who types it. It cannot tell a human from
//! another program running under the same account, and says so.

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::Subcommand;
use serde_json::json;
use time::OffsetDateTime;

use crate::core::{Error, Result};
use crate::grants::{
    self, access_label, add_grant, deny_request, fmt_time, read_requests, revoke_grant,
    sanitize_display, NewGrant, PathPolicy, Status, Store, REQUEST_FILE,
};
use crate::mcp::Access;

const SAME_USER_NOTE: &str =
    "Note: this does not stop a program that runs as your user (for example an agent with \
shell access) from running `mind grant` itself. See docs/adr-030-jit-authorization.md.";

#[derive(Subcommand)]
pub enum GrantCommand {
    /// Allow `mind mcp --grants` to use a directory (read-only unless --rw).
    /// Shows the exact directory and asks for confirmation
    Add {
        path: PathBuf,
        /// Read-write: repo_ingest may write the database and a marker in .git
        #[arg(long)]
        rw: bool,
        /// Expire after this long: 30m, 12h, 7d (at most 365d). Default: no expiry
        #[arg(long, value_name = "DURATION", value_parser = grants::parse_ttl)]
        ttl: Option<Duration>,
        /// Confirm without asking (scripts, no terminal)
        #[arg(long)]
        yes: bool,
    },
    /// List grants (active ones; --all adds expired and revoked)
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Stop using a grant at the NEXT start of `mind mcp`. Running servers keep
    /// the access they started with until they are restarted
    Revoke {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// List access requests that agents recorded and nobody answered yet.
    /// The text of a request comes from the agent and is not trusted
    Requests {
        #[arg(long)]
        json: bool,
    },
    /// Turn a pending request into a grant after you looked at it
    Approve {
        request_id: String,
        /// Grant read-only even if the request asked for read-write
        #[arg(long)]
        ro: bool,
        #[arg(long, value_name = "DURATION", value_parser = grants::parse_ttl)]
        ttl: Option<Duration>,
        #[arg(long)]
        yes: bool,
    },
    /// Refuse a pending request (it is recorded and cannot be approved later)
    Deny { request_id: String },
}

struct Ctx {
    store: Store,
    policy: PathPolicy,
    request_file: PathBuf,
}

fn context(cli: &super::Cli) -> Result<Ctx> {
    let state = crate::paths::state_dir()?;
    let db = super::db_path(cli)?;
    let db_dir = db
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute());
    let policy = PathPolicy::standard(&state.path, Some(&db_dir), home.as_deref());
    Ok(Ctx {
        store: Store::in_dir(&state.path),
        request_file: db_dir.join(REQUEST_FILE),
        policy,
    })
}

fn confirm(question: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::Grant(
            "there is no terminal to ask on; nothing was granted. Run it in a terminal, or pass --yes"
                .into(),
        ));
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| Error::Grant(format!("cannot read the answer: {e}")))?;
    if matches!(line.trim(), "y" | "Y" | "yes") {
        Ok(())
    } else {
        Err(Error::Grant("not confirmed; nothing was granted".into()))
    }
}

fn describe(path: &std::path::Path, access: Access, ttl: Option<Duration>) -> String {
    let expiry = match ttl {
        Some(t) => format!("expires in {}s", t.as_secs()),
        None => "no expiry".to_string(),
    };
    let what = match access {
        Access::ReadOnly => "READ-ONLY: repo_outline works, repo_ingest is refused",
        Access::ReadWrite => {
            "READ-WRITE: repo_ingest may write the database and an identity marker into .git"
        }
    };
    format!(
        "  directory: {}\n  access:    {}\n  expiry:    {expiry}",
        path.display(),
        what
    )
}

fn created(g: &grants::Grant) {
    println!(
        "grant {} created ({}, {})",
        g.id,
        access_label(g.access),
        g.path.display()
    );
    println!(
        "It is used by `mind mcp --grants` at its next start. Running servers are not changed."
    );
    println!("{SAME_USER_NOTE}");
}

pub fn run(cli: &super::Cli, cmd: &GrantCommand) -> Result<()> {
    let ctx = context(cli)?;
    let now = OffsetDateTime::now_utc();
    match cmd {
        GrantCommand::Add { path, rw, ttl, yes } => {
            let access = if *rw {
                Access::ReadWrite
            } else {
                Access::ReadOnly
            };
            let root = grants::validate_root(path, &ctx.policy).map_err(Error::Grant)?;
            println!("Grant access to:\n{}", describe(&root.path, access, *ttl));
            confirm("Grant this?", *yes)?;
            let g = add_grant(
                &ctx.store,
                &ctx.policy,
                &NewGrant {
                    path: &root.path,
                    access,
                    ttl: *ttl,
                    request_id: None,
                    source: "cli-add",
                },
                now,
            )?;
            created(&g);
        }
        GrantCommand::List { all, json } => {
            let state = ctx.store.load()?;
            let rows: Vec<_> = state
                .records
                .iter()
                .filter(|r| *all || r.status(now) == Status::Active)
                .collect();
            if *json {
                let v: Vec<_> = rows
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.grant.id,
                            "path": r.grant.path.to_string_lossy(),
                            "access": access_label(r.grant.access),
                            "status": r.status(now).label(),
                            "created_at": fmt_time(r.grant.created_at),
                            "expires_at": r.grant.expires_at.map(fmt_time),
                            "revoked_at": r.revoked_at.map(fmt_time),
                            "request_id": r.grant.request_id,
                            "source": r.grant.source,
                            "last_start_refusal": r.last_skip,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else if rows.is_empty() {
                println!("no {}grants", if *all { "" } else { "active " });
            } else {
                for r in rows {
                    let exp = r
                        .grant
                        .expires_at
                        .map(fmt_time)
                        .unwrap_or_else(|| "never".into());
                    println!(
                        "{}  {}  {:<7}  expires {}  {}",
                        r.grant.id,
                        access_label(r.grant.access),
                        r.status(now).label(),
                        exp,
                        r.grant.path.display()
                    );
                    if let Some(why) = &r.last_skip {
                        println!("    last start refused it: {why}");
                    }
                }
                println!(
                    "Grants are used by `mind mcp --grants` at start; revoking does not change running servers."
                );
            }
        }
        GrantCommand::Revoke { id, reason } => {
            revoke_grant(&ctx.store, id, reason.as_deref(), now)?;
            println!("grant {id} revoked; the next `mind mcp` start will not use it");
            println!("Servers that are running keep their access until they are restarted.");
        }
        GrantCommand::Requests { json } => {
            let state = ctx.store.load()?;
            let (reqs, bad) = read_requests(&ctx.request_file);
            let pending: Vec<_> = reqs
                .iter()
                .filter(|r| !state.request_is_closed(&r.id))
                .collect();
            if *json {
                let v: Vec<_> = pending
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.id,
                            "requested_at": sanitize_display(&r.ts, 40),
                            "path": sanitize_display(&r.path, 1024),
                            "access": access_label(r.access),
                            "reason": r.reason.as_deref().map(|x| sanitize_display(x, 300)),
                            "untrusted": true,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else if pending.is_empty() {
                println!("no pending requests");
            } else {
                println!("(the text of a request comes from the agent and is not trusted)");
                for r in pending {
                    println!(
                        "{}  {}  {}",
                        r.id,
                        access_label(r.access),
                        sanitize_display(&r.path, 200)
                    );
                    if let Some(why) = &r.reason {
                        println!("    reason: {}", sanitize_display(why, 300));
                    }
                }
                println!("Answer with `mind grant approve <id>` or `mind grant deny <id>`.");
            }
            if bad > 0 {
                eprintln!("{bad} malformed line(s) in the request log were ignored");
            }
        }
        GrantCommand::Approve {
            request_id,
            ro,
            ttl,
            yes,
        } => {
            let state = ctx.store.load()?;
            let (reqs, _) = read_requests(&ctx.request_file);
            let req = reqs.iter().find(|r| &r.id == request_id).ok_or_else(|| {
                Error::Grant(format!(
                    "no request {request_id}; see `mind grant requests`"
                ))
            })?;
            if state.request_is_closed(&req.id) {
                return Err(Error::Grant(format!(
                    "request {} was already answered",
                    req.id
                )));
            }
            let access = if *ro { Access::ReadOnly } else { req.access };
            println!("An agent asked for access (its text is not trusted):");
            println!("  asked for: {}", sanitize_display(&req.path, 200));
            if let Some(why) = &req.reason {
                println!("  reason:    {}", sanitize_display(why, 300));
            }
            let root = grants::validate_root(std::path::Path::new(&req.path), &ctx.policy)
                .map_err(Error::Grant)?;
            println!(
                "This would grant (resolved, canonical):\n{}",
                describe(&root.path, access, *ttl)
            );
            confirm("Grant this?", *yes)?;
            let g = add_grant(
                &ctx.store,
                &ctx.policy,
                &NewGrant {
                    path: &root.path,
                    access,
                    ttl: *ttl,
                    request_id: Some(&req.id),
                    source: "cli-approve",
                },
                now,
            )?;
            created(&g);
        }
        GrantCommand::Deny { request_id } => {
            let (reqs, _) = read_requests(&ctx.request_file);
            if !reqs.iter().any(|r| &r.id == request_id) {
                return Err(Error::Grant(format!(
                    "no request {request_id}; see `mind grant requests`"
                )));
            }
            deny_request(&ctx.store, request_id, now)?;
            println!("request {request_id} denied");
        }
    }
    Ok(())
}
