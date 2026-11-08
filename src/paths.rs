//! Where Mindspace keeps its data, and where each location came from.
//!
//! One implementation for the database and the model directory (they used to
//! have a copy each). The rules:
//!  - `--db` wins over `MINDSPACE_DB`, which wins over the default;
//!  - the default data directory is `$XDG_DATA_HOME/mindspace`, else
//!    `$HOME/.local/share/mindspace`;
//!  - an empty or relative `XDG_DATA_HOME`/`HOME` is not used (the XDG rule for
//!    relative values; an empty value is "unset");
//!  - if nothing usable is left the lookup FAILS. It never falls back to the
//!    current working directory, because data would then end up wherever the
//!    command happened to run.
//!
//! The database path and the model directory stay independent: `--db` moves the
//! database only, models always live in the data directory.
//!
//! Pure functions take their inputs as arguments so the rules are testable
//! without touching the process environment.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::core::{Error, Result};

/// Where a resolved location came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The `--db` option.
    CommandLine,
    /// An environment variable (its name).
    Environment(&'static str),
}

impl Source {
    /// Stable, human-readable label (also used in `mind doctor --json`).
    pub fn label(&self) -> String {
        match self {
            Source::CommandLine => "--db".to_string(),
            Source::Environment(name) => format!("environment variable {name}"),
        }
    }
}

/// A resolved location with its origin and anything that was ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: PathBuf,
    /// What decided the path. For defaults this is the variable the base came
    /// from (`XDG_DATA_HOME` or `HOME`); an explicit database path carries
    /// `CommandLine` or `MINDSPACE_DB`.
    pub source: Source,
    /// True when the path is the built-in default (derived from the data directory).
    pub is_default: bool,
    /// Inputs that were looked at and not used, with the reason.
    pub notes: Vec<String>,
}

/// How the `--db` value reached the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DbOrigin {
    /// No value was given.
    #[default]
    Unset,
    CommandLine,
    Environment,
}

/// A value is usable as a base directory when it is non-empty and absolute.
fn usable(v: &OsString) -> bool {
    !v.is_empty() && Path::new(v).is_absolute()
}

/// The data directory from explicit inputs (`XDG_DATA_HOME`, `HOME`).
pub fn data_dir_from(xdg: Option<OsString>, home: Option<OsString>) -> Result<Located> {
    let mut notes = Vec::new();
    if let Some(x) = &xdg {
        if x.is_empty() {
            // Empty means unset in the XDG convention: no note needed.
        } else if !usable(x) {
            notes.push(format!(
                "XDG_DATA_HOME={} is not an absolute path and was ignored",
                x.to_string_lossy()
            ));
        } else {
            return Ok(Located {
                path: PathBuf::from(x).join("mindspace"),
                source: Source::Environment("XDG_DATA_HOME"),
                is_default: true,
                notes,
            });
        }
    }
    if let Some(h) = &home {
        if h.is_empty() {
            notes.push("HOME is empty and was ignored".to_string());
        } else if !usable(h) {
            notes.push(format!(
                "HOME={} is not an absolute path and was ignored",
                h.to_string_lossy()
            ));
        } else {
            return Ok(Located {
                path: PathBuf::from(h).join(".local/share/mindspace"),
                source: Source::Environment("HOME"),
                is_default: true,
                notes,
            });
        }
    }
    let reason = if notes.is_empty() {
        "neither XDG_DATA_HOME nor HOME is set".to_string()
    } else {
        notes.join("; ")
    };
    Err(Error::DataDirUnresolved { reason })
}

/// The state directory from explicit inputs (`XDG_STATE_HOME`, `HOME`): where the
/// repository grant store lives. Deliberately NOT under the data directory,
/// which the sandboxed MCP server may write.
pub fn state_dir_from(xdg: Option<OsString>, home: Option<OsString>) -> Result<Located> {
    let mut notes = Vec::new();
    if let Some(x) = &xdg {
        if x.is_empty() {
        } else if !usable(x) {
            notes.push(format!(
                "XDG_STATE_HOME={} is not an absolute path and was ignored",
                x.to_string_lossy()
            ));
        } else {
            return Ok(Located {
                path: PathBuf::from(x).join("mindspace"),
                source: Source::Environment("XDG_STATE_HOME"),
                is_default: true,
                notes,
            });
        }
    }
    if let Some(h) = &home {
        if h.is_empty() {
            notes.push("HOME is empty and was ignored".to_string());
        } else if !usable(h) {
            notes.push(format!(
                "HOME={} is not an absolute path and was ignored",
                h.to_string_lossy()
            ));
        } else {
            return Ok(Located {
                path: PathBuf::from(h).join(".local/state/mindspace"),
                source: Source::Environment("HOME"),
                is_default: true,
                notes,
            });
        }
    }
    let reason = if notes.is_empty() {
        "neither XDG_STATE_HOME nor HOME is set".to_string()
    } else {
        notes.join("; ")
    };
    Err(Error::Grant(format!(
        "cannot determine where to keep the grant store: {reason}"
    )))
}

