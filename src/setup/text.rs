//! The explanations `mind setup` shows, kept in one place so the wizard, the
//! preview, `--help` and the documentation say the same thing.

/// Printed first by the interactive setup.
pub const INTRO: &str = "\
What this does
  MCP (Model Context Protocol) is the standard way a coding agent talks to local tools.
  This setup registers Mindspace as an MCP server with your agent, so the agent can store and
  search memories and look up code definitions through Mindspace.
  Mindspace keeps everything in one local database file on this machine.
  Nothing is sent to a network service by this setup.";

macro_rules! permissions {
    () => {
        "\
Repository access (these are MINDSPACE permissions)
  read-only (--ro)   Mindspace may read repository files inside the folder you name, and it
                     cannot change them. Recommended.
  read-write (--rw)  Mindspace may read AND change repository files inside the folder you name,
                     as part of the functions it supports (for example writing its repository
                     identity marker into a repository's .git directory when ingesting).
  By default NO folder is shared: without --ro/--rw the agent only gets the memory tools.
  Only folders you name are covered; the permissions do not extend to the rest of the file system."
    };
}

macro_rules! limits {
    () => {
        "\
Good to know
  - Read access is not a confidentiality guarantee towards the connected agent: whatever Mindspace
    can read in a shared folder, the agent can ask Mindspace about.
  - --ro/--rw describe what Mindspace may do. They do not limit what your agent or its other tools
    (a shell, an editor plugin, other MCP servers) may do.
  - The repository tools run inside a kernel sandbox (Linux Landlock). Where no effective sandbox
    exists they stay switched off (fail-closed); the memory tools keep working."
    };
}

/// The default, and what the guided session says first about folders.
pub const ZERO_PERMISSION: &str = "\
Zero-permission setup (the default)
  Setup without any folder is complete: the agent gets all the memory tools (store and search notes,
  decisions and facts) and Mindspace can read NO repository files. Sharing repository folders is
  optional; you can start without and add folders later (remove the registration, run setup again
  with --ro).";

/// The two permission modes, as shown in the wizard, the preview and the docs.
pub const PERMISSIONS: &str = permissions!();

/// Limits worth knowing before saying yes.
pub const LIMITS: &str = limits!();

/// The text under `mind setup --help`.
pub const HELP_AFTER: &str = concat!(
    "Zero-permission setup is the default: without --ro/--rw the registration is complete, the agent gets all the\nmemory tools and Mindspace can read no repository files. Sharing folders is optional.\n\n",
    permissions!(),
    "\n\n",
    limits!(),
    "\n\nExamples (the folders are only examples; they are never created):\n  mind setup                                      guided, in a terminal\n  mind setup claude-code                          preview, changes nothing\n  mind setup claude-code --apply                  register with no folder shared, after you confirm\n  mind setup claude-code --ro ~/projects --apply  also let Mindspace read ~/projects\n  mind setup codex --apply --yes                  without a terminal\n\nSupported clients: claude-code, codex (both can be applied), opencode (preview only).\nSee docs/setup.md for how to undo a setup."
);

/// Shown where a path example appears.
pub const EXAMPLE_NOTE: &str =
    "~/projects and ~/reference are only examples: they are not required and are never created.";
