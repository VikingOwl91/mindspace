# Contributing

Public `main` is the development line: the source of the qualified Alpha archive plus
later changes. Only the signed archive is the qualified Alpha: to run the Alpha, use the
[official release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.2)
and the [install guide](docs/install.md). A clone or generated source ZIP is not that qualification,
even where the code matches. Report vulnerabilities through the [private channel](SECURITY.md).


Thanks for your interest. Mindspace is an Alpha; please open an issue before starting larger work.

## Build and test

```sh
cargo build --release
cargo test --locked                                   # about 430 tests, no model download needed
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo deny check advisories licenses bans sources     # config: deny.toml (no ignored advisories)
cargo audit
python3 scripts/check-doc-links.py                    # relative links and anchors in the Markdown files
sh tests/scripts/install_sh_test.sh                   # release installer, no build; needs network (--with-build also builds)
shellcheck -s sh install.sh                           # POSIX shell lint of the installer
```

Linux is the only tested platform.

## Rust toolchain

| Purpose | Rust version | Where it is set |
| --- | --- | --- |
| Development and CI | 1.100.0, pinned exactly | `rust-toolchain.toml` |
| Minimum supported (MSRV) | 1.96 | `rust-version` in `Cargo.toml`, checked by the `msrv` CI job |
| Qualified Alpha source archive | 1.100.0 (alpha.1: 1.98.1), frozen | `rust-toolchain.toml` inside the archive |

Changing the toolchain of `main` does not
change a published archive, which is built with the version it pins, as described in the
[install guide](docs/source-install.md). The `msrv` job runs a locked check of all
targets with default features, a check of the library and `mind` binary without
embeddings, and the unit tests, using the minimum compiler. The full test suite
runs on the pinned toolchain.

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