/// The state directory from the process environment.
pub fn state_dir() -> Result<Located> {
    state_dir_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

/// The data directory from the process environment.
pub fn data_dir() -> Result<Located> {
    data_dir_from(std::env::var_os("XDG_DATA_HOME"), std::env::var_os("HOME"))
}

/// The directory that holds installed models (independent of `--db`).
pub fn models_dir() -> Result<PathBuf> {
    Ok(data_dir()?.path.join("models"))
}

/// The database path: an explicit value wins, otherwise `mind.db` in the data
/// directory. `data` is only evaluated when no explicit value exists.
pub fn db_path_from(
    explicit: Option<&Path>,
    origin: DbOrigin,
    data: impl FnOnce() -> Result<Located>,
) -> Result<Located> {
    if let Some(p) = explicit {
        let source = match origin {
            DbOrigin::CommandLine => Source::CommandLine,
            // A value that clap took from the environment, or an origin that
            // was not recorded (library callers): the variable is the best label.
            DbOrigin::Environment | DbOrigin::Unset => Source::Environment("MINDSPACE_DB"),
        };
        return Ok(Located {
            path: p.to_path_buf(),
            source,
            is_default: false,
            notes: Vec::new(),
        });
    }
    let base = data()?;
    Ok(Located {
        path: base.path.join("mind.db"),
        source: base.source,
        is_default: true,
        notes: base.notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(s: &str) -> Option<OsString> {
        Some(OsString::from(s))
    }

    #[test]
    fn xdg_wins_over_home() {
        let l = data_dir_from(os("/x"), os("/h")).unwrap();
        assert_eq!(l.path, PathBuf::from("/x/mindspace"));
        assert_eq!(l.source, Source::Environment("XDG_DATA_HOME"));
    }

    #[test]
    fn home_is_the_fallback_and_keeps_the_linux_default() {
        let l = data_dir_from(None, os("/h")).unwrap();
        assert_eq!(l.path, PathBuf::from("/h/.local/share/mindspace"));
        assert_eq!(l.source, Source::Environment("HOME"));
    }

    #[test]
    fn an_empty_xdg_value_means_unset() {
        let l = data_dir_from(os(""), os("/h")).unwrap();
        assert_eq!(l.path, PathBuf::from("/h/.local/share/mindspace"));
        assert!(l.notes.is_empty());
    }

    #[test]
    fn a_relative_xdg_value_is_ignored_with_a_note() {
        let l = data_dir_from(os("rel"), os("/h")).unwrap();
        assert_eq!(l.path, PathBuf::from("/h/.local/share/mindspace"));
        assert_eq!(l.notes.len(), 1);
        assert!(l.notes[0].contains("XDG_DATA_HOME=rel"));
    }

    #[test]
    fn nothing_usable_is_an_error_not_the_working_directory() {
        for (x, h) in [(None, None), (os(""), os("")), (os("rel"), os("also/rel"))] {
            let e = data_dir_from(x, h).unwrap_err();
            assert!(matches!(e, Error::DataDirUnresolved { .. }), "{e:?}");
            assert!(e.to_string().contains("MINDSPACE_DB"), "{e}");
        }
    }

    #[test]
    fn the_database_path_priority_is_flag_then_environment_then_default() {
        let data = || data_dir_from(os("/x"), None);
        let flag = db_path_from(Some(Path::new("/f.db")), DbOrigin::CommandLine, data).unwrap();
        assert_eq!(
            (flag.path.as_path(), flag.source),
            (Path::new("/f.db"), Source::CommandLine)
        );
        let env = db_path_from(Some(Path::new("/e.db")), DbOrigin::Environment, data).unwrap();
        assert_eq!(env.source, Source::Environment("MINDSPACE_DB"));
        let def = db_path_from(None, DbOrigin::Unset, data).unwrap();
        assert_eq!(def.path, PathBuf::from("/x/mindspace/mind.db"));
        assert!(def.is_default);
    }

    #[test]
    fn an_explicit_database_path_does_not_need_a_data_directory() {
        let none = || data_dir_from(None, None);
        assert!(db_path_from(Some(Path::new("/f.db")), DbOrigin::CommandLine, none).is_ok());
        assert!(db_path_from(None, DbOrigin::Unset, none).is_err());
    }

    #[test]
    fn the_model_directory_does_not_follow_the_database_path() {
        // models_dir() has no database argument at all; this documents the rule.
        let m = data_dir_from(os("/x"), None).unwrap().path.join("models");
        assert_eq!(m, PathBuf::from("/x/mindspace/models"));
    }
}
