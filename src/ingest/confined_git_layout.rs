//! Broker-only discovery of local worktree and object directory pointers. Git receives
//! process-local descriptors and a private object directory with no Git control files.

use super::{exists_beneath, kind_of, read_marker_at};
use crate::core::{Error, Result, Uuid};
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, RawFd};
use std::os::unix::fs::{symlink, DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

const CONTROL_BYTES: u64 = 64 * 1024;
const MAX_STORES: usize = 16;
const MAX_DEPTH: usize = 5;
const MAX_ALTERNATES: usize = 64;
const MAX_FILES: usize = 4096;
const MAX_ENTRIES: usize = 16_384;

pub(crate) fn unsupported(detail: &str) -> Error {
    Error::refused(
        crate::core::RefusalKind::UnsupportedRepository,
        format!("unsupported repository: {detail}"),
    )
}

fn too_large() -> Error {
    Error::refused(crate::core::RefusalKind::TooLarge, "repository too large for isolated object projection: pack loose objects or reduce the number of alternates")
}

fn io_error(source: io::Error) -> Error {
    Error::Io {
        path: "isolated Git layout".into(),
        source,
    }
}

fn procfd(fd: RawFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{fd}"))
}

/// Extended layout inputs cannot cross a bind mount or descend through symlinks. The
/// operator can authorize the root of a mounted filesystem itself.
fn open_beneath(dir: RawFd, name: &str, flags: i32) -> io::Result<File> {
    let name = std::ffi::CString::new(name)?;
    // Linux open_how consists of these three aligned u64 fields. RESOLVE_NO_XDEV,
    // NO_MAGICLINKS, NO_SYMLINKS and BENEATH apply to this descriptor-relative lookup.
    let how = [
        (flags | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64,
        0,
        0x01 | 0x02 | 0x04 | 0x08,
    ];
    // SAFETY: the C string and complete open_how buffer live through the syscall.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dir,
            name.as_ptr(),
            how.as_ptr(),
            std::mem::size_of_val(&how),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat2 returned a new owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}

fn child_dir(dir: &File, name: &str) -> Result<File> {
    open_beneath(dir.as_raw_fd(), name, libc::O_DIRECTORY | libc::O_RDONLY)
        .map_err(|_| unsupported("missing, linked or inaccessible Git metadata directory"))
}

/// The broker has pinned the chosen authority root. Walk only literal path components below it.
pub(crate) fn open_relative(root: &File, path: &Path) -> Result<File> {
    let mut dir = root.try_clone().map_err(io_error)?;
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(unsupported("invalid layout path"));
        };
        dir = child_dir(
            &dir,
            name.to_str()
                .ok_or_else(|| unsupported("non-UTF-8 layout path"))?,
        )?;
    }
    Ok(dir)
}

fn control(dir: &File, name: &str) -> Result<Option<String>> {
    let pin = match open_beneath(dir.as_raw_fd(), name, libc::O_PATH) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    if !pin.metadata().map_err(io_error)?.is_file() {
        return Err(unsupported("Git layout control must be a regular file"));
    }
    let mut bytes = Vec::new();
    File::open(procfd(pin.as_raw_fd()))
        .map_err(io_error)?
        .take(CONTROL_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > CONTROL_BYTES {
        return Err(Error::refused(
            crate::core::RefusalKind::TooLarge,
            "Git layout control exceeds its byte limit",
        ));
    }
    let value = String::from_utf8(bytes).map_err(|_| unsupported("invalid Git layout control"))?;
    if value.contains('\0') {
        return Err(unsupported("malformed Git layout control"));
    }
    Ok(Some(value))
}

fn target(base: &Path, value: &str) -> Result<PathBuf> {
    if value.is_empty()
        || value.len() > libc::PATH_MAX as usize
        || value.contains(['\r', '\n'])
        || value.starts_with('"')
        || value.contains("://")
    {
        return Err(unsupported("malformed or non-local Git layout pointer"));
    }
    let path = Path::new(value);
    let mut out = if path.is_absolute() {
        PathBuf::new()
    } else {
        base.to_owned()
    };
    let mut saw_name = false;
    for component in path.components() {
        match component {
            Component::RootDir => out.push("/"),
            Component::CurDir => (),
            Component::Normal(name) => {
                saw_name = true;
                out.push(name);
            }
            // Native worktree and alternate pointers use leading parent components.
            // Collapsing a parent after a name could change the meaning across a symlink.
            Component::ParentDir if !saw_name && out.pop() => (),
            _ => return Err(unsupported("ambiguous Git layout pointer")),
        }
    }
    Ok(out)
}

/// Detached fd numbers are passed in broker-owned argv. Keep them above stdio and worker IPC.
fn stable(file: File) -> Result<File> {
    if file.as_raw_fd() > 5 {
        return Ok(file);
    }
    // SAFETY: this makes a new owned descriptor for a live file.
    let n = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 6) };
    if n < 0 {
        return Err(io_error(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(n) })
}

