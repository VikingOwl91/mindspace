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
authorized local archive verification, stable prefix, runtime dependencies and supported scope.
No public tag/release or anonymous acquisition is asserted.

Build installable ordinary artifacts in their separate target directories. The dev self-dependency
enables a test-only spawn-gate; do not distribute a test or all-targets artifact. Historical full-suite qualification reported 1,327 passed, 0 failed and 35 ignored;
these are historical counts from exact earlier source/artifact inputs, not results
newly obtained from this curated archive. Report each new run's actual results,
ignored tests, optional early-return branches and prerequisites. A Cargo pass count
does not execute ignored model/resource/actor endpoints.

The archive deliberately excludes the historical `scripts/m8-s3e3b-harness.py`
old-reader/actor harness and all `.git` history. Retained ignored tests referring
to that harness are opt-in fixture endpoints and cannot reproduce that historical
qualification from this archive alone. Historical Git refs are unavailable; do
not synthesize substitutes and call them the qualified old client. The retained
`scripts/qualify-alpha-recovery.py` also requires separately preserved exact
ordinary binaries, including the historical old binary; see the
[recovery runbook](docs/offline-recovery.md#historical-qualification-and-limits).
Real-model tests require separately obtained assets and resources; no model
weights are included. See [third-party notices](THIRD_PARTY_NOTICES.md).

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

## Reconstruct the source candidate

From a clean checkout of the intended signed source commit, run:

```sh
python3 scripts/assemble-source-archive.py --repo . --output /tmp/mindspace-source-rc
```

The output directory must not exist. The assembler reads only exact Git blobs
listed in `release/source-files.txt`; untracked files and local configuration are
never payload inputs. It includes that list and the assembler in the payload.
No historical archive, private report or temporary overlay directory is needed.
`--ref COMMIT` selects a different commit only when its assembler equals the
running version. Review the allowlist whenever build/test inputs are added.
Normalized USTAR and gzip metadata make repeated construction deterministic for
the qualified Python/zlib versions. Detached GPG signatures are issued separately
and are not claimed byte-reproducible. This command does not publish anything.
