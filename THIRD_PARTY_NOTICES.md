# RC.5 archive third-party notices

These notices are retained from the qualified RC.5 archive. The tokenizer fixture
is in that archive; this documentation-only update does not add it to public main.
The original notice follows.

# Third-party tokenizer fixture

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
