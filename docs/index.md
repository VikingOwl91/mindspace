# Alpha documentation

Start with the [README](../README.md), then [verify and install the RC.5 source
archive](source-install.md) from the
[official Alpha release](https://github.com/VikingOwl91/mindspace/releases/tag/v0.1.0-alpha.1).
Public main contains an older implementation plus these documentation updates.
The release tag points to the older hosting commit
`92f68ff646ec9b06ba21df2fb7879dfbc1281838`; the tag and generated source downloads
do not identify or contain the qualified RC.5 implementation. The former
`v0.2.0` and `v0.3.0` pre-alpha releases and their tags have been removed;
`v0.1.0-alpha.1` is the only published release.

- [Installation, verification and uninstall](source-install.md)
- [CLI, configuration and manual MCP examples](reference.md)
- [Client setup and undo](setup.md)
- [Release notes and excluded capabilities](release-notes.md)
- [Architecture and security boundaries](architecture.md)
- [Offline backup and pre-upgrade recovery](offline-recovery.md)
- [Security reports and supported versions](../SECURITY.md)
- [MIT license](../LICENSE) and [archive third-party notices](../THIRD_PARTY_NOTICES.md)

The README has a short quickstart against your own repository. The install
guide (sections 6 to 9) includes the complete temporary Git-repository demo and a
standalone Python/stdio MCP memory example, so an AI account is not required. Execute examples against the verified archive build, not a build
of older public main.

The supported profile is Linux x86_64 without embedding models: explicit memories,
legacy Markdown/text, committed Git content, lexical retrieval and Rust symbols.
Canonical document/PDF ingestion and unfinished M8 publication are unsupported.
Other existing design documents, experiments, reviews and benchmark records in
this repository are historical development material. They are not required for
installation and do not extend supported Alpha capabilities.
