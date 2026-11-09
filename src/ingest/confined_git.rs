//! M7.3b.5b: the confined Git reader.
//!
//! The reader a sandboxed extraction worker uses to read ONE repository, HEAD only, without ever
//! changing it. It differs from the in-process reader ([`super::git::GitReader`]) in exactly the
//! ways the confinement needs, and shares its command execution and parsers:
//!
//! * **Authority is a descriptor.** The caller passes the repository root as an already verified
//!   directory descriptor (in the worker: descriptor 4, placed by the broker). Everything else is
//!   opened *beneath* it with `openat(.., O_NOFOLLOW)`: no path of the filesystem is ever
//!   resolved, so there is nothing to swap.
//! * **Git inherits descriptors; it is not given paths.** The git directory's descriptor is
//!   inherited by each git child under the same number and named `GIT_DIR=/proc/self/fd/<n>`
//!   (resolved in the child). The worker's own `/proc/<pid>/fd/<n>` is NOT used for a child: a
//!   child of a non-dumpable process may not open it (see `docs/design-m7.3b5-git-worker.md`
//!   section 3 and the tests).
//! * **The executable is explicit.** An absolute path, resolved once by the caller (the broker, in
//!   the worker design), with its ELF interpreter, so that the kernel sandbox can grant execution
//!   of exactly those two files. No `PATH` lookup, no shell, a cleared environment.
//! * **Read only, by construction.** The reader opens nothing for writing and creates nothing; in
//!   particular it never creates the identity marker. The worker's Landlock rule is read-only too.
//! * **Refuse what v1 cannot confine.** A linked worktree (`.git` is a file, or the git directory
//!   has a `commondir`), a repository that has linked worktrees (`worktrees/`), external object
//!   alternates, a grafts file, and links among the top-level entries of the git directory are
//!   refused with a precise error. A bare repository is refused too (v1 reads `<root>/.git`).
//!
//! Scope: HEAD. The API has no ref listing, no history walk, no branch/remote/tag access.

use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, RawFd};
use std::path::{Path, PathBuf};

use std::collections::{BTreeSet, HashSet};

use super::deadline::Deadline;
use super::git::{parse_commit, parse_tree, ConfinedExec, GitPin, GitReader};
use super::gitverify::{hex_encode, HeadClosure, RawObject, VerifyLimits};
use crate::core::{
    Error, GitBlobData, GitCommitData, GitObjectKind, GitTagData, GitTreeData, Result, Uuid,
};

fn refuse(msg: impl Into<String>) -> Error {
    Error::Storage(msg.into())
}

/// The git binary and the one other file the kernel executes on its behalf (the ELF
/// interpreter), both canonical absolute paths. These are the only two files the worker's sandbox
/// may be allowed to execute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitExecutable {
    exe: PathBuf,
    interpreter: Option<PathBuf>,
    /// Set by [`GitExecutable::resolve_trusted`] only: what the files were when the trust policy
    /// was applied. The worker re-checks it on the descriptors it opens (see [`FilePin`]).
    exe_pin: Option<FilePin>,
    interpreter_pin: Option<FilePin>,
}

impl GitExecutable {
    /// Validates `path` (absolute, a regular executable file that neither group nor others can
    /// write, an ELF binary) and finds its interpreter. A script is refused: its interpreter
    /// would be a third executable.
    pub fn resolve(path: &Path) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        if !path.is_absolute() {
            return Err(refuse("the git executable must be an absolute path"));
        }
        let exe = std::fs::canonicalize(path).map_err(|e| Error::GitUnavailable {
            detail: format!("git executable {}: {e}", path.display()),
        })?;
        let meta = std::fs::metadata(&exe).map_err(|e| Error::GitUnavailable {
            detail: format!("git executable {}: {e}", exe.display()),
        })?;
        let mode = meta.permissions().mode();
        if !meta.is_file() || mode & 0o111 == 0 {
            return Err(refuse(format!(
                "{} is not an executable file",
                exe.display()
            )));
        }
        if mode & 0o022 != 0 {
            return Err(refuse(format!(
                "{} is writable by group or others; not trusted as the git executable",
                exe.display()
            )));
        }
        let interpreter = elf_interpreter(&exe)?
            .map(|i| {
                std::fs::canonicalize(&i).map_err(|e| Error::GitUnavailable {
                    detail: format!("ELF interpreter {}: {e}", i.display()),
                })
            })
            .transpose()?;
        Ok(Self {
            exe,
            interpreter,
            exe_pin: None,
            interpreter_pin: None,
        })
    }

    pub fn exe(&self) -> &Path {
        &self.exe
    }

    pub fn interpreter(&self) -> Option<&Path> {
        self.interpreter.as_deref()
    }

    /// The files a sandbox must allow to be executed, and nothing else.
    pub fn exec_grants(&self) -> Vec<PathBuf> {
        let mut v = vec![self.exe.clone()];
        v.extend(self.interpreter.clone());
        v
    }
}

