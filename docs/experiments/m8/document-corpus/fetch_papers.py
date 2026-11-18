"""Fetch the two public evaluation papers and check their published-file revisions."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
from tempfile import TemporaryDirectory


def fetch(case, directory):
    target = directory / case["file"]
    if not target.exists():
        with TemporaryDirectory(dir=directory) as scratch:
            download = Path(scratch) / "download.pdf"
            subprocess.run(["curl", "--fail", "--silent", "--show-error", "--location",
                            "--proto", "=https", "--proto-redir", "=https", "--retry", "2",
                            "--max-time", "60", "--max-filesize", "5242880",
                            "--output", str(download), case["publisher_pdf"]], check=True)
            if hashlib.sha256(download.read_bytes()).hexdigest() != case["sha256"]:
                raise ValueError(f"publisher file differs from the expected revision: {target.name}")
            try:
                os.link(download, target)
            except FileExistsError:
                pass  # Another fetch may have finished first. Check its bytes below.
    if hashlib.sha256(target.read_bytes()).hexdigest() != case["sha256"]:
        raise ValueError(f"existing file differs from the expected revision: {target.name}")
    print(f"Verified {target.name}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path, help="local cache for the verified PDFs")
    destination = parser.parse_args().directory
    destination.mkdir(parents=True, exist_ok=True)
    cases = json.loads(Path(__file__).with_name("gold.json").read_text(encoding="utf-8"))
    try:
        for name in ("paper", "stroop"):
            fetch(cases[name], destination)
    except (OSError, ValueError, subprocess.CalledProcessError) as exc:
        parser.exit(1, f"{exc}\n")
