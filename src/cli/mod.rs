#[cfg(unix)]
pub mod grant;
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

    /// How `--db` was supplied (flag or environment); set by [`parse`], used by
    /// `mind doctor` to say where the effective path came from.
    #[arg(skip)]
    pub db_origin: crate::paths::DbOrigin,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a new mindspace database (idempotent)
    Init,
    /// Register Mindspace as an MCP server with Claude Code, Codex CLI or OpenCode
    ///
    /// MCP (Model Context Protocol) is how a coding agent talks to local tools; this
    /// registers Mindspace with your agent. Mindspace keeps its data in one local
    /// database. Nothing is changed unless you pass --apply and confirm.
    ///
    /// `mind setup` in a terminal guides you. `mind setup <client>` shows a preview of
    /// what would be registered. `--apply` registers it through the client's own
    /// `mcp add` command and checks the result before reporting success.
    #[command(after_long_help = crate::setup::HELP_AFTER)]
    Setup {
        /// The client to set up. Without it, a terminal session guides you (and without a
        /// terminal the clients found are listed)
        #[arg(value_enum)]
        client: Option<crate::setup::Client>,
        /// Share a folder READ-ONLY: Mindspace may read repository files inside it and
        /// cannot change them (recommended). Repeatable; absolute path of an existing
        /// folder (~/projects is only an example). Default: no folder is shared
        #[arg(long = "ro", value_name = "DIR")]
        ro: Vec<PathBuf>,
        /// Share a folder READ-WRITE: Mindspace may also change repository files inside it.
        /// Repeatable; you are asked to confirm it explicitly
        #[arg(long = "rw", value_name = "DIR")]
        rw: Vec<PathBuf>,
        /// Claude Code only: 'user' = all your sessions (default), 'local' = only sessions
        /// started in the directory you run setup from
        #[arg(long, value_enum, default_value = "user")]
        scope: crate::setup::Scope,
        /// Register Mindspace (otherwise: preview only). Asks for confirmation
        #[arg(long)]
        apply: bool,
        /// Confirm without asking, for scripts and when there is no terminal. With --rw
        /// folders this means you accept write access
        #[arg(long)]
        yes: bool,
    },
    /// Diagnose the installation: binary, database, configuration, git,
    /// embeddings and the MCP sandbox. Read-only; exit code 0 = no blocking
    /// failure, 1 = a check failed, 2 = the diagnosis could not be carried out
    Doctor {
        /// Print the report as JSON (and nothing else) on stdout
        #[arg(long)]
        json: bool,
    },
    /// Ingest a file, directory, or git repository as sources
    Ingest {
        path: PathBuf,
        #[arg(long = "space", required = true)]
        spaces: Vec<String>,
        /// Only ingest this path below a directory or repository root
        /// (repeatable). See `mind scan` for an outline to choose from.
        #[arg(long = "path", value_name = "SUBPATH")]
        select: Vec<String>,
        /// Git: observe and interpret every ref tip, not only HEAD. Derived
        /// memories of different commits then coexist.
        #[arg(long)]
        history: bool,
        /// Git: also interpret local branch tips (HEAD and worktrees always are)
        #[arg(long, conflicts_with = "history")]
        branches: bool,
        /// Git: also interpret remote-tracking branch tips
        #[arg(long, conflicts_with = "history")]
        remotes: bool,
        /// Git: also interpret tag tips
        #[arg(long, conflicts_with = "history")]
        tags: bool,
        #[arg(long)]
        json: bool,
    },
    /// Outline a directory or git repository (at HEAD) without ingesting
    Scan {
        path: PathBuf,
        /// Directory levels to show below the root
        #[arg(long, default_value_t = 2)]
        depth: u32,
        #[arg(long)]
        json: bool,
    },
    Space {
        #[command(subcommand)]
        command: SpaceCommand,
    },
    /// Search memories with lexical, semantic, hybrid, or automatic retrieval
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
        /// Retrieval mode; omitted mode preserves legacy lexical search
        #[arg(long, value_enum)]
        mode: Option<SearchModeArg>,
    },
    /// Find definitions (functions, structs, ...) by name, signature, or path
    Symbols {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        /// Restrict to an entity kind (repeatable): fn, method, struct, enum,
        /// trait, type, const, static, mod, field, use
        #[arg(long = "kind", value_name = "KIND")]
        kinds: Vec<String>,
        /// Restrict to code attached to this space or any space it imports
        /// (repeatable)
        #[arg(long = "space", value_name = "SPACE")]
        spaces: Vec<String>,
        /// Restrict to a repository: owner/name slug, path substring, or id prefix
        #[arg(long, value_name = "REPO")]
        repo: Option<String>,
        /// Restrict to a path prefix (repository-relative for git sources)
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// Include definitions that are no longer part of a current view
        #[arg(long)]
        all_versions: bool,
        #[arg(long)]
        json: bool,
    },
    /// Recall the most relevant current memories with their provenance
    Recall {
        query: String,
        #[arg(long, value_name = "SPACE")]
        space: Option<String>,
        #[arg(long, default_value_t = 5)]
        limit: u32,
        #[arg(long)]
        json: bool,
        /// Retrieval mode; omitted mode preserves legacy lexical search
        #[arg(long, value_enum)]
        mode: Option<SearchModeArg>,
    },
    /// Show a memory with its full provenance chain
    Inspect {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Inspect deterministic syntax structure by canonical file path or Git blob id
    Structure {
        target: String,
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
    /// Manage trusted embedding models
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    /// Manage embedding functionality
    Embeddings {
        #[command(subcommand)]
        command: EmbeddingsCommand,
    },
    /// Index embeddings for stored assertions
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    /// Inspect a git repository: identity, HEAD, remotes with slugs, worktrees
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Repository grants: what `mind mcp --grants` may use besides --allow-root
    /// directories (ADR-030). A request from an agent is never a grant; only
    /// these commands, run by you, create one
    #[cfg(unix)]
    Grant {
        #[command(subcommand)]
        command: grant::GrantCommand,
    },
    /// EXPERIMENT (M7.3b.2a): one confined extraction worker, started by a parent process
    /// that hands it a root directory descriptor and a channel. Not for users.
    #[cfg(unix)]
    #[command(hide = true, name = "extraction-worker")]
    ExtractionWorker,
    /// Run the local MCP server over stdio
    Mcp {
        /// Directory the repo_outline and repo_ingest tools may use, as
        /// DIR[:ro|:rw]. repo_outline works on any root; repo_ingest needs a
        /// read-write one (the default without a suffix). Repeatable, or
        /// several joined with ':' in MINDSPACE_MCP_ALLOW_ROOTS
        /// (/a:ro:/b:rw). Without any root those tools are disabled.
        #[arg(
            long = "allow-root",
            value_name = "DIR[:ro|:rw]",
            env = "MINDSPACE_MCP_ALLOW_ROOTS"
        )]
        allow_roots: Vec<String>,
        /// Deadline for the PREPARATION of a repo_ingest, in seconds (0.001 to
        /// 3600). It is not an end-to-end timeout: until the database write
        /// starts it stops the ingest without changing anything and kills a
        /// hung git; the write, once started, runs to its end and may exceed
        /// the limit.
        #[arg(
            long = "ingest-timeout",
            value_name = "SECONDS",
            env = "MINDSPACE_MCP_INGEST_TIMEOUT",
            default_value = "30",
            value_parser = crate::mcp::parse_ingest_timeout
        )]
        ingest_timeout: std::time::Duration,
        /// Run the repository tools WITHOUT the kernel sandbox. Only the
        /// application layer (anchored access, pointer checks, ro/rw rules)
        /// protects then: a repository or a concurrent actor that exploits a gap
        /// there is no longer stopped by the kernel. A warning is printed on every
        /// start and every result says so. Use only for trusted input.
        #[arg(long = "insecure-no-sandbox")]
        insecure_no_sandbox: bool,
        /// Also use the directories you granted with `mind grant` (read at
        /// start, validated again, expired and revoked ones skipped). Without
        /// this flag the grant store is not read. A running server never gains
        /// access: restart it after `mind grant approve`
        #[arg(long)]
        grants: bool,
        /// Start the worker broker (stage 1 forks it outside the sandbox; M7.3b.3a). Hidden:
        /// no MCP tool uses it yet.
        #[arg(long, hide = true)]
        broker: bool,
    },
}

