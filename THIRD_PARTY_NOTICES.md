# Third-party notices

## Document preview corpus

The preview tests reference PLOS ONE articles by Makovski et al. (2008) and
Fitzhugh et al. (2019). The corpus records their authors, publisher URLs,
Creative Commons Attribution licenses, and the revisions used for testing.
[Corpus sources and permissions](https://github.com/VikingOwl91/mindspace/blob/main/docs/experiments/m8/document-corpus/README.md#sources-and-permissions)

## Third-party tokenizer fixture

Mindspace's MIT license applies to Mindspace-authored source code. The separately sourced fixture `tests/fixtures/nomic-tokenizer.json` is provided under Apache License, Version 2.0; see `LICENSES/Apache-2.0.txt`.

The fixture is an unmodified copy of `tokenizer.json` published by Nomic AI with `nomic-ai/nomic-embed-text-v1.5` at revision `e9b6763023c676ca8431644204f50c2b100d9aab`:

- Source: https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/resolve/e9b6763023c676ca8431644204f50c2b100d9aab/tokenizer.json
- License declaration: https://huggingface.co/nomic-ai/nomic-embed-text-v1.5/blob/e9b6763023c676ca8431644204f50c2b100d9aab/README.md (`license: apache-2.0`).
- Size: 711396 bytes. SHA-256: `d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66`.
- Its complete token-to-ID vocabulary equals the Nomic revision's `vocab.txt` and the Google BERT project's `google-bert/bert-base-uncased` `vocab.txt` at revision `86b5e0934494bd15c9632b12f734a8a67f723594` (vocabulary SHA-256: `07eced375cec144d27c900241f3e339478dec958f92fddbc551f295c992038a3`). This comparison establishes identical vocabulary contents, not the history or authorship of every tokenizer configuration field.
- BERT vocabulary source: https://huggingface.co/google-bert/bert-base-uncased/blob/86b5e0934494bd15c9632b12f734a8a67f723594/vocab.txt
- BERT license: https://huggingface.co/google-bert/bert-base-uncased/blob/86b5e0934494bd15c9632b12f734a8a67f723594/LICENSE

No upstream NOTICE file appears in the complete Nomic file list at the specified revision, or in the complete Google BERT Hugging Face file list at the specified revision. No copyright or attribution metadata fields were found in the fixture. This document supplies provenance and attribution; it is not a purported upstream NOTICE file and does not assert an unverified copyright owner or date.

The fixture is unchanged. Future modifications to the fixture must be prominently identified, and applicable upstream notices retained, as required by Apache License 2.0 section 4.

The archive includes references to external Rust crates in `Cargo.toml` and `Cargo.lock`; their source code, build outputs, and model weights are not included. These crates retain their own licenses. A binary or vendored-source distribution needs a separate inventory of the code and notices it actually contains.

## Additional source grammars

The following Tree-sitter grammar crates are linked by the symbol indexer. Their pinned versions are in `Cargo.lock`. The upstream MIT licenses are included here.

| Grammar | Upstream source | License |
| --- | --- | --- |
| Go 0.25.0 | [tree-sitter-go](https://github.com/tree-sitter/tree-sitter-go/tree/v0.25.0) | [MIT](LICENSES/tree-sitter-go.txt) |
| JavaScript / JSX 0.25.0 | [tree-sitter-javascript](https://github.com/tree-sitter/tree-sitter-javascript/tree/v0.25.0) | [MIT](LICENSES/tree-sitter-javascript.txt) |
| TypeScript / TSX 0.23.2 | [tree-sitter-typescript](https://github.com/tree-sitter/tree-sitter-typescript/tree/f975a621f4e7f532fe322e13c4f79495e0a7b2e7) | [MIT](LICENSES/tree-sitter-typescript.txt) |
| Svelte 1.0.2 | [tree-sitter-svelte](https://github.com/tree-sitter-grammars/tree-sitter-svelte/tree/774a65aea563accc35f5d45fafa4d96ec5761f57) | [MIT](LICENSES/tree-sitter-svelte.txt) |
