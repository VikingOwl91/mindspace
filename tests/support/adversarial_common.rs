//! Private infrastructure for the two S3e.3b opt-in targets only.
#![allow(dead_code)]

use mindspace::core::sha256_hex;
use serde_json::{json, Value};
use std::cell::Cell;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

pub const WAIT: Duration = Duration::from_secs(45);

pub fn old() -> PathBuf {
    required_path("MINDSPACE_ADVERSARIAL_OLD")
}
pub fn current() -> PathBuf {
    required_path("MINDSPACE_ADVERSARIAL_CURRENT")
}
pub fn driver() -> PathBuf {
    required_path("MINDSPACE_ADVERSARIAL_DRIVER")
}

fn required_path(key: &str) -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os(key)
            .unwrap_or_else(|| panic!("run scripts/m8-s3e3b-harness.py: missing {key}")),
    );
    assert!(path.is_absolute(), "{key} must be absolute");
    path
}

pub struct Case {
    pub db: PathBuf,
    pub work: PathBuf,
    pub artifacts: PathBuf,
    temporary: Option<tempfile::TempDir>,
    complete: Cell<bool>,
}

impl Case {
    pub fn new(name: &str) -> Self {
        assert!(name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        let root = required_path("MINDSPACE_ADVERSARIAL_ROOT");
        let output = required_path("MINDSPACE_ADVERSARIAL_ARTIFACTS");
        fs::create_dir_all(&root).unwrap();
        let temporary = tempfile::Builder::new()
            .prefix(&format!("{name}-"))
            .tempdir_in(&root)
            .unwrap();
        let work = temporary.path().to_path_buf();
        let artifacts = output.join(name);
        fs::create_dir_all(&artifacts).unwrap();
        assert!(
            !artifacts.join("result.json").exists(),
            "refuse contaminated case output"
        );
        for dir in ["home", "config", "data", "cache", "tmp"] {
            fs::create_dir(work.join(dir)).unwrap();
        }
        let case = Self {
            db: work.join("mind.db"),
            work,
            artifacts,
            temporary: Some(temporary),
            complete: Cell::new(false),
        };
        case.save("environment", &json!({"sqlite_version":rusqlite::version(),"sqlite_version_number":rusqlite::version_number(),"fixture_root":case.work,"atime_caveat":"hashing reads may update atime; stat fields are captured before hash reads"}));
        case
    }

    pub fn command(&self, binary: &Path) -> Command {
        let mut c = Command::new(binary);
        c.env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.work.join("home"))
            .env("XDG_CONFIG_HOME", self.work.join("config"))
            .env("XDG_DATA_HOME", self.work.join("data"))
            .env("XDG_CACHE_HOME", self.work.join("cache"))
            .env("TMPDIR", self.work.join("tmp"))
            .env("LANG", "C.UTF-8")
            .current_dir(&self.work);
        c
    }

    pub fn mind(&self, binary: &Path) -> Command {
        let mut c = self.command(binary);
        c.arg("--db").arg(&self.db);
        c
    }

