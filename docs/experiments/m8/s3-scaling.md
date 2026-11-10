# S3 isolated scaling measurements

The standalone Rust probe links this checkout's locked release library and its bundled
SQLite. It has no additional dependencies and creates no persistent database. Run from
the checkout with an existing writable `CARGO_TARGET_DIR`, or let Cargo use `target`:

```sh
RUSTC_WRAPPER= cargo build --locked --release --lib --message-format=json > /tmp/s3-build.jsonl
python3 - <<'PY'
import json, pathlib, subprocess
rows = [json.loads(s) for s in pathlib.Path('/tmp/s3-build.jsonl').read_text().splitlines()]
def artifact(name):
    row = next(r for r in rows if r.get('reason') == 'compiler-artifact' and r['target']['name'] == name)
    return next(p for p in row['filenames'] if p.endswith('.rlib'))
lib, sqlite = artifact('mindspace'), artifact('rusqlite')
subprocess.run(['rustc', '--edition=2021', '-O', 'docs/experiments/m8/s3_scaling.rs',
                '--extern', f'mindspace={lib}', '--extern', f'rusqlite={sqlite}',
                '-L', f'dependency={pathlib.Path(sqlite).parent}', '-o', '/tmp/s3-scaling'], check=True)
PY
MINDSPACE_S3_ROOT="$PWD" /tmp/s3-scaling markdown 4194304
```

For `prose`, `cjk`, `unbroken`, and `markdown`, use 262144, 1048576 and
4194304 bytes, three fresh processes per combination. Report medians and peak RSS
ranges, not just the fastest observation. No whole-document tokenizer measurement is
mixed into these runs. Extraction and chunk generation are timed separately. The
probe then verifies contiguous full-byte coverage, real final token counts and exact
containment of context paths, outside the timed chunk operation. RSS is the Linux
process high-water mark read immediately before/after chunk generation; the latter
includes tokenizer loading, extraction and output metadata, not validation afterwards.

For `sql-old` and `sql-fixed`, use 1, 2, 4, 8 and 16 MiB, three fresh processes each.
This is an isolated insertion microbenchmark with minimal prerequisite tables and the
actual migration 0012 SQL. `sql-old` restores only the previous CAST expression; no
application source or database is mutated. A fixed 8 KiB range per row isolates trigger
cost. Synthetic token/anchor fields do not simulate embedding or assertion projection.
Report the bundled SQLite version printed by the probe. These times are not complete
application ingestion timings; S1 node validation/triggers are deliberately outside it.
