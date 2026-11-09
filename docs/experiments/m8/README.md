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
