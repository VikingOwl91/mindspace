# M8.0 parser probes (experiment, not production code)

`parser_probe.rs` is the `main.rs` of a throw-away crate (dependencies: `lopdf 0.47`, `pdf_oxide 0.3`, `zip 9` without
default features but with `deflate`, `roxmltree 0.21`, `scraper 0.29`), built `--release` outside this repository. It is not
part of the Mindspace build and nothing under `src/` depends on it. Fixtures were generated, not committed:

| Fixture | How |
|---|---|
| `paper.pdf` | `typst compile paper.typ` (document metadata title deliberately says "Invoice"; body is a paper with a table, German, Japanese and Russian text) |
| `enc.pdf` | `qpdf --encrypt user owner 256 -- paper.pdf enc.pdf` |
| `trunc.pdf` | first 1500 bytes of `paper.pdf` |
| `bomb.pdf` | one FlateDecode stream: `zlib.compress(b"\0"*200_000_000, 9)` (195 KB on disk) |
| `memo.docx` | `soffice --headless --infilter="HTML (StarWriter)" --convert-to docx:"MS Word 2007 XML" memo.html` |
| `bomb.docx` | valid OOXML package whose `word/document.xml` expands to 1.02 GB (3 MB on disk) |

Results (this machine, release build, single run each; sizes tiny, so times are indicative only): see
`docs/design-m8-document-intelligence.md` section 4.3. Licence probe: `cargo deny check licenses` with this repository's
`deny.toml` over the probe crate.

## M8.0a additions

New probe sub-commands in `parser_probe.rs`: `scancheck` (image-only page heuristic), `enccheck` (`pdf_oxide::is_encrypted`), `zipmeta` (entry names, `enclosed_name`, `by_name` after duplicate entries), `xmlopts` (`roxmltree` default vs `allow_dtd` + `nodes_limit`). Extra fixtures (generated, not committed): `scan.pdf` (`gs -sDEVICE=pdfimage24 -r100`), `laughs.docx` (nested internal entities), `xxe.docx` (`SYSTEM "file:///etc/hostname"`), `deep.docx` (5,000 nested `w:p`), `deep2.docx` (300,000), `traversal.docx` (`../../evil.txt`, `/abs/evil.txt`), `dupname.docx` (two `word/document.xml`), `manyentries.docx` (100,000 entries). Memory-limit runs: `systemd-run --user --wait --pipe -p MemoryMax=128M -p MemorySwapMax=0 <probe> lopdf bomb.pdf` (and `oxide`), and `ulimit -v 262144` for `RLIMIT_AS`. The embedding-tokenizer measurement used a separate throw-away crate (`tokenizers 0.23`, the repository's `nomic-embed-text-v1.5` `tokenizer.json`) over seven text samples: English prose 3.96 bytes/token, English with tables 3.54, Rust code 3.35, German 2.98, Japanese 3.00, Russian 2.19, pipe table 2.02. Results are tabulated in `docs/design-m8-document-intelligence.md` sections 4.3 and 8.