/// Who may own the files and directories the git executable is reached through. The default is
/// root alone: nobody but root can then change what the path leads to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustPolicy {
    pub owners: Vec<u32>,
}

impl Default for TrustPolicy {
    fn default() -> Self {
        Self { owners: vec![0] }
    }
}

/// What a trusted file was when the policy was applied, compared again by the worker on the
/// descriptor it opens before it confines itself. A replaced file (another inode), a file modified
/// in place (another size or digest) and a changed owner or mode are all refused.
///
/// This pins the file between the policy check and the worker's start. It does NOT pin what
/// executes later: the worker's Landlock rule is bound to the verified inode, so a path swapped
/// afterwards cannot run (fail closed), but a file modified in place afterwards would. Writing it
/// requires the owner or root; the trust policy exists so that is nobody the server defends against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePin {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub uid: u32,
    pub mode: u32,
    pub sha256: String,
}

impl FilePin {
    /// `dev:ino:size:uid:mode:sha256`, the form passed on the worker's command line.
    pub fn to_arg(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.dev, self.ino, self.size, self.uid, self.mode, self.sha256
        )
    }

    pub fn parse(arg: &str) -> Result<Self> {
        let bad = || refuse("a malformed executable pin");
        let p: Vec<&str> = arg.split(':').collect();
        let [dev, ino, size, uid, mode, sha] = p.as_slice() else {
            return Err(bad());
        };
        if sha.len() != 64
            || !sha
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(bad());
        }
        Ok(Self {
            dev: dev.parse().map_err(|_| bad())?,
            ino: ino.parse().map_err(|_| bad())?,
            size: size.parse().map_err(|_| bad())?,
            uid: uid.parse().map_err(|_| bad())?,
            mode: mode.parse().map_err(|_| bad())?,
            sha256: sha.to_string(),
        })
    }

    fn of(file: &File, what: &Path) -> Result<Self> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::MetadataExt;
        let io = |e: std::io::Error| Error::GitUnavailable {
            detail: format!("{}: {e}", what.display()),
        };
        let meta = file.metadata().map_err(io)?;
        if !meta.is_file() {
            return Err(refuse(format!("{} is not a regular file", what.display())));
        }
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        let mut f = file;
        loop {
            let n = f.read(&mut buf).map_err(io)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > MAX_EXECUTABLE_BYTES {
                return Err(refuse(format!("{} is implausibly large", what.display())));
            }
            h.update(&buf[..n]);
        }
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            uid: meta.uid(),
            mode: meta.mode(),
            sha256: hex_encode(&h.finalize()),
        })
    }
}

/// Larger than any real git binary or dynamic loader.
const MAX_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;

/// Opens `path` without following a link, reads the pin of the OPENED file and compares it with
/// `expected`. Returns the file, so that the caller can bind the sandbox's execute right to exactly
/// this inode (no second open of the path).
pub fn verify_pin(path: &Path, expected: &FilePin) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| Error::GitUnavailable {
            detail: format!("{}: {e}", path.display()),
        })?;
    let now = FilePin::of(&file, path)?;
    if &now != expected {
        return Err(refuse(format!(
            "{} is not the file that was approved (it was replaced or modified since the server started, or its owner or mode changed)",
            path.display()
        )));
    }
    Ok(file)
}

fn trusted_owner(policy: &TrustPolicy, uid: u32) -> bool {
    policy.owners.contains(&uid)
}

