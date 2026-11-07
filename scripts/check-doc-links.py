#!/usr/bin/env python3
"""Check relative Markdown links and in-page anchors (no network). Exit 1 on any broken link.

  scripts/check-doc-links.py            # README.md, SECURITY.md, CONTRIBUTING.md, docs/**/*.md
"""
import glob, os, re, sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
files = [f for f in ("README.md", "SECURITY.md", "CONTRIBUTING.md") if os.path.exists(os.path.join(root, f))]
files += sorted(os.path.relpath(p, root) for p in glob.glob(os.path.join(root, "docs", "**", "*.md"), recursive=True))


def slug(h):
    h = re.sub(r"[`*_]", "", h.strip().lower())
    h = re.sub(r"[^\w\- ]", "", h)
    return h.replace(" ", "-")


def anchors(path):
    out, in_code = set(), False
    for line in open(path, encoding="utf-8"):
        if line.startswith("```"):
            in_code = not in_code
        m = None if in_code else re.match(r"#{1,6}\s+(.*)", line)
        if m:
            out.add(slug(m.group(1)))
    return out


bad = []
for f in files:
    path = os.path.join(root, f)
    text = re.sub(r"```.*?```", "", open(path, encoding="utf-8").read(), flags=re.S)
    text = re.sub(r"`[^`\n]*`", "", text)
    for target in re.findall(r"\]\(([^)\s]+)\)", text):
        if re.match(r"[a-z]+:", target):
            continue
        base, _, frag = target.partition("#")
        dest = path if not base else os.path.normpath(os.path.join(os.path.dirname(path), base))
        if not os.path.exists(dest):
            bad.append(f"{f}: missing target {target}")
        elif frag and dest.endswith(".md") and frag not in anchors(dest):
            bad.append(f"{f}: missing anchor #{frag} in {os.path.relpath(dest, root)}")
print(f"checked {len(files)} files")
for b in bad:
    print("BROKEN", b)
sys.exit(1 if bad else 0)
