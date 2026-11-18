//! Profile a first Git ingest in fresh temporary databases. The fixture is generated
//! locally and contains only synthetic documents and directories.
use std::cell::Cell;
use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use clap::Parser;
use mindspace::ingest::deadline::Deadline;
use mindspace::ingest::extract::{persist_git_gated, Extracted, StoreContext};
use mindspace::ingest::extract_path_with_deadline;
use mindspace::storage::Store;
use serde_json::json;

#[derive(Parser)]
struct Args {
    #[arg(long, default_value_t = 35794)]
    directories: usize,
    #[arg(long, default_value_t = 256)]
    documents: usize,
    #[arg(long, default_value_t = 8)]
    sections: usize,
    #[arg(long, default_value_t = 3)]
    runs: usize,
}

fn git(repo: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "0");
    cmd
}

fn fixture(repo: &Path, args: &Args) -> Result<(), Box<dyn Error>> {
    std::fs::create_dir(repo)?;
    if !git(repo)
        .args(["init", "-q", "-b", "main", "--object-format=sha1"])
        .status()?
        .success()
    {
        return Err("git init failed".into());
    }
    let mut import = git(repo)
        .args(["fast-import", "--quiet"])
        .stdin(Stdio::piped())
        .spawn()?;
    let mut stream = std::io::BufWriter::new(import.stdin.take().unwrap());
    stream.write_all(b"blob\nmark :1\ndata 1\nx\ncommit refs/heads/main\ncommitter fixture <bench@example.invalid> 1230000000 +0000\ndata 5\nbench\n")?;
    for i in 0..args.directories {
        writeln!(
            stream,
            "M 100644 :1 shard-{:02}/part-{i:05}/payload-{i:05}.bin",
            i / 1000
        )?;
    }
    for i in 0..args.documents {
        let mut content = format!("# Document {i:04}\n\n");
        for j in 0..args.sections {
            content.push_str(&format!("## Section {j:02}\n\nThe calibration record for unit {i:04} lists observation {j:02} and its reference value.\n\n"));
        }
        writeln!(
            stream,
            "M 100644 inline docs/document-{i:04}.md\ndata {}\n{content}",
            content.len()
        )?;
    }
    stream.write_all(b"\ndone\n")?;
    stream.flush()?;
    drop(stream);
    if !import.wait()?.success() {
        return Err("git fast-import failed".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if args.runs == 0 || (args.directories == 0 && args.documents == 0) || args.sections == 0 {
        return Err("use at least one run, one input and one document section".into());
    }
    let temp = tempfile::tempdir()?;
    let repo = temp.path().join("repo");
    fixture(&repo, &args)?;
    for run in 1..=args.runs {
        let state = tempfile::tempdir_in(temp.path())?;
        let db = state.path().join("mind.db");
        mindspace::initialize(&db)?;
        let store = mindspace::open_existing(&db)?;
        store.create_space("bench", "")?;
        let deadline = Deadline::none();
        let started = Instant::now();
        let Extracted::Git(input) =
            extract_path_with_deadline(&StoreContext(&store), &repo, &["bench"], &deadline)?
        else {
            return Err("fixture was not recognized as Git".into());
        };
        let prepared = Instant::now();
        let will_commit = Cell::new(prepared);
        let report = persist_git_gated(&store, input, &deadline, &|| {
            will_commit.set(Instant::now());
            Ok(())
        })?;
        let finished = Instant::now();
        drop(store);
        let conn =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let count = |table| {
            conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
        };
        let memories = count("memories")?;
        if memories as usize != args.documents * args.sections {
            return Err(format!("unexpected memory count {memories}").into());
        }
        println!(
            "{}",
            json!({
                "run":run,"documents":args.documents,"memories":memories,
                "trees":count("git_trees")?,"tree_entries":count("git_tree_entries")?,
                "objects":report.objects_discovered,"database_bytes":std::fs::metadata(db)?.len(),
                "extract_ms":(prepared-started).as_millis(),
                "write_ms":(will_commit.get()-prepared).as_millis(),
                "commit_ms":(finished-will_commit.get()).as_millis(),
                "total_ms":(finished-started).as_millis()
            })
        );
    }
    Ok(())
}