/// Resolves `path` component by component the way the kernel would, and requires that EVERYTHING
/// it passes through is trusted: every directory and the final file owned by an accepted uid and
/// not writable by group or others (a sticky bit does not excuse that), every symbolic link owned
/// by an accepted uid. Returns the resulting path, free of links. This is the attacker model:
/// whoever is not an accepted owner cannot change what the path leads to, because they can
/// neither write a directory entry nor a file on the way.
fn trusted_resolve(path: &Path, policy: &TrustPolicy) -> Result<PathBuf> {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute() {
        return Err(refuse("the git executable must be an absolute path"));
    }
    let components = |p: &Path| -> VecDeque<OsString> {
        p.components()
            .filter_map(|c| match c {
                std::path::Component::Normal(n) => Some(n.to_os_string()),
                std::path::Component::ParentDir => Some(OsString::from("..")),
                _ => None,
            })
            .collect()
    };
    let untrusted = |what: &str, p: &Path| {
        refuse(format!(
            "{what} {} is not trusted: it must be owned by an accepted owner (default: root) and not writable by group or others",
            p.display()
        ))
    };
    let check_dir = |md: &std::fs::Metadata, p: &Path| -> Result<()> {
        if !trusted_owner(policy, md.uid()) || md.mode() & 0o022 != 0 {
            return Err(untrusted("the directory", p));
        }
        Ok(())
    };
    let io = |p: &Path, e: std::io::Error| Error::GitUnavailable {
        detail: format!("git executable {}: {e}", p.display()),
    };
    let mut resolved = PathBuf::from("/");
    check_dir(
        &std::fs::symlink_metadata("/").map_err(|e| io(&resolved, e))?,
        &resolved,
    )?;
    let mut todo = components(path);
    let mut links = 0;
    while let Some(name) = todo.pop_front() {
        if name == ".." {
            resolved.pop();
            continue;
        }
        let next = resolved.join(&name);
        let md = std::fs::symlink_metadata(&next).map_err(|e| io(&next, e))?;
        if md.file_type().is_symlink() {
            links += 1;
            if links > 40 {
                return Err(refuse(
                    "too many symbolic links on the path of the git executable",
                ));
            }
            if !trusted_owner(policy, md.uid()) {
                return Err(untrusted("the symbolic link", &next));
            }
            let target = std::fs::read_link(&next).map_err(|e| io(&next, e))?;
            if target.is_absolute() {
                resolved = PathBuf::from("/");
            }
            for c in components(&target).into_iter().rev() {
                todo.push_front(c);
            }
        } else if md.is_dir() {
            check_dir(&md, &next)?;
            resolved = next;
        } else if todo.is_empty() {
            if !trusted_owner(policy, md.uid()) || md.mode() & 0o022 != 0 {
                return Err(untrusted("the file", &next));
            }
            resolved = next;
        } else {
            return Err(refuse(format!("{} is not a directory", next.display())));
        }
    }
    Ok(resolved)
}

impl GitExecutable {
    /// The production resolver: [`GitExecutable::resolve`] plus the trust policy and the pins.
    ///
    /// Required, else an error (and the Git tool is not offered): the path is absolute; the whole
    /// path to the binary, links included, is trusted ([`TrustPolicy`]); the same for the
    /// `PT_INTERP` path of the binary; the binary is a 64-bit ELF; the pins of both files are
    /// taken from descriptors that were opened without following a link.
    pub fn resolve_trusted(path: &Path, policy: &TrustPolicy) -> Result<Self> {
        let exe_path = trusted_resolve(path, policy)?;
        let mut g = Self::resolve(&exe_path)?;
        if g.exe != exe_path {
            return Err(refuse(
                "the git executable changed while it was being resolved",
            ));
        }
        let interp_trusted = match elf_interpreter(&exe_path)? {
            Some(raw) => {
                let resolved = trusted_resolve(&raw, policy)?;
                if g.interpreter.as_deref() != Some(resolved.as_path()) {
                    return Err(refuse(
                        "the ELF interpreter changed while it was being resolved",
                    ));
                }
                Some(resolved)
            }
            None => None,
        };
        let pin = |p: &Path| -> Result<FilePin> {
            use std::os::unix::fs::OpenOptionsExt;
            let f = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(p)
                .map_err(|e| Error::GitUnavailable {
                    detail: format!("{}: {e}", p.display()),
                })?;
            let pin = FilePin::of(&f, p)?;
            // the descriptor, not the path, decides: owner and mode of what was actually opened
            if !trusted_owner(policy, pin.uid) || pin.mode & 0o022 != 0 {
                return Err(refuse(format!("{} is not trusted", p.display())));
            }
            Ok(pin)
        };
        g.exe_pin = Some(pin(&exe_path)?);
        g.interpreter_pin = interp_trusted.as_deref().map(pin).transpose()?;
        Ok(g)
    }

