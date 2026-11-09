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

use super::deadline::Deadline;
use super::git::{ConfinedExec, GitPin, GitReader};
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
        Ok(Self { exe, interpreter })
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
        let format = reader.inner.object_format()?;
        if format != "sha1" {
            return Err(refuse(format!(
                "unsupported repository: object format `{format}`; only SHA-1 repositories are supported"
            )));
        }
        Ok(reader)
    }

    /// The Mindspace identity of the repository: the marker that already exists. A repository
    /// without one is an error that says so; the reader never creates one.
    pub fn repository_id(&self) -> Result<Uuid> {
        const NAME: &str = "mindspace-repository-id";
        let at = |reason: String| Error::RepositoryIdentity {
            path: format!(".git/{NAME}"),
            reason,
        };
        let mut f = match open_beneath(self.git_dir.as_raw_fd(), NAME, libc::O_RDONLY) {
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

    /// The commit HEAD resolves to and the entries of its tree. An unborn HEAD is an error.
    pub fn head(&self) -> Result<HeadSnapshot> {
        let commit = self.inner.head_commit()?;
        require_sha1_oids(std::slice::from_ref(&commit))?;
        let files = self.inner.commit_files(&commit)?;
        Ok(HeadSnapshot { commit, files })
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