pub(crate) struct View {
    // Removal via the pinned parent does not resolve a mutable ancestor again.
    _parent: File,
    path: PathBuf,
}

impl View {
    pub(crate) fn session(parent: &Path) -> Result<Self> {
        let md = fs::symlink_metadata(parent).map_err(io_error)?;
        if !md.is_dir() || md.uid() != unsafe { libc::geteuid() } || md.mode() & 0o077 != 0 {
            return Err(unsupported("private projection directory is unavailable"));
        }
        let parent: File = crate::sandbox::open_verified_dir(parent, (md.dev(), md.ino()))
            .map_err(|_| unsupported("private projection directory changed"))?
            .into();
        Self::new(&parent)
    }

    pub(crate) fn open(&self) -> Result<File> {
        File::open(&self.path).map_err(io_error)
    }

    fn new(parent: &File) -> Result<Self> {
        let parent = parent.try_clone().map_err(io_error)?;
        let path = procfd(parent.as_raw_fd()).join(format!("git-{}", Uuid::now_v7()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(io_error)?;
        Ok(Self {
            _parent: parent,
            path,
        })
    }
}

impl Drop for View {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// The first three descriptors name the per-worktree gitdir, common dir and projected ODB.
/// Every remaining descriptor is an O_PATH pin for one regular object/index file.
pub(crate) struct Prepared {
    pub(crate) files: Vec<File>,
    _view: View,
}

fn check_top(dir: &File) -> Result<()> {
    for name in [
        "HEAD",
        "config",
        "config.worktree",
        "refs",
        "objects",
        "info",
        "packed-refs",
        "shallow",
        "reftable",
    ] {
        if kind_of(dir.as_raw_fd(), name).map_err(io_error)? == Some(libc::S_IFLNK) {
            return Err(unsupported("symbolic link in Git metadata"));
        }
    }
    if exists_beneath(dir.as_raw_fd(), &["info", "grafts"])? {
        return Err(unsupported("info/grafts rewrites history"));
    }
    Ok(())
}

/// The ordinary single-root reader retains its existing capability and resource envelope.
pub(crate) fn prepare(
    root: &File,
    selected: &Path,
    scratch: Option<&File>,
    authorize: impl Fn(&Path) -> Result<File>,
) -> Result<(Uuid, Option<Prepared>)> {
    let (git, git_path, common, common_path) =
        match kind_of(root.as_raw_fd(), ".git").map_err(io_error)? {
            Some(libc::S_IFREG) => {
                let text = control(root, ".git")?.ok_or_else(|| unsupported("missing gitfile"))?;
                let value = text
                    .trim_end_matches(['\r', '\n'])
                    .strip_prefix("gitdir: ")
                    .ok_or_else(|| unsupported("invalid linked worktree gitfile"))?;
                let gp = target(selected, value)?;
                let g = authorize(&gp)?;
                let pointer = control(&g, "commondir")?
                    .ok_or_else(|| unsupported("missing linked worktree commondir"))?;
                let cp = target(&gp, pointer.trim_end_matches(['\r', '\n']))?;
                let c = authorize(&cp)?;
                let link = control(&g, "gitdir")?
                    .ok_or_else(|| unsupported("missing linked worktree backlink"))?;
                if target(&gp, link.trim_end_matches(['\r', '\n']))? != selected.join(".git") {
                    return Err(unsupported(
                        "linked worktree backlink belongs to a different checkout",
                    ));
                }
                if gp.parent().and_then(Path::parent) != Some(cp.as_path())
                    || gp.parent().and_then(Path::file_name) != Some(OsStr::new("worktrees"))
                {
                    return Err(unsupported("invalid linked worktree administration layout"));
                }
                (g, gp, c, cp)
            }
            Some(libc::S_IFDIR) => {
                let git = child_dir(root, ".git")?;
                if !exists_beneath(git.as_raw_fd(), &["objects", "info", "alternates"])? {
                    return Ok((super::inspect_root(root.as_fd())?, None));
                }
                if exists_beneath(git.as_raw_fd(), &["commondir"])? {
                    return Err(unsupported("unexpected commondir"));
                }
                let p = selected.join(".git");
                let checked = authorize(&p)?;
                (
                    checked.try_clone().map_err(io_error)?,
                    p.clone(),
                    checked,
                    p,
                )
            }
            _ => return Ok((super::inspect_root(root.as_fd())?, None)),
        };
    check_top(&git)?;
    if git_path != common_path {
        check_top(&common)?;
    }
    if kind_of(git.as_raw_fd(), "HEAD").map_err(io_error)? != Some(libc::S_IFREG) {
        return Err(unsupported("missing worktree HEAD"));
    }
    let id = read_marker_at(common.as_raw_fd())?;
    let object_path = common_path.join("objects");
    let object_dir = authorize(&object_path)?;
    let view = View::new(scratch.ok_or_else(|| {
        unsupported(
            "private projection directory unavailable for linked worktrees and object alternates",
        )
    })?)?;
    let mut graph = Graph {
        view: &view.path,
        files: vec![stable(git)?, stable(common)?, stable(view.open()?)?],
        entries: 0,
        alternates: 0,
        done: HashSet::new(),
        active: HashSet::new(),
        names: HashSet::new(),
    };
    graph.visit(object_dir, &object_path, &authorize)?;
    Ok((
        id,
        Some(Prepared {
            files: graph.files,
            _view: view,
        }),
    ))
}

struct Graph<'a> {
    view: &'a Path,
    files: Vec<File>,
    entries: usize,
    alternates: usize,
    done: HashSet<(u64, u64)>,
    active: HashSet<(u64, u64)>,
    names: HashSet<PathBuf>,
}

fn hex(name: &str, len: usize) -> bool {
    name.len() == len
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Graph<'_> {
    fn visit(
        &mut self,
        store: File,
        path: &Path,
        authorize: &impl Fn(&Path) -> Result<File>,
    ) -> Result<()> {
        let md = store.metadata().map_err(io_error)?;
        let key = (md.dev(), md.ino());
        if self.active.contains(&key) {
            return Err(unsupported("cycle in local object alternates"));
        }
        if self.done.contains(&key) {
            return Ok(());
        }
        if self.active.len() >= MAX_DEPTH || self.active.len() + self.done.len() >= MAX_STORES {
            return Err(too_large());
        }
        self.active.insert(key);
        let alt = match open_beneath(
            store.as_raw_fd(),
            "info",
            libc::O_RDONLY | libc::O_DIRECTORY,
        ) {
            Ok(info) => {
                if kind_of(info.as_raw_fd(), "http-alternates")
                    .map_err(io_error)?
                    .is_some()
                {
                    return Err(unsupported("HTTP object alternates"));
                }
                control(&info, "alternates")?
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return Err(unsupported("invalid object store info directory")),
        };
        for name in self.entries(&store)? {
            if name == "pack" || hex(&name, 2) {
                let dir = child_dir(&store, &name)?;
                self.project_dir(&name, &dir)?;
            }
        }
        if let Some(lines) = alt {
            for line in lines
                .lines()
                .filter(|s| !s.is_empty() && !s.starts_with('#'))
            {
                self.alternates += 1;
                if self.alternates > MAX_ALTERNATES {
                    return Err(too_large());
                }
                let p = target(path, line)?;
                self.visit(authorize(&p)?, &p, authorize)?;
            }
        }
        self.active.remove(&key);
        self.done.insert(key);
        Ok(())
    }

    fn entries(&mut self, dir: &File) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(procfd(dir.as_raw_fd())).map_err(io_error)? {
            self.entries += 1;
            if self.entries > MAX_ENTRIES {
                return Err(too_large());
            }
            if let Ok(name) = entry.map_err(io_error)?.file_name().into_string() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    fn project_dir(&mut self, name: &str, dir: &File) -> Result<()> {
        let dst = self.view.join(name);
        match fs::DirBuilder::new().mode(0o700).create(&dst) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(io_error(e)),
        }
        let mut pairs: HashMap<String, u8> = HashMap::new();
        for fname in self.entries(dir)? {
            if name == "pack" {
                let Some((stem, suffix)) = fname.rsplit_once('.') else {
                    continue;
                };
                if !stem.strip_prefix("pack-").is_some_and(|hash| hex(hash, 40)) {
                    continue;
                }
                let flag = match suffix {
                    "idx" => 1,
                    "pack" => 2,
                    "promisor" => 0,
                    _ => continue,
                };
                *pairs.entry(stem.to_owned()).or_default() |= flag;
            } else if !hex(&fname, 38) {
                continue;
            }
            let rel = Path::new(name).join(&fname);
            if !self.names.insert(rel) {
                continue;
            }
            if self.files.len() - 3 >= MAX_FILES {
                return Err(too_large());
            }
            let pin = stable(open_beneath(dir.as_raw_fd(), &fname, libc::O_PATH).map_err(
                |e| {
                    if e.raw_os_error() == Some(libc::EMFILE) {
                        too_large()
                    } else {
                        io_error(e)
                    }
                },
            )?)?;
            if !pin.metadata().map_err(io_error)?.is_file() {
                return Err(unsupported("object or pack entry is not regular"));
            }
            symlink(procfd(pin.as_raw_fd()), dst.join(&fname)).map_err(io_error)?;
            self.files.push(pin);
        }
        if pairs.values().any(|flags| *flags != 3) {
            return Err(unsupported("incomplete local pack/index pair"));
        }
        Ok(())
    }
}

/// The hidden worker entry point takes ownership only after validating the entire list.
pub(crate) fn adopt(fds: &[RawFd]) -> Result<Vec<File>> {
    if !(3..=3 + MAX_FILES).contains(&fds.len()) {
        return Err(unsupported("invalid layout descriptors"));
    }
    let mut seen = HashSet::new();
    for (i, &fd) in fds.iter().enumerate() {
        if fd <= 5 || !seen.insert(fd) || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(unsupported("invalid layout descriptor"));
        }
        // SAFETY: F_GETFD succeeded, the launcher handed the worker this fd, and this thread
        // does not close it during validation. The duplicate owns only its own descriptor.
        let check = File::from(
            unsafe { BorrowedFd::borrow_raw(fd) }
                .try_clone_to_owned()
                .map_err(io_error)?,
        );
        let md = check.metadata().map_err(io_error)?;
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0
            || flags & libc::O_ACCMODE != libc::O_RDONLY
            || (i < 3 && !md.is_dir())
            || (i >= 3 && (!md.is_file() || flags & libc::O_PATH == 0))
        {
            return Err(unsupported("wrong layout descriptor type or access mode"));
        }
    }
    // SAFETY: unique live descriptors placed by the launcher, now owned by the worker.
    Ok(fds
        .iter()
        .map(|&fd| unsafe { File::from_raw_fd(fd) })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::confined_git::{ConfinedGitReader, GitExecutable};
    use crate::ingest::deadline::Deadline;
    use std::process::Command;

    #[test]
    fn projected_object_reads_survive_relocation_and_live_alternate_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let source = base.join("source");
        let checkout = base.join("checkout");
        let scratch = base.join("scratch");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&scratch).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            let r = Command::new("/usr/bin/git")
                .arg("-C")
                .arg(cwd)
                .args([
                    "-c",
                    "user.name=fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env_clear()
                .output()
                .unwrap();
            assert!(r.status.success(), "fixture Git returned {}", r.status);
            String::from_utf8(r.stdout).unwrap().trim().to_owned()
        };
        git(&source, &["init", "-q", "-b", "main"]);
        fs::write(source.join("file"), "fixture bytes").unwrap();
        git(&source, &["add", "file"]);
        git(&source, &["commit", "-qm", "fixture"]);
        let oid = git(&source, &["rev-parse", "HEAD:file"]);
        git(
            &base,
            &[
                "clone",
                "-q",
                "--shared",
                source.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        fs::write(
            checkout.join(".git/mindspace-repository-id"),
            Uuid::now_v7().to_string(),
        )
        .unwrap();
        let root = File::open(&checkout).unwrap();
        let basefd = File::open(&base).unwrap();
        let (_, prepared) = prepare(
            &root,
            &checkout,
            Some(&File::open(&scratch).unwrap()),
            |p| open_relative(&basefd, p.strip_prefix(&base).unwrap()),
        )
        .unwrap();
        let mut prepared = prepared.unwrap();
        fs::rename(source.join(".git/objects"), base.join("moved")).unwrap();
        fs::write(
            checkout.join(".git/objects/info/alternates"),
            "../unavailable\n",
        )
        .unwrap();
        let exe = GitExecutable::resolve(Path::new("/usr/bin/git")).unwrap();
        let reader = ConfinedGitReader::from_layout(
            root.as_fd(),
            std::mem::take(&mut prepared.files),
            &exe,
            Deadline::none(),
        )
        .unwrap();
        assert_eq!(
            reader.blob_bytes(std::slice::from_ref(&oid)).unwrap(),
            vec![(oid, b"fixture bytes".to_vec())]
        );
        drop(reader);
        drop(prepared);
        assert_eq!(fs::read_dir(scratch).unwrap().count(), 0);
    }
}