    /// The pins taken by [`GitExecutable::resolve_trusted`] (binary, interpreter), if any.
    pub fn pins(&self) -> Option<(&FilePin, Option<&FilePin>)> {
        self.exe_pin
            .as_ref()
            .map(|p| (p, self.interpreter_pin.as_ref()))
    }

    /// Re-checks the pins on freshly opened descriptors and returns those descriptors (binary,
    /// interpreter) for the sandbox. An executable without pins is refused here: a caller that
    /// wants the trusted form must have it.
    pub fn verify_pins(&self) -> Result<Vec<File>> {
        let Some((exe_pin, interp_pin)) = self.pins() else {
            return Err(refuse("the git executable carries no pin"));
        };
        let mut files = vec![verify_pin(&self.exe, exe_pin)?];
        match (&self.interpreter, interp_pin) {
            (Some(path), Some(pin)) => files.push(verify_pin(path, pin)?),
            (None, None) => {}
            _ => return Err(refuse("the interpreter pin does not match the executable")),
        }
        Ok(files)
    }

    /// The same executable with the given pins attached (the worker rebuilds the form the broker
    /// had from its command line).
    pub fn with_pins(mut self, exe: FilePin, interpreter: Option<FilePin>) -> Self {
        self.exe_pin = Some(exe);
        self.interpreter_pin = interpreter;
        self
    }
}