#[derive(Subcommand)]
pub enum RepoCommand {
    /// Show identity, HEAD, remotes with slugs, and worktrees (read-only)
    Info {
        path: PathBuf,
        #[arg(long)]
        json: bool,
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

#[derive(Subcommand)]
pub enum ModelCommand {
    /// List supported models and installation state
    List,
    /// Show metadata for a model
    Info {
        /// Model id, as listed by `mind model list`
        id: String,
    },
    /// Download and install a trusted model
    Install {
        /// Model id, as listed by `mind model list`
        id: String,
        /// Download without asking for confirmation (required when stdin is not
        /// a terminal). The files come from the model's source over HTTPS and
        /// are checked against the sizes and SHA-256 hashes in the built-in manifest.
        #[arg(long)]
        yes: bool,
    },
    /// Select an installed model
    Use {
        /// Model id, as listed by `mind model list`
        id: String,
    },
}

#[derive(Subcommand)]
pub enum EmbeddingsCommand {
    /// Enable embedding functionality
    Enable {
        #[arg(long)]
        yes: bool,
    },
    /// Disable embedding functionality (keeps files and vectors)
    Disable,
    /// Show embedding status
    Status,
}

#[derive(Subcommand)]
pub enum IndexCommand {
    /// Run incremental embedding indexing
    Run {
        #[arg(long, default_value_t = 32)]
        batch_size: u32,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Show indexing status
    Status,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum SearchModeArg {
    Lexical,
    Semantic,
    Hybrid,
    Auto,
}

impl From<SearchModeArg> for retrieval::SearchMode {
    fn from(mode: SearchModeArg) -> Self {
        match mode {
            SearchModeArg::Lexical => Self::Lexical,
            SearchModeArg::Semantic => Self::Semantic,
            SearchModeArg::Hybrid => Self::Hybrid,
            SearchModeArg::Auto => Self::Auto,
        }
    }
}

enum SearchResults {
    Legacy(Vec<crate::core::SearchHit>),
    Mode(retrieval::SearchOutcome),
}

fn search_for_cli(
    store: &dyn Store,
    query: SearchQuery,
    mode: Option<retrieval::SearchMode>,
) -> Result<SearchResults> {
    match mode {
        Some(mode) => Ok(SearchResults::Mode(
            retrieval::search_with_mode_resolving_capability(store, query, mode)?,
        )),
        None => Ok(SearchResults::Legacy(retrieval::search(store, query)?)),
    }
}

pub fn execute(cli: &Cli) -> Result<()> {
    match &cli.command {
        // Library callers get the report printed but not the exit code; the
        // binary goes through `run`.
        Command::Doctor { json } => {
            doctor_command(cli, *json)?;
        }
        Command::Setup {
            client,
            ro,
            rw,
            scope,
            apply,
            yes,
        } => setup_command(cli, *client, ro, rw, *scope, *apply, *yes)?,
        Command::Init => {
            let path = db_path(cli)?;
            initialize(&path)?;
            println!("initialized mindspace at {}", path.display());
            println!("next: mind space create <name>   (then: mind ingest <path> --space <name>)");
        }
        Command::Ingest {
            path,
            spaces,
            select,
            history,
            branches,
            remotes,
            tags,
            json,
        } => {
            let scope = if *history {
                ingest::GitScope::History
            } else if *branches || *remotes || *tags {
                ingest::GitScope::Views(ingest::GitViews {
                    branches: *branches,
                    remotes: *remotes,
                    tags: *tags,
                })
            } else {
                ingest::GitScope::Head
            };
            let selection = ingest::outline::PathSelection::new(select)?;
            let store = open(cli)?;
            let outcomes = ingest::ingest_paths(&store, path, &selection, scope, spaces)?;
            if *json && outcomes.len() > 1 {
                // One valid JSON document for several ingest roots.
                let values: Vec<serde_json::Value> = outcomes
                    .iter()
                    .map(|o| match o {
                        crate::core::IngestOutcome::Files(r) => serde_json::to_value(r),
                        crate::core::IngestOutcome::Git(r) => serde_json::to_value(r),
                    })
                    .collect::<std::result::Result<_, _>>()
                    .map_err(|e| Error::Storage(e.to_string()))?;
                println!(
                    "{}",
                    serde_json::to_string_pretty(&values).expect("values serialize")
                );
            } else {
                for outcome in outcomes {
                    match outcome {
                        crate::core::IngestOutcome::Files(report) => {
                            render::observation_report(&report, *json)
                        }
                        crate::core::IngestOutcome::Git(report) => {
                            render::git_observation_report(&report, *json)
                        }
                    }
                }
            }
        }
        Command::Repo { command } => match command {
            RepoCommand::Info { path, json } => {
                render::repo_info(&ingest::repo_info::repo_info(path)?, *json);
            }
        },
        Command::Scan { path, depth, json } => {
            render::outline(&ingest::scan_path(path, *depth)?, *json);
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
        Command::Symbols {
            query,
            kinds,
            spaces,
            repo,
            path,
            limit,
            all_versions,
            json,
        } => {
            let store = open(cli)?;
            let space_ids = if spaces.is_empty() {
                None
            } else {
                Some(
                    spaces
                        .iter()
                        .map(|name| store.get_space(name).map(|s| s.id))
                        .collect::<Result<Vec<_>>>()?,
                )
            };
            let hits = retrieval::search_code(
                &store,
                crate::core::CodeSearchQuery {
                    text: query.join(" "),
                    kinds: retrieval::parse_code_kinds(kinds)?,
                    repository: repo.clone(),
                    path_prefix: path.clone(),
                    space_ids,
                    limit: *limit,
                    all_versions: *all_versions,
                },
            )?;
            render::code_hits(&hits, *json);
        }
        Command::Structure { target, json } => {
            let store = open(cli)?;
            let target = std::fs::canonicalize(target)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| target.clone());
            render::code_structure(&store.code_structure(&target)?, *json);
        }
        Command::Search {
            query,
            space,
            limit,
            at,
            include_superseded,
            include_retired,
            json,
            mode,
        } => {
            let store = open(cli)?;
            let space_ids = match space {
                Some(name) => Some(vec![store.get_space(name)?.id]),
                None => None,
            };
            let at = at.as_deref().map(Ts::parse).transpose()?;
            let results = search_for_cli(
                &store,
                SearchQuery {
                    text: query.clone(),
                    space_ids,
                    limit: *limit,
                    at,
                    include_superseded: *include_superseded,
                    include_retired: *include_retired,
                },
                mode.map(Into::into),
            )?;
            match results {
                SearchResults::Legacy(hits) => render::search_hits(&hits, *json),
                SearchResults::Mode(outcome) => render::search_outcome(&outcome, *json),
            }
        }
        Command::Recall {
            query,
            space,
            limit,
            json,
            mode,
        } => {
            let store = open(cli)?;
            let space_ids = match space {
                Some(name) => Some(vec![store.get_space(name)?.id]),
                None => None,
            };
            let results = search_for_cli(
                &store,
                SearchQuery {
                    text: query.clone(),
                    space_ids,
                    limit: *limit,
                    at: None,
                    include_superseded: false,
                    include_retired: false,
                },
                mode.map(Into::into),
            )?;
            match results {
                SearchResults::Legacy(hits) => render::recall_hits(&hits, *json),
                SearchResults::Mode(outcome) => render::recall_outcome(&outcome, *json),
            }
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
        Command::Model { command } => match command {
            ModelCommand::List => {
                for (m, installed) in crate::embeddings::manager::list_models() {
                    println!(
                        "{}  v{}  dim={}  installed={}",
                        m.id, m.version, m.dimension, installed
                    );
                }
            }
            ModelCommand::Info { id } => {
                let m = crate::embeddings::manager::model_info(id)?;
                println!("id: {}", m.id);
                println!("version: {}", m.version);
                println!("revision: {}", m.revision);
                println!("architecture: {}", m.architecture);
                println!("runtime: {}", m.runtime);
                println!("runtime_version: {}", m.runtime_version);
                println!("dimension: {}", m.dimension);
                println!("max_seq_len: {}", m.max_seq_len);
                println!("prefix_document: {:?}", m.prefix_document);
                println!("prefix_query: {:?}", m.prefix_query);
                println!("pooling: {}", m.pooling);
                println!("normalization: {}", m.normalization);
                println!("license: {}", m.license);
                println!("source: {}", m.source);
                println!("installed: {}", crate::embeddings::manager::is_installed(m));
                for a in m.artifacts {
                    let p = crate::embeddings::manager::model_dir(m.id)?.join(a.filename);
                    let verified = crate::embeddings::manager::verify_file(&p, a).is_ok();
                    println!(
                        "  artifact {} size={} sha256={} verified={}",
                        a.filename, a.size, a.sha256, verified
                    );
                }
            }
            ModelCommand::Install { id, yes } => {
                let m = crate::embeddings::manager::model_info(id)?;
                println!(
                    "Model: {} v{} ({})
Format: safetensors (FP32)
Size: {} bytes
Source: {}
License: {}",
                    m.id,
                    m.version,
                    m.architecture,
                    m.artifacts.iter().map(|a| a.size).sum::<u64>(),
                    m.source,
                    m.license
                );
                let tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
                if !*yes {
                    if !tty {
                        return Err(Error::Storage(
                            "noninteractive install requires --yes".to_string(),
                        ));
                    }
                    print!("Download and install? [y/N] ");
                    use std::io::Write;
                    std::io::stdout().flush().ok();
                    let mut line = String::new();
                    std::io::stdin()
                        .read_line(&mut line)
                        .map_err(|e| Error::Storage(e.to_string()))?;
                    let ans = line.trim().to_lowercase();
                    if ans != "y" && ans != "yes" {
                        println!("cancelled");
                        return Ok(());
                    }
                }
                crate::embeddings::manager::install_model(m)?;
                println!("installed {}", m.id);
                println!(
                    "note: a `mind mcp` server with repository roots that was started before the \
                     first model was installed cannot read the new model directory (its sandbox \
                     is built at start); restart it. Next: mind model use {}",
                    m.id
                );
            }
            ModelCommand::Use { id } => {
                let store = open(cli)?;
                crate::embeddings::manager::set_selected_model(&store, id)?;
                println!("selected model: {}", id);
            }
        },
        Command::Embeddings { command } => match command {
            EmbeddingsCommand::Enable { yes } => {
                let store = open(cli)?;
                // install default model if needed
                let default = crate::embeddings::manifest::default_model();
                if !crate::embeddings::manager::is_installed(default) {
                    println!(
                        "Model: {} v{} ({})
Format: safetensors (FP32)
Size: {} bytes
Source: {}
License: {}",
                        default.id,
                        default.version,
                        default.architecture,
                        default.artifacts.iter().map(|a| a.size).sum::<u64>(),
                        default.source,
                        default.license
                    );
                    let tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
                    if !*yes {
                        if !tty {
                            return Err(Error::Storage(
                                "noninteractive enable requires --yes".to_string(),
                            ));
                        }
                        print!("Download and install? [y/N] ");
                        use std::io::Write;
                        std::io::stdout().flush().ok();
                        let mut line = String::new();
                        std::io::stdin()
                            .read_line(&mut line)
                            .map_err(|e| Error::Storage(e.to_string()))?;
                        let ans = line.trim().to_lowercase();
                        if ans != "y" && ans != "yes" {
                            println!("cancelled; embeddings remain disabled");
                            return Ok(());
                        }
                    }
                    crate::embeddings::manager::install_model(default)?;
                }
                // Preserve an existing selected model if it is installed; otherwise select default.
                let current = crate::embeddings::manager::selected_model(&store);
                if current
                    .as_deref()
                    .map(|id| {
                        crate::embeddings::manager::is_installed(
                            crate::embeddings::manifest::find_model(id).unwrap_or(default),
                        )
                    })
                    .unwrap_or(false)
                {
                    // keep existing
                } else {
                    crate::embeddings::manager::set_selected_model(&store, default.id)?;
                }
                crate::embeddings::manager::set_enabled(&store, true)?;
                let sel = crate::embeddings::manager::selected_model(&store)
                    .unwrap_or_else(|| default.id.to_string());
                println!("embeddings enabled (model: {})", sel);
            }
            EmbeddingsCommand::Disable => {
                let store = open(cli)?;
                crate::embeddings::manager::set_enabled(&store, false)?;
                println!("embeddings disabled; installed models and vectors preserved");
            }
            EmbeddingsCommand::Status => {
                let store = open(cli)?;
                let st = crate::embeddings::manager::status(&store)?;
                println!("enabled: {}", st.enabled);
                println!("selected_model: {:?}", st.selected_model);
                println!("installed: {}", st.installed);
                println!("verified: {}", st.verified);
                println!("dimension: {:?}", st.dimension);
                println!("runtime: {}", st.runtime);
                println!("indexing_available: {}", st.indexing_available);
            }
        },
        Command::Index { command } => match command {
            IndexCommand::Run { batch_size, limit } => {
                let store = open(cli)?;
                if !crate::embeddings::manager::is_enabled(&store) {
                    return Err(Error::Storage(
                        "embeddings disabled; run: mind embeddings enable".to_string(),
                    ));
                }
                let selected =
                    crate::embeddings::manager::selected_model(&store).ok_or_else(|| {
                        Error::Storage("no model selected; run: mind model use <id>".to_string())
                    })?;
                let entry = crate::embeddings::manifest::find_model(&selected)
                    .ok_or_else(|| Error::Storage(format!("unknown model: {selected}")))?;
                if !crate::embeddings::manager::is_installed(entry) {
                    return Err(Error::Storage(format!(
                        "model '{}' not installed; run: mind model install {}",
                        selected, selected
                    )));
                }
                #[cfg(feature = "embeddings")]
                {
                    let dir = crate::embeddings::manager::model_dir(&selected)?;
                    let rt = crate::embeddings::CandleRuntime::load(&dir)?;
                    let deriv = crate::embeddings::manifest::derivation_from_manifest(entry);
                    let report = crate::indexing::index_assertions(
                        &store,
                        &rt,
                        &deriv,
                        *batch_size,
                        *limit,
                    )?;
                    println!("derivation_key: {}", report.derivation_key);
                    println!("eligible: {}", report.eligible);
                    println!("already_indexed: {}", report.already_indexed);
                    println!("indexed: {}", report.indexed);
                    println!("failed: {}", report.failed);
                    println!("remaining: {}", report.remaining);
                    for f in &report.failures {
                        eprintln!("  failed {}: {}", f.assertion_id, f.error);
                    }
                }
                #[cfg(not(feature = "embeddings"))]
                {
                    let _ = (batch_size, limit);
                    return Err(Error::Storage(
                        "embeddings feature disabled; rebuild with --features embeddings"
                            .to_string(),
                    ));
                }
            }
            IndexCommand::Status => {
                let store = open(cli)?;
                let enabled = crate::embeddings::manager::is_enabled(&store);
                let selected = crate::embeddings::manager::selected_model(&store);
                println!("enabled: {}", enabled);
                println!("selected_model: {:?}", selected);
                if let Some(id) = &selected {
                    if let Some(entry) = crate::embeddings::manifest::find_model(id) {
                        let deriv = crate::embeddings::manifest::derivation_from_manifest(entry);
                        let key = deriv.derivation_key();
                        let st = store.embedding_index_status(&key)?;
                        println!("eligible: {}", st.total_assertions);
                        println!("indexed: {}", st.embedded);
                        println!("missing: {}", st.missing());
                        println!("dimension: {}", entry.dimension);
                    }
                } else {
                    println!("eligible: 0");
                    println!("indexed: 0");
                    println!("missing: 0");
                }
                println!("indexing_available: {}", enabled && selected.is_some());
            }
        },
        #[cfg(unix)]
        Command::Grant { command } => grant::run(cli, command)?,
        // handled at the top of `run`, before anything else happens
        #[cfg(unix)]
        Command::ExtractionWorker => unreachable!("the worker is started by `run`"),
        Command::Mcp {
            allow_roots,
            ingest_timeout,
            insecure_no_sandbox,
            grants,
            broker,
        } => mcp_command(
            cli,
            allow_roots,
            *ingest_timeout,
            *insecure_no_sandbox,
            *grants,
            *broker,
        )?,
    }
    Ok(())
}

/// `mind mcp`: sets up the kernel sandbox (self re-exec, ADR-029) when roots are
/// configured, then serves. Never starts the repository tools without either a
/// verified sandbox or the explicit opt-out.
fn mcp_command(
    cli: &Cli,
    allow_roots: &[String],
    ingest_timeout: std::time::Duration,
    insecure: bool,
    use_grants: bool,
    broker: bool,
) -> Result<()> {
    use crate::mcp::{McpConfig, SandboxState};
    // Stage 2 keeps exactly one inherited descriptor: the validated broker channel (if any).
    #[cfg(target_os = "linux")]
    {
        let keep = crate::sandbox::broker_fd_from_env();
        crate::sandbox::close_inherited_fds_except(keep);
        if let Some(fd) = keep {
            // never into the Git children this process starts, whether or not anything uses it
            // SAFETY: close-on-exec on a descriptor this process owns.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
    }
    // One clear primary error for a database that was never initialized, before
    // any sandbox work that would otherwise report an unrelated reason first.
    let db = db_path(cli)?;
    if !db.exists() {
        return Err(Error::NotInitialized {
            path: db.display().to_string(),
        });
    }
    // Granted directories (ADR-030, M7.3a): read and validated HERE, in stage 1,
    // while the process can still read the store. Stage 2 is already confined; it
    // gets the resolved roots as extra arguments and never reads the store.
    let mut specs: Vec<String> = allow_roots.to_vec();
    let mut grant_args: Vec<std::ffi::OsString> = Vec::new();
    let mut grant_ids: std::collections::BTreeMap<PathBuf, (u64, u64)> = Default::default();
    #[cfg(unix)]
    let store_dir = if use_grants {
        grant_roots_for_start(cli, &db, &mut specs, &mut grant_args, &mut grant_ids)
    } else {
        None
    };
    #[cfg(not(unix))]
    if use_grants {
        return Err(Error::Grant("--grants needs a Unix platform".into()));
    }
    let config = McpConfig::from_specs(&specs)?.with_ingest_timeout(ingest_timeout);
    // Each granted directory must still be the very path that was approved; if
    // canonicalization now leads somewhere else, the directory was swapped while
    // starting: refuse instead of allowing the new target.
    for path in grant_ids.keys() {
        if !config.roots().iter().any(|r| &r.path == path) {
            return Err(Error::Grant(format!(
                "the granted directory {} changed while the server was starting; nothing was allowed. Start again",
                path.display()
            )));
        }
    }
    #[cfg(unix)]
    {
        // A root over the grant store would let the sandboxed server write grants.
        if let Some(store) = &store_dir {
            for r in config.roots() {
                if r.path.starts_with(store) || store.starts_with(&r.path) {
                    return Err(Error::Grant(format!(
                        "the allowed root {} overlaps the grant store {}; with --grants a root there \
                         would let the sandboxed server forge grants. Choose a different root",
                        r.path.display(),
                        store.display()
                    )));
                }
            }
        }
    }
    #[cfg(unix)]
    let config = {
        let dir = db.parent().filter(|p| !p.as_os_str().is_empty());
        let file = dir
            .map(|d| d.join(crate::grants::REQUEST_FILE))
            .unwrap_or_else(|| PathBuf::from(crate::grants::REQUEST_FILE));
        config.with_request_log(file)
    };
    let mut seccomp_report = serde_json::json!({"state": "not_evaluated"});
    let state = if config.roots().is_empty() {
        SandboxState::NotNeeded
    } else if insecure {
        #[cfg(target_os = "linux")]
        {
            eprintln!(
                "mind: WARNING: --insecure-no-sandbox: the repository tools run WITHOUT the kernel \
                 sandbox. Only the application layer protects; use only for trusted input (ADR-029)."
            );
            SandboxState::Disabled
        }
        #[cfg(not(target_os = "linux"))]
        SandboxState::Unavailable("--insecure-no-sandbox is only supported on Linux".to_string())
    } else {
        #[cfg(unix)]
        let broker_req = broker.then(|| crate::sandbox::BrokerRequest {
            grants: store_dir.clone().map(|dir| {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|h| h.is_absolute());
                let policy =
                    crate::grants::PathPolicy::standard(&dir, db.parent(), home.as_deref());
                (dir, policy)
            }),
        });
        #[cfg(not(unix))]
        let broker_req = None;
        let (state, seccomp) =
            sandbox_state(cli, &config, &grant_args, &grant_ids, broker_req.as_ref());
        seccomp_report = seccomp;
        state
    };
    if let SandboxState::Unavailable(reason) = &state {
        eprintln!("mind: the repository tools are OFF: {reason}");
    }
    let config = config.with_sandbox(state).with_seccomp(seccomp_report);
    let store = open(cli)?;
    // Test harness only (feature `spawn-gate`, enabled for `cargo test` builds, absent from a plain build):
    // run the worker from this sandboxed frontend.
    #[cfg(all(unix, feature = "spawn-gate"))]
    if std::env::var_os("MINDSPACE_BROKER_GATE").is_some() {
        return crate::worker::gate::run_broker_gate(&store, &config);
    }
    #[cfg(all(unix, feature = "spawn-gate"))]
    if let Ok(root) = std::env::var("MINDSPACE_SPAWN_GATE") {
        return crate::worker::gate::run(&store, &config, &root);
    }
    crate::mcp::serve_with_config(&store, &config)?;
    Ok(())
}

/// Resolves the grants for `mind mcp --grants` and appends them to `specs`
/// (and, as `--allow-root` pairs, to `grant_args` for the stage-2 re-exec).
/// Returns the grant store directory when it could be determined. Every problem
/// is reported on stderr and means FEWER roots, never more.
#[cfg(unix)]
fn grant_roots_for_start(
    cli: &Cli,
    db: &std::path::Path,
    specs: &mut Vec<String>,
    grant_args: &mut Vec<std::ffi::OsString>,
    grant_ids: &mut std::collections::BTreeMap<PathBuf, (u64, u64)>,
) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    if std::env::var_os(crate::sandbox::ENV_STAGE2).is_some() {
        // Stage 2: the resolved roots arrived as arguments; the store is neither
        // readable (Landlock) nor to be trusted again.
        return None;
    }
    let _ = cli;
    let state = match crate::paths::state_dir() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("mind: grants: {e}; no grant is used");
            return None;
        }
    };
    let store = crate::grants::Store::in_dir(&state.path);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute());
    let policy = crate::grants::PathPolicy::standard(&state.path, db.parent(), home.as_deref());
    let now = time::OffsetDateTime::now_utc();
    let res = crate::grants::resolve_for_start(&store, &policy, now);
    if let Some(why) = &res.store_error {
        eprintln!("mind: grants: {why}");
    }
    for (id, why) in &res.skipped {
        eprintln!(
            "mind: grants: NOT using {id}: {}",
            crate::grants::sanitize_display(why, 300)
        );
    }
    crate::grants::record_skips(&store, &res, now);
    for (path, access) in &res.roots {
        let spec = format!(
            "{}:{}",
            path.display(),
            crate::grants::access_label(*access)
        );
        eprintln!("mind: grants: using {spec}");
        grant_args.push("--allow-root".into());
        grant_args.push(spec.clone().into());
        specs.push(spec);
    }
    grant_ids.extend(res.identities.iter().map(|(k, v)| (k.clone(), *v)));
    Some(std::fs::canonicalize(&state.path).unwrap_or(state.path))
}

#[cfg(target_os = "linux")]
fn sandbox_state(
    cli: &Cli,
    config: &crate::mcp::McpConfig,
    extra_args: &[std::ffi::OsString],
    grant_ids: &std::collections::BTreeMap<PathBuf, (u64, u64)>,
    broker: Option<&crate::sandbox::BrokerRequest>,
) -> (crate::mcp::SandboxState, serde_json::Value) {
    let (state, seccomp, scope) = sandbox_state_inner(cli, config, extra_args, grant_ids, broker);
    // `seccomp` is the socket-deny report; the Landlock signal scope travels next to it.
    let mut report = seccomp.to_json();
    report["signal_scope"] = serde_json::json!(scope.label());
    (state, report)
}

#[cfg(target_os = "linux")]
fn sandbox_state_inner(
    cli: &Cli,
    config: &crate::mcp::McpConfig,
    extra_args: &[std::ffi::OsString],
    grant_ids: &std::collections::BTreeMap<PathBuf, (u64, u64)>,
    broker: Option<&crate::sandbox::BrokerRequest>,
) -> (
    crate::mcp::SandboxState,
    crate::sandbox::seccomp::Status,
    crate::sandbox::SignalScope,
) {
    use crate::mcp::{Access, SandboxState};
    use crate::sandbox::seccomp::Status;
    use crate::sandbox::{self, Outcome, Plan};
    let na = Status::NotEvaluated;
    let no_scope = crate::sandbox::SignalScope::NotEvaluated;
    let db = match db_path(cli) {
        Ok(db) => db,
        Err(e) => return (SandboxState::Unavailable(e.to_string()), na, no_scope),
    };
    let Some(db_dir) = db
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .or_else(|| Some(PathBuf::from(".")))
    else {
        return (
            SandboxState::Unavailable("cannot determine the database directory".into()),
            na,
            no_scope,
        );
    };
    let db_dir = match std::fs::canonicalize(&db_dir) {
        Ok(d) => d,
        Err(e) => {
            return (
                SandboxState::Unavailable(format!("database directory {}: {e}", db_dir.display())),
                na,
                no_scope,
            )
        }
    };
    let exe = match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(e) => e,
        Err(e) => {
            return (
                SandboxState::Unavailable(format!("cannot locate the mind binary: {e}")),
                na,
                no_scope,
            )
        }
    };
    let plan = Plan {
        roots: config
            .roots()
            .iter()
            .map(|r| (r.path.clone(), r.access == Access::ReadWrite))
            .collect(),
        db_dir,
        // Without a usable data directory there is no model directory to allow;
        // the rule for a path that does not exist is skipped (sandbox::rules).
        models_dir: crate::embeddings::manager::models_dir()
            .unwrap_or_else(|_| PathBuf::from("/nonexistent/mindspace-models")),
        exe,
        identities: {
            use std::os::unix::fs::MetadataExt;
            // Static roots: the identity at this moment. Granted roots: the
            // identity that was approved (this overrides, it never trusts the path).
            let mut ids: std::collections::BTreeMap<PathBuf, (u64, u64)> = config
                .roots()
                .iter()
                .filter_map(|r| {
                    std::fs::metadata(&r.path)
                        .ok()
                        .map(|m| (r.path.clone(), (m.dev(), m.ino())))
                })
                .collect();
            ids.extend(grant_ids.iter().map(|(k, v)| (k.clone(), *v)));
            ids
        },
    };
    match sandbox::verify_stage2(&plan.tmp_dir()) {
        Ok(Some(report)) => {
            if let Status::NotApplied(why) = &report.seccomp {
                eprintln!(
                    "mind: sandbox: the seccomp socket filter is NOT applied: {}",
                    why.reason()
                );
            }
            let seccomp = report.seccomp.clone();
            return match crate::ingest::git::sandbox_probe(&plan.tmp_dir()) {
                Ok(()) => (
                    SandboxState::Enforced(report.description),
                    seccomp,
                    report.signal_scope,
                ),
                Err(e) => (
                    SandboxState::Unavailable(format!("Git does not work inside the sandbox: {e}")),
                    seccomp,
                    report.signal_scope,
                ),
            };
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("mind: sandbox: {e}");
            std::process::exit(78);
        }
    }
    match sandbox::enter_with_broker(&plan, extra_args, broker) {
        Outcome::NotNeeded => (SandboxState::NotNeeded, na, no_scope),
        Outcome::Unavailable(reason) => (SandboxState::Unavailable(reason), na, no_scope),
    }
}

