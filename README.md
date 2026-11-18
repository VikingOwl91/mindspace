# Mindspace

Mindspace gives coding agents a local memory they can use across sessions. Save
a decision, index a Git checkout, then search for a note or a code definition
and inspect where it came from. The `mind` CLI and the stdio Model Context
Protocol (MCP) server use the same SQLite database. Lexical search works
without a model, an account, or a daemon.

## Install

The published **v0.1.0-alpha.2** is a signed source release for Linux x86_64.
It includes explicit memories, Markdown and text ingestion, lexical search,
and Rust symbol lookup. The installer builds the release locally. Confirm the
publisher fingerprint through an independent trusted channel before running it.
[Install guide and prerequisites](docs/install.md) · [Release and signatures](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.2)

```sh
curl -fsSL https://raw.githubusercontent.com/VikingOwl91/mindspace/main/install.sh | sh -s -- --trust-fingerprint 0E6699CB48F3941AE28CA88D509F78007F62D5EA
```

Use the binary path the installer prints, or add its directory to your shell's
`PATH`. [Bash, Zsh and Fish examples](docs/install.md#after-installing)

### Build from a checkout

Build `main` for Go, JavaScript/JSX, TypeScript/TSX and inline Svelte symbol
search, linked-worktree ingest improvements, and ingest deadlines. This
model-free build uses the locked dependencies. Development pins Rust 1.100.0
and CI also checks the minimum supported toolchain, Rust 1.96.0.

```sh
git clone https://github.com/VikingOwl91/mindspace
cd mindspace
cargo build --locked --release --no-default-features --bin mind
target/release/mind --version
```

Use `target/release/mind` explicitly when testing a checkout. The checkout and
the signed release can report the same package version. [Development roadmap](docs/roadmap.md)

## Quickstart

Try this in an **empty working directory** with Git and `mind` on your
`PATH`. The commands create a small repository and a separate demo database.

```sh
git init -q demo-repo
printf '# Retry policy\n\nRetry a failed upload once.\n' > demo-repo/notes.md
git -C demo-repo add notes.md
git -C demo-repo -c user.name=Demo -c user.email=demo@example.invalid commit -qm "Record retry policy"

mind --db ./demo-data/mind.db init
mind --db ./demo-data/mind.db space create demo
mind --db ./demo-data/mind.db ingest ./demo-repo --space demo
mind --db ./demo-data/mind.db search retry --space demo
```

The hit points back to `notes.md` at the commit you just made. Pass its memory
ID to `mind --db ./demo-data/mind.db inspect <memory-id>` for the evidence.
Edit the note, commit it, and ingest again to see how a decision changes.
Git ingestion reads committed content and may create a repository identity
marker in Git metadata. Use the CLI with repositories you trust.

For your own database, run `mind init` without `--db`. The CLI and agent use
`--db` first, then `MINDSPACE_DB`, then the default under
`$XDG_DATA_HOME/mindspace/` (normally `~/.local/share/mindspace/mind.db`).
Keep the database outside any repository you share with an agent.

## Connect an agent

Preview registration against your own database, then apply it.

```sh
mind setup codex
mind setup codex --apply
```

Use `claude-code` for Claude Code. OpenCode supports preview only. The default
registration exposes memory tools and grants no repository access. To share a
repository, include `--ro /absolute/path` for outlines or `--rw /absolute/path`
for ingestion and review the preview before applying. Repository tools require
working Linux confinement. [Setup, permissions and undo](docs/setup.md) · [Manual MCP configuration](docs/reference.md#manual-mcp-configuration)

## Documentation

- [CLI and MCP reference](docs/reference.md) for search, symbol coverage, ingest limits and failure handling.
- [Development roadmap](docs/roadmap.md) for what has landed on `main` and what remains open.
- [Architecture and security boundaries](docs/architecture.md#security-boundaries) for local stdio access, confinement and storage.
- [Offline backup and recovery](docs/offline-recovery.md) before upgrading an existing database.
- [Documentation index](docs/index.md) for operator guides, release notes and development checks.

Report vulnerabilities through the project's private reporting channel. [Security policy](SECURITY.md)

## License

Mindspace uses the [MIT License](LICENSE). Third-party assets have their own
licenses. [Third-party notices](THIRD_PARTY_NOTICES.md) · [License texts](LICENSES/)