/// The `PT_INTERP` string of a 64-bit little-endian ELF file; `None` for a static binary.
fn elf_interpreter(path: &Path) -> Result<Option<PathBuf>> {
    let unreadable = |e: std::io::Error| Error::GitUnavailable {
        detail: format!("cannot read {}: {e}", path.display()),
    };
    let mut head = Vec::new();
    File::open(path)
        .map_err(unreadable)?
        .take(1 << 20)
        .read_to_end(&mut head)
        .map_err(unreadable)?;
    let not_elf = || {
        refuse(format!(
            "{} is not a 64-bit little-endian ELF binary",
            path.display()
        ))
    };
    if head.len() < 64 || &head[..4] != b"\x7fELF" || head[4] != 2 || head[5] != 1 {
        return Err(not_elf());
    }
    let u16_at = |o: usize| u16::from_le_bytes([head[o], head[o + 1]]) as usize;
    let u32_at = |o: usize| u32::from_le_bytes(head[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(head[o..o + 8].try_into().unwrap()) as usize;
    let (phoff, phentsize, phnum) = (u64_at(0x20), u16_at(0x36), u16_at(0x38));
    for i in 0..phnum {
        let base = phoff.checked_add(i * phentsize).ok_or_else(not_elf)?;
        if base + 56 > head.len() {
            return Err(not_elf());
        }
        if u32_at(base) == 3 {
            let (off, len) = (u64_at(base + 8), u64_at(base + 32));
            let raw = head
                .get(off..off.checked_add(len).ok_or_else(not_elf)?)
                .ok_or_else(not_elf)?;
            let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
            let text = std::str::from_utf8(&raw[..end]).map_err(|_| not_elf())?;
            return Ok(Some(PathBuf::from(text)));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------------- openat

fn cname(name: &str) -> std::io::Result<CString> {
    CString::new(name).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}

/// `openat(dir, name)` without following a link at `name`, close-on-exec.
fn open_beneath(dir: RawFd, name: &str, flags: i32) -> std::io::Result<File> {
    let c = cname(name)?;
    // SAFETY: `dir` is an open descriptor owned by the caller; the result is wrapped at once.
    let fd = unsafe { libc::openat(dir, c.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if fd < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        // SAFETY: `fd` is a fresh descriptor that nothing else owns.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`: the file type bits, or `None` when absent.
fn kind_of(dir: RawFd, name: &str) -> std::io::Result<Option<u32>> {
    let c = cname(name)?;
    // SAFETY: a zeroed `stat` is a valid output buffer; `dir` is open.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::fstatat(dir, c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
    if r == 0 {
        return Ok(Some(st.st_mode & libc::S_IFMT));
    }
    let e = std::io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(e)
    }
}

/// Whether `path` (several components) exists beneath `dir`, with no link on the way: an
/// intermediate that is a link or not a directory is an error, not a "no".
fn exists_beneath(dir: RawFd, path: &[&str]) -> Result<bool> {
    let io = |e: std::io::Error| Error::Io {
        path: path.join("/"),
        source: e,
    };
    let Some((last, parents)) = path.split_last() else {
        return Ok(true);
    };
    let mut held: Option<File> = None;
    for part in parents {
        let at = held.as_ref().map(|f| f.as_raw_fd()).unwrap_or(dir);
        match open_beneath(at, part, libc::O_RDONLY | libc::O_DIRECTORY) {
            Ok(f) => held = Some(f),
            Err(e) if e.raw_os_error() == Some(libc::ENOENT) => return Ok(false),
            Err(e) => {
                return Err(refuse(format!(
                    "`{}` inside the git directory is a link or not a directory ({e})",
                    part
                )))
            }
        }
    }
    let at = held.as_ref().map(|f| f.as_raw_fd()).unwrap_or(dir);
    Ok(kind_of(at, last).map_err(io)?.is_some())
}

/// Every id must be exactly 40 lowercase hex digits. Also keeps an argument that is not an id
/// (for example one starting with `-`) from ever reaching git's command line or its stdin.
fn require_sha1_oids(oids: &[String]) -> Result<()> {
    match oids.iter().find(|o| {
        o.len() != 40
            || !o
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }) {
        None => Ok(()),
        Some(bad) => Err(refuse(format!(
            "unsupported object id `{}`: only 40-digit SHA-1 object ids are supported",
            bad.chars().take(80).collect::<String>()
        ))),
    }
}

// ---------------------------------------------------------------------------------- reader

/// What HEAD points at: the commit and every entry of its tree (`mode`, `kind`, `oid`, `path`;
/// symlinks and gitlinks keep their true modes so the caller can leave them out).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadSnapshot {
    pub commit: String,
    pub files: Vec<(String, String, String, String)>,
}

/// The layout checks of [`ConfinedGitReader::open`], without starting any process: `<root>/.git`
/// must be a real directory of a plain (non-linked, non-bare) repository without alternates or
/// grafts. Returns the opened git directory. The broker runs this too, before it starts a worker,
/// so an unsupported layout never reaches one.
fn check_layout(root_fd: RawFd) -> Result<File> {
    match kind_of(root_fd, ".") {
        Ok(Some(k)) if k == libc::S_IFDIR => {}
        _ => return Err(refuse("the root descriptor is not a directory")),
    }
    match kind_of(root_fd, ".git").map_err(|e| Error::Io {
            path: ".git".into(),
            source: e,
        })? {
            None => {
                return Err(refuse(
                    "not a git repository: the root has no `.git` (bare repositories are not supported)",
                ))
            }
            Some(k) if k == libc::S_IFLNK => {
                return Err(refuse(
                    "`.git` is a symbolic link; a repository that points elsewhere through a link is not supported",
                ))
            }
            Some(k) if k == libc::S_IFREG => {
                return Err(refuse(
                    "unsupported repository: `.git` is a file (a linked worktree); linked worktrees are not supported",
                ))
            }
            Some(k) if k == libc::S_IFDIR => {}
            Some(_) => return Err(refuse("`.git` is neither a directory nor a file")),
        }
    let git_dir = open_beneath(root_fd, ".git", libc::O_RDONLY | libc::O_DIRECTORY)
        .map_err(|e| refuse(format!("cannot open `.git`: {e}")))?;
    let gfd = git_dir.as_raw_fd();
    // layouts v1 cannot confine
    if exists_beneath(gfd, &["commondir"])? {
        return Err(refuse(
            "unsupported repository: the git directory belongs to a linked worktree (`commondir`)",
        ));
    }
    if exists_beneath(gfd, &["worktrees"])? {
        return Err(refuse(
                "unsupported repository: it has linked worktrees (`.git/worktrees`); they are not supported",
            ));
    }
    for alt in ["alternates", "http-alternates"] {
        if exists_beneath(gfd, &["objects", "info", alt])? {
            return Err(refuse(format!(
                    "unsupported repository: object alternates (`objects/info/{alt}`) are not supported"
                )));
        }
    }
    if exists_beneath(gfd, &["info", "grafts"])? {
        return Err(refuse(
            "unsupported repository: `info/grafts` rewrites history and cannot be switched off",
        ));
    }
    // top-level entries git follows; a link among them is refused (a link cannot leave the
    // authorized root, the kernel stops that, but it can point somewhere else inside it)
    for name in [
        "HEAD",
        "config",
        "objects",
        "refs",
        "packed-refs",
        "info",
        "shallow",
    ] {
        if let Some(k) = kind_of(gfd, name).map_err(|e| Error::Io {
            path: name.into(),
            source: e,
        })? {
            if k == libc::S_IFLNK {
                return Err(refuse(format!(
                    "the git directory contains a symbolic link (`{name}`); not supported"
                )));
            }
        }
    }
    if kind_of(gfd, "HEAD").ok().flatten() != Some(libc::S_IFREG) {
        return Err(refuse("not a git repository: `.git/HEAD` is missing"));
    }
    Ok(git_dir)
}

/// The identity marker of the repository whose git directory is `git_dir_fd`: read-only,
/// `O_NOFOLLOW`, at most 256 bytes, never created. A missing marker is `identity_missing`; a marker
/// that is not a UUID is refused.
fn read_marker_at(git_dir_fd: RawFd) -> Result<Uuid> {
    const NAME: &str = "mindspace-repository-id";
    let at = |reason: String| Error::RepositoryIdentity {
        path: format!(".git/{NAME}"),
        reason,
    };
    let mut f = match open_beneath(git_dir_fd, NAME, libc::O_RDONLY) {
        Ok(f) => f,
        Err(e) if e.raw_os_error() == Some(libc::ENOENT) => {
            return Err(at(
                "identity_missing: this repository has no identity marker; ingest it once with `mind ingest`"
                    .into(),
            ))
        }
        Err(e) => return Err(at(format!("cannot read marker: {e}"))),
    };
    let mut text = String::new();
    (&mut f)
        .take(256)
        .read_to_string(&mut text)
        .map_err(|e| at(format!("cannot read marker: {e}")))?;
    Uuid::parse_str(text.trim()).map_err(|_| at("marker exists but is not a UUID".into()))
}

/// What the BROKER learns about an authorized root before it starts a worker, without starting
/// any process and without parsing repository content: the layout is one this milestone can
/// confine ([`ConfinedGitReader::open`] applies the same checks) and the repository has an
/// identity marker. The broker binds that identity to the operation; the worker cannot choose
/// another. (The marker is repository content: a hostile repository can ship any UUID in it. That
/// limitation is documented and unchanged.)
pub fn inspect_root(root: BorrowedFd<'_>) -> Result<Uuid> {
    let root_file: File = root
        .try_clone_to_owned()
        .map_err(|e| Error::Io {
            path: "<root descriptor>".into(),
            source: e,
        })?
        .into();
    let git_dir = check_layout(root_file.as_raw_fd())?;
    read_marker_at(git_dir.as_raw_fd())
}

/// A Git reader for a sandboxed worker. See the module documentation.
pub struct ConfinedGitReader {
    inner: GitReader,
    /// For the identity marker: read through `openat`, never created.
    git_dir: File,
    #[cfg_attr(not(feature = "spawn-gate"), allow(dead_code))]
    exec: ConfinedExec,
}

impl ConfinedGitReader {
    /// Opens `<root>/.git` beneath the verified root descriptor and refuses everything v1 cannot
    /// confine (module documentation). `root` is not consumed: it is duplicated.
    pub fn open(root: BorrowedFd<'_>, git: &GitExecutable, deadline: Deadline) -> Result<Self> {
        let root_file: File = root
            .try_clone_to_owned()
            .map_err(|e| Error::Io {
                path: "<root descriptor>".into(),
                source: e,
            })?
            .into();
        let root_fd = root_file.as_raw_fd();
        let git_dir = check_layout(root_fd)?;
        let gfd = git_dir.as_raw_fd();
        let exec = ConfinedExec {
            exe: git.exe().to_path_buf(),
            git_dir_fd: gfd,
            root_fd,
            #[cfg(feature = "spawn-gate")]
            apply_pins: true,
        };
        let marker_dir = git_dir.try_clone().map_err(|e| Error::Io {
            path: ".git".into(),
            source: e,
        })?;
        let pin = GitPin {
            git_dir: PathBuf::from(format!("/proc/self/fd/{gfd}")),
            common_dir: PathBuf::from(format!("/proc/self/fd/{gfd}")),
            // the descriptors must outlive every child: the reader owns them
            keep: vec![git_dir, root_file],
            confined: Some(exec.clone()),
        };
        // `work` is only a label here; a confined child changes into the root by descriptor
        let reader = Self {
            inner: GitReader::pinned(PathBuf::from("/"), deadline, pin),
            git_dir: marker_dir,
            exec,
        };
        // Object ids are 40 hex digits (SHA-1) throughout the stored model and the tree parser
        // (`parse_tree` reads 20-byte entries): a SHA-256 repository would be misread, not
        // rejected, so it is refused here, by what git itself reports. (Milestone 5c decides
        // whether SHA-256 is supported; see docs/m7.3b5c-verified-git-transport-plan.md.)
        reader.inner.require_sha1()?;
        Ok(reader)
    }

    /// The Mindspace identity of the repository: the marker that already exists. A repository
    /// without one is an error that says so; the reader never creates one.
    pub fn repository_id(&self) -> Result<Uuid> {
        read_marker_at(self.git_dir.as_raw_fd())
    }

    /// The commit HEAD resolves to and the entries of its tree. An unborn HEAD is an error.
    pub fn head(&self) -> Result<HeadSnapshot> {
        let commit = self.inner.head_commit()?;
        require_sha1_oids(std::slice::from_ref(&commit))?;
        let files = self.inner.commit_files(&commit)?;
        Ok(HeadSnapshot { commit, files })
    }

    /// HEAD's commit and every tree below its root tree as RAW objects, and the `(oid, size)` of
    /// every blob those trees name: what a worker sends for a verified transport
    /// ([`crate::ingest::stream::Kind::GitVerified`]). The limits are enforced while the closure
    /// is collected, so an oversized repository stops early; a subtree shared by several paths
    /// is read once. Parents are NOT read.
    pub fn head_closure(&self, limits: &VerifyLimits) -> Result<HeadClosure> {
        let commit_oid = self.inner.head_commit()?;
        require_sha1_oids(std::slice::from_ref(&commit_oid))?;
        let symbolic = self.inner.head_symbolic()?;
        let too_big =
            |what: &str| refuse(format!("the repository is too large to transport: {what}"));
        let mut objects: Vec<RawObject> = Vec::new();
        let mut total: u64 = 0;
        let mut take = |raw: Vec<(String, GitObjectKind, Vec<u8>)>,
                        objects: &mut Vec<RawObject>|
         -> Result<()> {
            for (oid, kind, bytes) in raw {
                if bytes.len() > limits.max_raw_object_bytes {
                    return Err(too_big(&format!("object {oid} is {} bytes", bytes.len())));
                }
                total = total.saturating_add(bytes.len() as u64);
                if total > limits.max_total_object_bytes {
                    return Err(too_big("the objects exceed the byte limit"));
                }
                objects.push(RawObject { kind, oid, bytes });
            }
            Ok(())
        };
        take(
            self.inner.raw_objects(std::slice::from_ref(&commit_oid))?,
            &mut objects,
        )?;
        let commit = parse_commit(&commit_oid, &objects[0].bytes)?;
        let mut seen: HashSet<String> = HashSet::new();
        let mut level = vec![commit.tree_oid.clone()];
        seen.insert(commit.tree_oid.clone());
        let mut blobs: BTreeSet<String> = BTreeSet::new();
        let mut entries = 0usize;
        let mut depth = 0usize;
        while !level.is_empty() {
            depth += 1;
            if depth > limits.max_depth + 1 {
                return Err(too_big("directories are nested too deeply"));
            }
            let mut next: Vec<String> = Vec::new();
            for chunk in level.chunks(256) {
                if seen.len() > limits.max_trees {
                    return Err(too_big("too many trees"));
                }
                let before = objects.len();
                take(self.inner.raw_objects(chunk)?, &mut objects)?;
                for o in &objects[before..] {
                    let t = parse_tree(&o.oid, &o.bytes)?;
                    entries = entries.saturating_add(t.entries.len());
                    if entries > limits.max_tree_entries {
                        return Err(too_big("too many tree entries"));
                    }
                    for e in t.entries {
                        match e.target_kind {
                            GitObjectKind::Tree => {
                                if seen.insert(e.target_oid.clone()) {
                                    next.push(e.target_oid);
                                }
                            }
                            GitObjectKind::Blob => {
                                blobs.insert(e.target_oid);
                            }
                            _ => {}
                        }
                    }
                }
            }
            level = next;
        }
        if blobs.len() > limits.max_blobs {
            return Err(too_big("too many blobs"));
        }
        let wanted: Vec<String> = blobs.into_iter().collect();
        let blobs = self
            .inner
            .batch_check(&wanted)?
            .into_iter()
            .map(|(oid, _, size)| (oid, size))
            .collect();
        Ok(HeadClosure {
            symbolic,
            commit_oid,
            objects,
            blobs,
        })
    }

    /// Types and sizes of the given objects (missing ones are left out).
    pub fn batch_check(&self, oids: &[String]) -> Result<Vec<(String, GitObjectKind, u64)>> {
        require_sha1_oids(oids)?;
        self.inner.batch_check(oids)
    }

    /// The commits, trees and annotated tags among `oids`, parsed.
    pub fn objects(
        &self,
        oids: &[String],
    ) -> Result<(Vec<GitCommitData>, Vec<GitTreeData>, Vec<GitTagData>)> {
        require_sha1_oids(oids)?;
        self.inner.objects(oids)
    }

    /// The configured remotes `(name, raw url)`; the url is sanitized by the caller.
    pub fn remotes(&self) -> Result<Vec<(String, String)>> {
        self.inner.remotes()
    }

    /// Blob contents as the exact bytes git stores (no text conversion), by oid. One git call.
    pub fn blob_bytes(&self, oids: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
        require_sha1_oids(oids)?;
        Ok(self
            .inner
            .raw_objects(oids)?
            .into_iter()
            .filter(|(_, kind, _)| *kind == GitObjectKind::Blob)
            .map(|(oid, _, bytes)| (oid, bytes))
            .collect())
    }

    /// Blob contents by oid.
    pub fn blob_contents(&self, oids: &[String]) -> Result<Vec<GitBlobData>> {
        require_sha1_oids(oids)?;
        self.inner.blob_contents(oids)
    }

    /// The descriptor number git children inherit (test harness).
    #[cfg(feature = "spawn-gate")]
    pub fn git_dir_fd(&self) -> RawFd {
        self.exec.git_dir_fd
    }

    /// Any git arguments through the hardened command builder (test harness). With
    /// `pins == false` the `-c` overrides are left off, to show what the kernel alone stops.
    #[cfg(feature = "spawn-gate")]
    pub fn run_raw(&self, args: &[&str], pins: bool) -> Result<Vec<u8>> {
        let mut exec = self.exec.clone();
        exec.apply_pins = pins;
        self.with_exec(exec).run_raw(args, None)
    }

    /// Runs `rev-parse HEAD` with the git directory's descriptor number replaced by one that is
    /// not open (test harness): the spawn must fail.
    #[cfg(feature = "spawn-gate")]
    pub fn run_without_descriptor(&self) -> Result<Vec<u8>> {
        let mut exec = self.exec.clone();
        exec.git_dir_fd = 987;
        self.with_exec(exec).run_raw(&["rev-parse", "HEAD"], None)
    }

    /// Runs `rev-parse HEAD` with `GIT_DIR` naming an inherited descriptor that is NOT a
    /// repository (test harness), while the working directory IS the repository: git must fail,
    /// not discover `.git` from its working directory.
    #[cfg(feature = "spawn-gate")]
    pub fn run_with_foreign_descriptor(&self) -> Result<Vec<u8>> {
        let foreign = File::open("/dev/null").map_err(|e| Error::Io {
            path: "/dev/null".into(),
            source: e,
        })?;
        let mut exec = self.exec.clone();
        exec.git_dir_fd = foreign.as_raw_fd();
        self.with_exec(exec).run_raw(&["rev-parse", "HEAD"], None)
    }

    /// Spawns a git whose executable is replaced (test harness: a script that reports what the
    /// child inherited). Runs with the same descriptors and environment as the real one.
    #[cfg(feature = "spawn-gate")]
    pub fn run_with_executable(&self, exe: &Path, args: &[&str]) -> Result<Vec<u8>> {
        let mut exec = self.exec.clone();
        exec.exe = exe.to_path_buf();
        exec.apply_pins = false;
        self.with_exec(exec).run_raw(args, None)
    }

    /// Starts a git child and returns it with its stdin open (test harness).
    #[cfg(feature = "spawn-gate")]
    pub fn spawn_raw(&self, args: &[&str]) -> std::io::Result<std::process::Child> {
        self.inner.spawn_raw(args)
    }

    #[cfg(feature = "spawn-gate")]
    fn with_exec(&self, exec: ConfinedExec) -> GitReader {
        let pin = GitPin {
            git_dir: PathBuf::from(format!("/proc/self/fd/{}", exec.git_dir_fd)),
            common_dir: PathBuf::from(format!("/proc/self/fd/{}", exec.git_dir_fd)),
            keep: Vec::new(),
            confined: Some(exec),
        };
        GitReader::pinned(PathBuf::from("/"), self.inner.deadline(), pin)
    }
}
