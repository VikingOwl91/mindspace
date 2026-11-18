# Mindspace

Mindspace gives coding agents a local memory across sessions. Save a decision,
index a Git checkout, then search for a note or code definition and inspect
its source. The `mind` CLI and stdio Model Context Protocol (MCP) server share
a SQLite database. Lexical search runs locally without a model or an account.
The [document preview](#preview-a-document) also answers cited questions about
papers and manuals from a checkout.

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
search, linked-worktree ingest, and ingest deadlines. This command builds the
model-free CLI with the project's pinned Rust toolchain and locked dependencies.

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

## Preview a document

From a Linux checkout, query a trusted PDF or DOCX and get JSON with citations
to the exact file revision. The preview runs offline with Python 3. PDFs also
need Poppler's `pdfinfo`, `pdftotext`, and `pdfimages`. You can run it before
building Mindspace or creating a database.

```sh
python3 -B scripts/document_preview/cli.py docs/experiments/m8/document-corpus/procedure.docx section "Hill station > Equipment"
```

The example handbook has two Equipment sections. Selecting the hill station
returns “Pack the wind gauge.” with its heading and paragraph citations.
For scientific papers, the `paper` profile cites enrollment and analysis
counts separately for each experiment.
[Preview commands, examples and limits](https://github.com/VikingOwl91/mindspace/blob/main/docs/document-preview.md)

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

[CLI and MCP reference](docs/reference.md) · [Development roadmap](docs/roadmap.md) ·
[Architecture and security boundaries](docs/architecture.md#security-boundaries) ·
[Backup and recovery](docs/offline-recovery.md) · [All guides](docs/index.md)

Report vulnerabilities through the project's private reporting channel. [Security policy](SECURITY.md)

## License

Mindspace uses the [MIT License](LICENSE). Third-party assets have their own
licenses. [Third-party notices](THIRD_PARTY_NOTICES.md) · [License texts](LICENSES/)