#[cfg(not(target_os = "linux"))]
fn sandbox_state(
    _cli: &Cli,
    _config: &crate::mcp::McpConfig,
    _extra_args: &[std::ffi::OsString],
    _grant_ids: &std::collections::BTreeMap<PathBuf, (u64, u64)>,
    _broker: Option<&crate::sandbox::BrokerRequest>,
) -> (crate::mcp::SandboxState, serde_json::Value) {
    (
        crate::mcp::SandboxState::Unavailable(
            "the kernel sandbox exists only on Linux".to_string(),
        ),
        serde_json::json!({"state": "not_evaluated"}),
    )
}

/// Runs a command and returns the process exit code. `mind doctor` has a
/// contract of its own (0 / 1 / 2); every other command is 0 on success and an
/// error otherwise.
pub fn run(cli: &Cli) -> Result<i32> {
    // The worker does nothing before it confines itself: no database, no configuration.
    #[cfg(unix)]
    if let Command::ExtractionWorker = &cli.command {
        return Ok(crate::worker::run::main());
    }
    if let Command::Doctor { json } = &cli.command {
        return Ok(doctor_command(cli, *json).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            2
        }));
    }
    execute(cli)?;
    Ok(0)
}

#[allow(clippy::too_many_arguments)]
fn setup_command(
    cli: &Cli,
    client: Option<crate::setup::Client>,
    ro: &[PathBuf],
    rw: &[PathBuf],
    scope: crate::setup::Scope,
    apply: bool,
    yes: bool,
) -> Result<()> {
    use crate::setup::{Context, Request, Ui};
    use std::io::IsTerminal;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute());
    let ctx = Context {
        db: db_path(cli)?,
        exe: std::env::current_exe()
            .map_err(|e| Error::Setup(format!("cannot locate the running mind binary: {e}")))?,
        path_env: std::env::var_os("PATH"),
        home,
        codex_home: std::env::var_os("CODEX_HOME"),
        interactive: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        selftest: true,
        client_timeout: std::time::Duration::from_secs(60),
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout().lock();
    let mut ui = Ui {
        input: &mut input,
        out: &mut out,
    };
    match client {
        Some(client) => crate::setup::run(
            &Request {
                client,
                scope,
                ro: ro.to_vec(),
                rw: rw.to_vec(),
                apply,
                yes,
            },
            &ctx,
            &mut ui,
        ),
        None => {
            if apply || yes || !ro.is_empty() || !rw.is_empty() {
                return Err(Error::Setup(
                    "name the client to set up: mind setup <claude-code|codex|opencode> ...; nothing was changed"
                        .into(),
                ));
            }
            if ctx.interactive {
                crate::setup::run_wizard(&ctx, &mut ui)
            } else {
                crate::setup::print_detection(&ctx, &mut ui);
                Ok(())
            }
        }
    }
}

