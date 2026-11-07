# MIND-28: manual interactive Claude Code test (about 15 minutes)

Purpose: confirm in a real, interactive Claude Code session what the automated test
(`scripts/mcp-e2e-claude.py`, headless) could not: `claude mcp add`, the `/mcp` screen, permission
prompts, and the real sandbox status. Everything happens in a scratch directory; no real project is
touched. Syntax below was checked against `claude mcp add --help` of Claude Code 2.1.319.

Dry run by the author (2026-11-07, not interactive): sections 0, 1, 4 and the tool calls of steps b to e were executed
against the server registered with `claude mcp add --scope project` in a scratch directory and gave the expected
results. Not exercised there, and the reason for this manual test: the interactive UI, `/mcp`, permission prompts,
the default `local` scope and the restart in step f.

## 0. Preconditions

```sh
cd /path/to/mindspace && cargo build --release
MIND=$PWD/target/release/mind
grep -qw landlock /sys/kernel/security/lsm && echo "landlock active"      # must print this
python3 scripts/check-sandbox-enforced.py "$MIND"                           # must print OK: sandbox 'enforced ...'
```

If either fails, stop: the repository tools will be off and that is the correct behaviour.

## 1. Scratch world

```sh
T=$HOME/mindspace-e2e && mkdir -p $T/{data,repos,ref,outside}
git clone -q /path/to/mindspace $T/repos/mindspace          # read-write root content
git init -q -b main $T/ref/notes && printf 'pub fn READONLY_PROBE() {}\n' > $T/ref/notes/lib.rs \
  && git -C $T/ref/notes add -A && git -C $T/ref/notes -c user.name=t -c user.email=t@e.invalid commit -qm init
printf 'OUTSIDE_SECRET_TOKEN_1\n' > $T/outside/secret.txt    # bait outside every root
MINDSPACE_DB=$T/data/mind.db $MIND init
MINDSPACE_DB=$T/data/mind.db $MIND space create demo
( cd $T && find ref outside -type f -not -path '*/.git/*' | sort | xargs sha256sum; find ref -path '*/.git/*' | sort ) > $T/before.txt
```

## 2. Register the server (the step the headless test did not cover)

```sh
cd $T
claude mcp add mindspace -- "$MIND" --db "$T/data/mind.db" mcp \
  --allow-root "$T/repos:rw" --allow-root "$T/ref:ro"
claude mcp list            # expect: mindspace ... connected
```

## 3. Interactive session

Start `claude` inside `$T` and check:

| Step | Do | Expect |
| --- | --- | --- |
| a | `/mcp` | `mindspace` is listed as connected; its 14 tools can be inspected |
| b | Ask: "Use the mindspace tools: repo_outline on `$T/repos/mindspace`, then repo_ingest it into space demo, then code_search for `ingest_confined`. Show me the raw `sandbox` field of the repo_ingest result." | You are asked to approve the tool calls (note which prompts appear). The `sandbox` field reads `enforced (Landlock, ABI 3 or newer, fully enforced, canary verified)`. `code_search` returns `src/ingest/confined.rs` |
| c | Ask: "repo_ingest `$T/ref/notes` into demo" | Refused: the path is inside a read-only root |
| d | Ask: "repo_outline `$T/outside`" | Refused: outside the allowed roots |
| e | Ask: "repo_outline `$T/ref/notes`" | Allowed (read-only roots may be outlined) |
| f | Exit `claude`, start it again in `$T`, ask: "code_search `ingest_confined` in space demo (no ingest)" | Same hit: data persisted across the restart |

## 4. Independent checks afterwards (do not trust the chat)

```sh
cd $T
( find ref outside -type f -not -path '*/.git/*' | sort | xargs sha256sum; find ref -path '*/.git/*' | sort ) | diff before.txt - && echo "ref/outside unchanged"
grep -c -a OUTSIDE_SECRET_TOKEN_1 data/mind.db* ; grep -c -a READONLY_PROBE data/mind.db*   # both must be 0 for every file
ls repos/mindspace/.git | grep mindspace-repository-id                                      # marker exists (rw root only)
```

## 5. Clean up

```sh
claude mcp remove mindspace
rm -rf $HOME/mindspace-e2e
```

## Pass criteria and what to report

PASS if: `/mcp` shows connected; the `sandbox` field says `enforced ...`; steps c and d are refused with
the expected reasons; e is allowed; f finds the earlier data; section 4 prints "unchanged" and zeros.

Please report: Claude Code version (`claude --version`), kernel (`uname -r`) and distribution, which
permission prompts appeared, the exact `sandbox` text, and any step that behaved differently. If the
`sandbox` field says anything other than `enforced`, or a refused step succeeded, that is a release
blocker: stop and send the output.

Not covered by this test: other MCP clients, large repositories (a first ingest of a big repository
can take minutes, see the README), other kernels.