    pub fn save(&self, label: &str, value: &Value) {
        assert!(label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'));
        fs::write(
            self.artifacts.join(format!("{label}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }

    pub fn fingerprint(&self) -> Value {
        family(&self.db)
    }

    pub fn finish(&self, category: &str, oracle: &str, details: Value) {
        assert!(["regression_invariant", "known_unsafe_characterization"].contains(&category));
        self.save("result", &json!({"status":"PASS", "category":category, "oracle":oracle, "observations":details,"meaning":"PASS confirms this oracle only; a reproduced unsafe behavior is not fixed"}));
        self.complete.set(true);
    }

    pub fn skip(&self, reason: &str) {
        self.save("result", &json!({"status":"SKIPPED", "reason":reason}));
        self.complete.set(true);
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if !self.complete.get() || thread::panicking() {
            // Preserve the owned fixture, never copy/read any external database.
            if let Some(dir) = self.temporary.take() {
                let kept = dir.keep();
                let value = json!({"status":"FAIL","fixture_preserved":kept,"reason":"scenario did not complete its oracle; see test stdout/stderr"});
                let _ = fs::write(
                    self.artifacts.join("result.json"),
                    serde_json::to_vec_pretty(&value).unwrap(),
                );
            }
        }
    }
}

pub fn family(db: &Path) -> Value {
    let mut result = serde_json::Map::new();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        let path = PathBuf::from(name);
        let value = match fs::symlink_metadata(&path) {
            Ok(m) => {
                json!({"exists":true,"dev":m.dev(),"ino":m.ino(),"size":m.len(),"mode":m.mode(),"uid":m.uid(),"gid":m.gid(),"mtime":[m.mtime(),m.mtime_nsec()],"ctime":[m.ctime(),m.ctime_nsec()],"atime_before_hash":[m.atime(),m.atime_nsec()],"sha256":sha256_hex(&fs::read(&path).unwrap())})
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"exists":false}),
            Err(e) => panic!("stat {}: {e}", path.display()),
        };
        result.insert(
            if suffix.is_empty() { "main" } else { suffix }.into(),
            value,
        );
    }
    Value::Object(result)
}

fn terminate(child: &mut Child) {
    // Never signal a PID/group identity after its child has already been reaped.
    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }
    // All children are started in a process group owned exclusively by this helper.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(2)),
            Ok(None) => break,
        }
    }
}

pub fn run(mut command: Command, label: &str, case: &Case) -> Output {
    let stdout = case.artifacts.join(format!("{label}.stdout"));
    let stderr = case.artifacts.join(format!("{label}.stderr"));
    command
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).unwrap())
        .stderr(File::create(&stderr).unwrap())
        .process_group(0);
    let child = command.spawn().unwrap();
    let mut guard = OwnedChild(child);
    let deadline = Instant::now() + WAIT;
    let status = loop {
        if let Some(status) = guard.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "command {label} timed out; see {}",
            case.artifacts.display()
        );
        thread::sleep(Duration::from_millis(2));
    };
    Output {
        status,
        stdout: fs::read(stdout).unwrap(),
        stderr: fs::read(stderr).unwrap(),
    }
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        terminate(&mut self.0);
    }
}

pub fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "status {:?}; stdout {}; stderr {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

pub struct ChildGuard {
    pub child: Child,
    input: Option<ChildStdin>,
    lines: Receiver<String>,
    reader: Option<thread::JoinHandle<()>>,
}

impl ChildGuard {
    pub fn spawn(mut command: Command, stderr: PathBuf) -> Self {
        let stdout_log = stderr.with_extension("stdout");
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(File::create(stderr).unwrap())
            .process_group(0);
        let mut child = command.spawn().unwrap();
        let input = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut log = File::create(stdout_log).unwrap();
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let _ = writeln!(log, "{line}");
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            lines,
            reader: Some(reader),
        }
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn send(&mut self, line: &str) {
        let input = self.input.as_mut().expect("child stdin");
        writeln!(input, "{line}").unwrap();
        input.flush().unwrap();
    }
    pub fn line(&self) -> String {
        self.lines
            .recv_timeout(WAIT)
            .expect("bounded child stdout wait; inspect artifacts")
    }
    pub fn try_line(&self, timeout: Duration) -> Option<String> {
        match self.lines.recv_timeout(timeout) {
            Ok(s) => Some(s),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(e) => panic!("child stdout closed: {e}"),
        }
    }
    pub fn wait(&mut self) -> ExitStatus {
        self.input.take();
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "bounded child exit wait");
            thread::sleep(Duration::from_millis(2));
        }
    }
    pub fn kill(&mut self) {
        terminate(&mut self.child);
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
        if let Some(reader) = self.reader.take() {
            // With the owned process group gone, its stdout writer ends. Avoid
            // a blocking join if an unexpected unrelated descriptor was inherited.
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}