fn doctor_command(cli: &Cli, json: bool) -> Result<i32> {
    use std::io::Write;
    let inputs =
        crate::doctor::Inputs::from_process(db_location(cli), crate::paths::data_dir(), true);
    let report = crate::doctor::run(&inputs);
    let text = if json {
        let mut s = serde_json::to_string_pretty(&report.to_json())
            .map_err(|e| Error::Storage(format!("cannot render the report: {e}")))?;
        s.push('\n');
        s
    } else {
        report.render_text()
    };
    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())
        .and_then(|_| out.flush())
        .map_err(|e| Error::Storage(format!("cannot write the report: {e}")))?;
    Ok(report.exit_code())
}

/// Parses the command line and records whether `--db` came from the flag or
/// from `MINDSPACE_DB` (clap merges the two; the source is only known here).
pub fn parse() -> Cli {
    use clap::{CommandFactory, FromArgMatches};
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit());
    cli.db_origin = match matches.value_source("db") {
        Some(clap::parser::ValueSource::CommandLine) => crate::paths::DbOrigin::CommandLine,
        Some(clap::parser::ValueSource::EnvVariable) => crate::paths::DbOrigin::Environment,
        _ => crate::paths::DbOrigin::Unset,
    };
    cli
}

/// The effective database location and where it came from.
fn db_location(cli: &Cli) -> Result<crate::paths::Located> {
    crate::paths::db_path_from(cli.db.as_deref(), cli.db_origin, crate::paths::data_dir)
}

fn db_path(cli: &Cli) -> Result<PathBuf> {
    Ok(db_location(cli)?.path)
}

fn open(cli: &Cli) -> Result<SqliteStore> {
    open_existing(&db_path(cli)?)
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
