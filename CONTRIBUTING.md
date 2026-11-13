# Contributing

Public main has older implementation code than the qualified RC.5 Alpha archive.
For the first independently qualified public Alpha baseline, use the
[official release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1)
and the [install guide](docs/source-install.md). A clone or generated source ZIP is older code; its
builds/tests are not RC.5 qualification. Contributor instructions below describe
that public implementation. Report vulnerabilities through the [private channel](SECURITY.md).


Thanks for your interest. The implementation on public main is pre-alpha; please open an issue before starting larger work.

## Build and test

```sh
cargo build --release
cargo test --locked                                   # about 430 tests, no model download needed
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo deny check advisories licenses bans sources     # config: deny.toml (no ignored advisories)
cargo audit
python3 scripts/check-doc-links.py                    # relative links and anchors in the Markdown files
```

The toolchain is pinned in `rust-toolchain.toml`. Linux is the only tested platform.

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
