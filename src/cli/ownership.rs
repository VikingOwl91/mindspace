//! Direct trusted-local administration. No MCP route constructs this context.
//!
//! The OS account and this process are trusted (Model A). A same-UID program can
//! deliberately run this command; neither confirmation nor this context authenticates a human.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde::Serialize;
use uuid::Uuid;

use crate::core::{Error, Result};
use crate::paths::DbOrigin;
use crate::SqliteStore;

#[derive(Subcommand)]
pub enum OwnershipCommand {
    /// Enroll an existing file logical source as legacy-owned (no search changes).
    /// The local account/process is trusted; this does not authenticate a distinct human.
    EnrollLegacy {
        /// Exact full logical source UUID, not a pathname or UUID prefix.
        #[arg(long, value_parser = full_uuid)]
        source_id: Uuid,
        /// Expected current recording revision (before this operation).
        #[arg(long, value_parser = positive_revision)]
        expected_revision: i64,
        /// Preview nonce; preview writes no ownership and prints its full operation ID.
        #[arg(long, value_parser = full_uuid, conflicts_with_all = ["operation_id", "apply", "confirm_intent"])]
        nonce: Option<Uuid>,
        /// Full local-enroll@1:nonce:digest identity from the preview.
        #[arg(long, requires = "apply")]
        operation_id: Option<String>,
        /// Explicitly execute the exact intent; requires --confirm-intent and --operation-id.
        #[arg(long, requires_all = ["operation_id", "confirm_intent"])]
        apply: bool,
        /// Exact intent SHA-256 from the preview, not an independently reusable approval.
        #[arg(long, requires_all = ["apply", "operation_id"])]
        confirm_intent: Option<String>,
        /// Print machine-readable JSON instead of escaped human-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Associate a legacy-enrolled asset with one exact stored document extraction.
    /// This preparation changes no retrieval owner, index or active memory.
    EnrollDocumentPreparation {
        #[arg(long, value_parser = full_uuid)]
        asset_id: Uuid,
        #[arg(long, value_parser = full_uuid)]
        legacy_source_version: Uuid,
        #[arg(long, value_parser = full_uuid)]
        document_id: Uuid,
        #[arg(long, value_parser = full_uuid)]
        document_source_version: Uuid,
        #[arg(long, value_parser = full_uuid)]
        extraction_id: Uuid,
        #[arg(long, value_parser = positive_revision)]
        expected_revision: i64,
        #[arg(long, value_parser = full_uuid, conflicts_with_all = ["operation_id", "apply", "confirm_intent"])]
        nonce: Option<Uuid>,
        #[arg(long, requires = "apply")]
        operation_id: Option<String>,
        #[arg(long, requires_all = ["operation_id", "confirm_intent"])]
        apply: bool,
        #[arg(long, requires_all = ["apply", "operation_id"])]
        confirm_intent: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

fn full_uuid(raw: &str) -> std::result::Result<Uuid, String> {
    let id = Uuid::parse_str(raw).map_err(|_| "expected a full canonical UUID".to_string())?;
    if raw != id.to_string() {
        return Err("expected a full lowercase hyphenated UUID (no prefix or aliases)".into());
    }
    Ok(id)
}

fn positive_revision(raw: &str) -> std::result::Result<i64, String> {
    match raw.parse::<i64>() {
        Ok(revision) if revision > 0 => Ok(revision),
        _ => Err("expected a positive recording revision".into()),
    }
}

fn refused(message: impl Into<String>) -> Error {
    Error::Storage(format!("local administration refused: {}", message.into()))
}

/// Nonclone and nonserializable: only this direct command handler constructs it.
/// The retained descriptor prevents inode reuse from disguising target replacement.
pub(crate) struct AdminContext {
    path: PathBuf,
    uid: u32,
    dev: u64,
    ino: u64,
    file: File,
}

impl AdminContext {
    fn establish(cli: &super::Cli) -> Result<Self> {
        if cli.db_origin != DbOrigin::CommandLine {
            return Err(refused("ownership requires an explicit absolute --db command-line flag; environment/default selection is not accepted"));
        }
        let path = cli
            .db
            .as_deref()
            .ok_or_else(|| refused("missing explicit --db"))?;
        if !path.is_absolute() || path.to_str().is_none() {
            return Err(refused("--db must be an absolute UTF-8 filesystem path"));
        }
        let text = path.to_string_lossy();
        if text.contains('?') || text.contains('#') || text.contains('\0') {
            return Err(refused("SQLite URI syntax is not accepted for --db"));
        }
        let uid = operating_uid()?;
        let original = fs::symlink_metadata(path)
            .map_err(|e| refused(format!("cannot inspect existing database: {e}")))?;
        check_file(&original, uid, "database")?;
        let canonical = fs::canonicalize(path)
            .map_err(|e| refused(format!("cannot establish existing database path: {e}")))?;
        if canonical.to_str().is_none() {
            return Err(refused("canonical database path must be valid UTF-8"));
        }
        validate_parents(&canonical, uid)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&canonical)
            .map_err(|e| refused(format!("cannot retain existing database: {e}")))?;
        let metadata = file
            .metadata()
            .map_err(|e| refused(format!("cannot inspect retained database: {e}")))?;
        check_file(&metadata, uid, "database")?;
        if metadata.dev() != original.dev() || metadata.ino() != original.ino() {
            return Err(refused("database identity changed while opening"));
        }
        let context = Self {
            path: canonical,
            uid,
            dev: metadata.dev(),
            ino: metadata.ino(),
            file,
        };
        context.validate()?;
        Ok(context)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn uid(&self) -> u32 {
        self.uid
    }

    pub(crate) fn dev(&self) -> u64 {
        self.dev
    }

    pub(crate) fn ino(&self) -> u64 {
        self.ino
    }

    pub(crate) fn validate(&self) -> Result<()> {
        if operating_uid()? != self.uid {
            return Err(refused("operating UID changed"));
        }
        validate_parents(&self.path, self.uid)?;
        let current = fs::symlink_metadata(&self.path)
            .map_err(|e| refused(format!("cannot revalidate database: {e}")))?;
        let retained = self
            .file
            .metadata()
            .map_err(|e| refused(format!("cannot revalidate retained database: {e}")))?;
        for metadata in [&current, &retained] {
            check_file(metadata, self.uid, "database")?;
            if metadata.dev() != self.dev || metadata.ino() != self.ino {
                return Err(refused(
                    "database identity changed; old operation cannot be rebound",
                ));
            }
        }
        check_local_filesystem(&self.file)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = self.path.as_os_str().to_os_string();
            sidecar.push(suffix);
            match fs::symlink_metadata(Path::new(&sidecar)) {
                Ok(metadata) => check_file(&metadata, self.uid, "SQLite sidecar")?,
                Err(e) if e.kind() == io::ErrorKind::NotFound => (),
                Err(e) => return Err(refused(format!("cannot inspect SQLite sidecar: {e}"))),
            }
        }
        Ok(())
    }
}

fn operating_uid() -> Result<u32> {
    // SAFETY: these functions have no pointer arguments or side effects.
    let (uid, euid, gid, egid) = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    #[cfg(target_os = "linux")]
    // SAFETY: getauxval reads process startup metadata and takes no pointers.
    if unsafe { libc::getauxval(libc::AT_SECURE) } != 0 {
        return Err(refused(
            "secure/elevated executable contexts are unsupported",
        ));
    }
    if uid != euid || gid != egid {
        return Err(refused(
            "elevated/setuid/setgid operating contexts are unsupported",
        ));
    }
    Ok(uid)
}

fn check_file(metadata: &Metadata, uid: u32, what: &str) -> Result<()> {
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != uid
        || metadata.mode() & 0o022 != 0
    {
        return Err(refused(format!(
            "{what} must be a regular single-link file owned by the operating UID and not writable by group/others"
        )));
    }
    Ok(())
}

fn validate_parents(path: &Path, uid: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| refused("database has no parent directory"))?;
    let immediate = fs::symlink_metadata(parent)
        .map_err(|e| refused(format!("cannot inspect database directory: {e}")))?;
    if !immediate.is_dir() || immediate.uid() != uid || immediate.mode() & 0o022 != 0 {
        return Err(refused("database directory must be owned by the operating UID and not writable by group/others"));
    }
    // A protected account-owned directory below a root-owned sticky /tmp is safe
    // within Model A. Other group/world-writable or foreign-owned ancestors are refused.
    for ancestor in parent.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|e| refused(format!("cannot inspect database ancestor: {e}")))?;
        if !metadata.is_dir() || (metadata.uid() != 0 && metadata.uid() != uid) {
            return Err(refused(
                "database ancestor is not controlled by root or the operating UID",
            ));
        }
        if metadata.mode() & 0o022 != 0
            && !(metadata.uid() == 0 && metadata.mode() & libc::S_ISVTX != 0)
        {
            return Err(refused(
                "database ancestor is writable by untrusted accounts",
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn check_local_filesystem(file: &File) -> Result<()> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: fstatfs initializes the provided statfs on success; file owns a live FD.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(refused(format!(
            "cannot establish local filesystem: {}",
            io::Error::last_os_error()
        )));
    }
    // SAFETY: checked successful fstatfs above.
    let kind = unsafe { stat.assume_init() }.f_type as u64;
    // Linux filesystem magic values: ext2/3/4, XFS, Btrfs, tmpfs, overlayfs,
    // ramfs and F2FS. Unknown/network/FUSE filesystems are deliberately unsupported.
    if ![
        0xef53,
        0x5846_5342,
        0x9123_683e,
        0x0102_1994,
        0x794c_7630,
        0x8584_58f6,
        0xf2f5_2010,
    ]
    .contains(&kind)
    {
        return Err(refused(
            "database filesystem is not a supported local Linux filesystem",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn check_local_filesystem(_file: &File) -> Result<()> {
    Err(refused(
        "local ownership administration currently requires Linux filesystem validation",
    ))
}

pub(super) fn run(cli: &super::Cli, command: &OwnershipCommand) -> Result<()> {
    let (expected_revision, nonce, operation_id, apply, confirm_intent, json) = match command {
        OwnershipCommand::EnrollLegacy {
            expected_revision,
            nonce,
            operation_id,
            apply,
            confirm_intent,
            json,
            ..
        }
        | OwnershipCommand::EnrollDocumentPreparation {
            expected_revision,
            nonce,
            operation_id,
            apply,
            confirm_intent,
            json,
            ..
        } => (
            expected_revision,
            nonce,
            operation_id,
            apply,
            confirm_intent,
            json,
        ),
    };
    // Check mode even for constructed library Cli values; clap is not an authority check.
    let mode = match (apply, nonce, operation_id, confirm_intent) {
        (false, Some(nonce), None, None) => Some(*nonce),
        (true, None, Some(_), Some(_)) => None,
        _ => return Err(refused(
            "preview requires --nonce; apply requires only --operation-id --apply --confirm-intent",
        )),
    };
    if *expected_revision <= 0 {
        return Err(refused("expected revision must be positive"));
    }
    let context = AdminContext::establish(cli)?;
    let store = SqliteStore::open_ownership_admin(&context)?;
    context.validate()?;
    match command {
        OwnershipCommand::EnrollLegacy { source_id, .. } => match mode {
            Some(nonce) => write_output(
                &store.ownership_preview(&context, *source_id, *expected_revision, nonce)?,
                *json,
            ),
            None => {
                let receipt = store.ownership_enroll(
                    &context,
                    *source_id,
                    *expected_revision,
                    operation_id.as_deref().expect("validated apply mode"),
                    confirm_intent.as_deref().expect("validated apply mode"),
                )?;
                committed_output(&receipt, *json)
            }
        },
        OwnershipCommand::EnrollDocumentPreparation {
            asset_id,
            legacy_source_version,
            document_id,
            document_source_version,
            extraction_id,
            ..
        } => {
            let target = crate::storage::sqlite::document_preparation::PreparationTarget {
                asset_id: *asset_id,
                legacy_source_version: *legacy_source_version,
                document_id: *document_id,
                document_source_version: *document_source_version,
                extraction_id: *extraction_id,
            };
            match mode {
                Some(nonce) => write_output(
                    &store.preparation_preview(&context, &target, *expected_revision, nonce)?,
                    *json,
                ),
                None => {
                    let receipt = store.preparation_enroll(
                        &context,
                        &target,
                        *expected_revision,
                        operation_id.as_deref().expect("validated apply mode"),
                        confirm_intent.as_deref().expect("validated apply mode"),
                    )?;
                    committed_output(&receipt, *json)
                }
            }
        }
    }
}

fn committed_output(value: &impl Serialize, json: bool) -> Result<()> {
    write_output(value, json).map_err(|error| Error::CommitOutcomeUnknown {
        detail: format!("enrollment committed but the receipt could not be written: {error}; retry the same operation"),
    })
}

fn write_output(value: &impl Serialize, json: bool) -> Result<()> {
    let encoded = serde_json::to_string_pretty(value)
        .map_err(|e| Error::Storage(format!("cannot encode administrative result: {e}")))?;
    let display = if json {
        encoded
    } else {
        encoded
            .chars()
            .flat_map(|c| {
                if ('\u{202a}'..='\u{202e}').contains(&c)
                    || ('\u{2066}'..='\u{2069}').contains(&c)
                    || ('\u{200b}'..='\u{200f}').contains(&c)
                    || matches!(c, '\u{061c}' | '\u{180e}' | '\u{2060}' | '\u{feff}')
                {
                    c.escape_unicode().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            })
            .collect()
    };
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(display.as_bytes())
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush())
        .map_err(|e| Error::Storage(format!("cannot write administrative result: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn cli(path: &Path) -> super::super::Cli {
        super::super::Cli {
            db: Some(path.to_path_buf()),
            db_origin: DbOrigin::CommandLine,
            command: super::super::Command::Init,
        }
    }

    #[test]
    fn missing_target_is_never_created_and_environment_is_not_authority() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.db");
        assert!(AdminContext::establish(&cli(&path)).is_err());
        assert!(!path.exists());
        fs::write(&path, b"existing").unwrap();
        let mut command = cli(&path);
        command.db_origin = DbOrigin::Environment;
        assert!(AdminContext::establish(&command).is_err());
        command.db_origin = DbOrigin::Unset;
        assert!(AdminContext::establish(&command).is_err());
    }

    #[test]
    fn symlinks_hardlinks_and_writable_targets_cannot_authorize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        fs::write(&path, b"existing").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(AdminContext::establish(&cli(&link)).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&path, &link).unwrap();
        assert!(AdminContext::establish(&cli(&path)).is_err());
        fs::remove_file(&link).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).unwrap();
        assert!(AdminContext::establish(&cli(&path)).is_err());
    }

    #[test]
    fn retained_target_replacement_and_sidecar_substitution_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        fs::write(&path, b"existing").unwrap();
        let context = AdminContext::establish(&cli(&path)).unwrap();
        let sidecar = dir.path().join("db-wal");
        std::os::unix::fs::symlink(&path, &sidecar).unwrap();
        assert!(context.validate().is_err());
        fs::remove_file(&sidecar).unwrap();
        fs::rename(&path, dir.path().join("old-db")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(context.validate().is_err());
    }

    #[test]
    fn writable_database_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        fs::write(&path, b"existing").unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(AdminContext::establish(&cli(&path)).is_err());
    }

    #[test]
    fn source_ids_and_revisions_have_one_unambiguous_form() {
        let uuid = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        assert_eq!(full_uuid(&uuid.to_string()).unwrap(), uuid);
        assert!(full_uuid(&uuid.simple().to_string()).is_err());
        assert!(full_uuid(&uuid.to_string().to_uppercase()).is_err());
        assert!(positive_revision("0").is_err());
        assert!(positive_revision("-1").is_err());
        assert_eq!(positive_revision("7").unwrap(), 7);
    }
}
