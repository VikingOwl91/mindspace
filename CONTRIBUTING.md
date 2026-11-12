# Contributing

Thanks for your interest. Mindspace is an unreleased `0.1.0-alpha.1` candidate; please open an issue before starting larger work.

## Build and test

```sh
cargo build --locked --release --no-default-features --bin mind --target-dir target/alpha-noembed
cargo test --locked                                   # ignored/optional coverage must be reported
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo deny check advisories licenses bans sources     # config: deny.toml (no ignored advisories)
cargo audit
python3 scripts/check-doc-links.py                    # relative links and anchors in the Markdown files
```

The toolchain is pinned to Rust1.98.1 in `rust-toolchain.toml`. Linux is the qualified platform.
The noembed profile requires a C compiler/development headers; the alternative default build
(`cargo build --locked --release --bin mind --target-dir target/alpha-default`) also requires a
C++ compiler. SQLite is bundled. See [source installation](docs/source-install.md) for the
source-access blocker, stable prefix, runtime dependencies and supported scope. No public
tag/release or anonymous acquisition is asserted.

Build installable ordinary artifacts in their separate target directories. The dev self-dependency
enables a test-only spawn-gate; do not distribute a test or all-targets artifact. Current historical
qualification totals are recorded in `docs/releases/`; a Cargo pass count does not execute ignored
model/resource/actor endpoints or optional early-return branches.

Changes that touch the MCP repository tools, the sandbox or path handling must also pass
`scripts/security-profile.sh` (needs Linux 6.2+ with Landlock, about 5 minutes; run
`scripts/check-sandbox-enforced.py target/debug/mind` first to see whether the sandbox is really enforced on your machine). Changes to MCP behaviour
should keep `scripts/mcp-e2e-claude.py` green if you have the `claude` CLI (it uses your own account).

## What a good change looks like

- **Evidence over assertion.** A bug fix starts with a test that fails without the fix. A security fix
  states the invariant it protects and has a test that fails on the old code.
- **Small and scoped.** One concern per pull request, no drive-by reformatting of unrelated files.
- **Deterministic first.** Parsing, Git facts and lexical search must not depend on an AI model.
- **Decisions are recorded.** Architecture-relevant choices get an entry in `docs/decisions.md`
  (an ADR: context, decision, consequences).
- **Honest docs.** Do not describe planned features as implemented. Measurements need the command, the
  machine and the date.

## Security issues

Do not report vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).

## License

By contributing you agree that your contribution is licensed under the MIT License (see `LICENSE`).
