pub mod render;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::core::{normalize_subject, Error, MemoryActionKind, Result, SearchQuery, Ts};
use crate::ingest;
use crate::retrieval;
use crate::storage::Store;
use crate::{initialize, open_existing, resolve_memory_id, SqliteStore};

#[derive(Parser)]
#[command(
    name = "mind",
    version,
    about = "Local-first memory engine: sources are evidence, memories are interpretations."
)]
pub struct Cli {
    /// Path to the mindspace database (default: $XDG_DATA_HOME/mindspace/mind.db)
    #[arg(long, global = true, env = "MINDSPACE_DB", value_name = "PATH")]
    pub db: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new mindspace database (idempotent)
    Init,
    /// Ingest a file as a source (markdown or plain text)
    Ingest {
        path: PathBuf,
        #[arg(long = "space", required = true)]
        spaces: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    Space {
        #[command(subcommand)]
        command: SpaceCommand,
    },
    /// Full-text search over active memories
    Search {
        query: String,
        #[arg(long, value_name = "SPACE")]
        space: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Show memories as they were at this RFC 3339 time
        #[arg(long, value_name = "RFC3339")]
        at: Option<String>,
        #[arg(long)]
        include_superseded: bool,
        #[arg(long)]
        include_retired: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show a memory with its full provenance chain
    Inspect {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the memory -> assertion -> evidence -> source chain
    Trace {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the lifecycle of all memories with this subject
    History {
        #[arg(required = true, num_args = 1..)]
        subject: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// List conflicting memories (same subject, same space, both active)
    Conflicts {
        #[arg(long)]
        json: bool,
    },
    /// Retire an active memory (it stays inspectable)
    Retire {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum SpaceCommand {
    Create {
        name: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        json: bool,
    },
    List {
        #[arg(long)]
        json: bool,
    },
    Link {
        from: String,
        to: String,
        #[arg(long)]
        json: bool,
    },
}

pub fn execute(cli: &Cli) -> Result<()> {
    match &cli.command {
        Command::Init => {
            let path = db_path(cli);
            initialize(&path)?;
            println!("initialized mindspace at {}", path.display());
        }
        Command::Ingest { path, spaces, json } => {
            let store = open(cli)?;
            let report = ingest::ingest_path(&store, path, spaces)?;
            render::observation_report(&report, *json);
        }
        Command::Space { command } => match command {
            SpaceCommand::Create {
                name,
                description,
                json,
            } => {
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Err(Error::EmptyName);
                }
                let store = open(cli)?;
                let space = store.create_space(&name, description.as_deref().unwrap_or(""))?;
                render::space_created(&space, *json);
            }
            SpaceCommand::List { json } => {
                let store = open(cli)?;
                let spaces = store.list_spaces_with_counts()?;
                render::space_list(&spaces, *json);
            }
            SpaceCommand::Link { from, to, json } => {
                let store = open(cli)?;
                let from_space = store.get_space(from)?;
                let to_space = store.get_space(to)?;
                store.link_spaces(from_space.id, to_space.id)?;
                render::space_linked(&from_space, &to_space, *json);
            }
        },
        Command::Search {
            query,
            space,
            limit,
            at,
            include_superseded,
            include_retired,
            json,
        } => {
            let store = open(cli)?;
            let space_ids = match space {
                Some(name) => Some(vec![store.get_space(name)?.id]),
                None => None,
            };
            let at = at.as_deref().map(Ts::parse).transpose()?;
            let hits = retrieval::search(
                &store,
                SearchQuery {
                    text: query.clone(),
                    space_ids,
                    limit: *limit,
                    at,
                    include_superseded: *include_superseded,
                    include_retired: *include_retired,
                },
            )?;
            render::search_hits(&hits, *json);
        }
        Command::Inspect { id, json } => {
            let store = open(cli)?;
            let memory_id = resolve_memory_id(&store, id)?;
            let detail = store
                .get_memory_detail(memory_id)?
                .ok_or_else(|| Error::MemoryNotFound { prefix: id.clone() })?;
            render::inspect(&detail, *json);
        }
        Command::Trace { id, json } => {
            let store = open(cli)?;
            let memory_id = resolve_memory_id(&store, id)?;
            let detail = store
                .get_memory_detail(memory_id)?
                .ok_or_else(|| Error::MemoryNotFound { prefix: id.clone() })?;
            render::trace(&detail, *json);
        }
        Command::History { subject, json } => {
            let normalized = normalize_subject(&subject.join(" "));
            if normalized.is_empty() {
                return Err(Error::EmptyQuery);
            }
            let store = open(cli)?;
            let memories = store.list_memories_by_subject(&normalized)?;
            render::history(&normalized, &memories, *json);
        }
        Command::Conflicts { json } => {
            let store = open(cli)?;
            let conflicts = store.list_conflicts()?;
            render::conflicts(&conflicts, *json);
        }
        Command::Retire { id, reason } => {
            let store = open(cli)?;
            let memory_id = resolve_memory_id(&store, id)?;
            let memory = store.retire_memory(memory_id, reason.clone(), Ts::now())?;
            render::retired(&memory);
        }
    }
    Ok(())
}

fn db_path(cli: &Cli) -> PathBuf {
    cli.db.clone().unwrap_or_else(crate::default_db_path)
}

fn open(cli: &Cli) -> Result<SqliteStore> {
    open_existing(&db_path(cli))
}

/// Formats a memory action for human ingest output.
pub fn action_label(action: &MemoryActionKind) -> String {
    match action {
        MemoryActionKind::Created => "created".to_string(),
        MemoryActionKind::Reused { spaces_added } => {
            if spaces_added.is_empty() {
                "reused".to_string()
            } else {
                format!("reused (+{})", spaces_added.join(", "))
            }
        }
        MemoryActionKind::Superseded { old_memory_id } => {
            format!("superseded {}", old_memory_id)
        }
        MemoryActionKind::Unchanged => "unchanged".to_string(),
        MemoryActionKind::Retired => "retired".to_string(),
    }
}
