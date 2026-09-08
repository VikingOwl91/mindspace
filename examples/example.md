# Owlry project notes

Owlry is the internal name for the memory tooling project. It is built in
Rust and runs entirely on the local machine.

## Storage

Owlry stores everything in a single SQLite database file, including the
full-text index. There is no server process involved.

## Portability

The command line tool is built for Wayland desktops and ships as a single
static binary.
