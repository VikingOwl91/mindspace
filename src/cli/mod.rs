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
    Info { id: String },
    /// Download and install a trusted model
    Install {
        id: String,
        #[arg(long)]
        yes: bool,
    },
    /// Select an installed model
    Use { id: String },
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
        Command::Init => {
            let path = db_path(cli);
            initialize(&path)?;
            println!("initialized mindspace at {}", path.display());
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
                    let p = crate::embeddings::manager::model_dir(m.id).join(a.filename);
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
                let st = crate::embeddings::manager::status(&store);
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
                    let dir = crate::embeddings::manager::model_dir(&selected);
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
        Command::Mcp {
            allow_roots,
            ingest_timeout,
        } => {
            let config = crate::mcp::McpConfig::from_specs(allow_roots)?
                .with_ingest_timeout(*ingest_timeout);
            let store = open(cli)?;
            crate::mcp::serve_with_config(&store, &config)?;
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
