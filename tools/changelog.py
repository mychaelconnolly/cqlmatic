#!/usr/bin/env python3
"""Keep unreleased changelog entries in one file per change.

Two branches that both edit the top of CHANGELOG.md cannot both merge. So an
unreleased entry is its own file, changelog.d/NAME.md, holding one or more
lines that each start with "- ". CHANGELOG.md holds released versions only and
changes in one place: `release` moves every entry, word for word, into a new
section for the version in Cargo.toml and records the file's SHA-256 in
changelog.d/released.sha256. The entries become one list, ordered by file
name, with blank lines dropped.

    python3 tools/changelog.py            check (the check command runs this)
    python3 tools/changelog.py release    fold the entries into CHANGELOG.md

To correct released text, edit CHANGELOG.md and write its new SHA-256 to
changelog.d/released.sha256 in the same commit.
"""
import hashlib
import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
TITLE = "# Changelog\n\n"
ENTRY = "Put the entry in its own file, changelog.d/NAME.md, as lines that start with \"- \"."


def version(repo):
    return tomllib.loads((repo / "Cargo.toml").read_text(encoding="utf-8"))["package"]["version"]


def entries(repo):
    return sorted((repo / "changelog.d").glob("*.md"))


def digest(repo):
    return hashlib.sha256((repo / "CHANGELOG.md").read_bytes()).hexdigest()


def content_findings(repo):
    """Problems with the released text or an entry file, whatever the version is."""
    found = []
    if (repo / "changelog.d" / "released.sha256").read_text(encoding="utf-8").split()[:1] != [digest(repo)]:
        found.append(f"CHANGELOG.md differs from the released text recorded in changelog.d/released.sha256. "
                     f"Unreleased entries do not go in CHANGELOG.md. {ENTRY}")
    for path in sorted((repo / "changelog.d").iterdir()):
        if path.name == "released.sha256":
            continue
        if path.suffix != ".md":
            found.append(f"changelog.d/{path.name}: name an entry file NAME.md, or it is left out of the release.")
            continue
        lines = [line for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
        if not lines or not all(line.startswith("- ") and line[2:].strip() for line in lines):
            found.append(f"changelog.d/{path.name}: every line must be an entry that starts with \"- \".")
    return found


def findings(repo):
    text = (repo / "CHANGELOG.md").read_text(encoding="utf-8")
    first = re.search(r"^## (.+)$", text, re.M).group(1)
    if first != version(repo):
        return [f"CHANGELOG.md starts with section \"{first}\" but Cargo.toml is at {version(repo)}. "
                f"Only `python3 tools/changelog.py release` adds a section, in the commit that sets a new version. {ENTRY}"]
    return content_findings(repo)


def release(repo):
    text = (repo / "CHANGELOG.md").read_text(encoding="utf-8")
    current = version(repo)
    paths = entries(repo)
    if not text.startswith(TITLE) or f"## {current}\n" in text or not paths:
        raise SystemExit(f"Cannot release {current}: set the new version in Cargo.toml first, "
                         "and keep at least one entry in changelog.d/.")
    found = content_findings(repo)
    if found:
        raise SystemExit(f"Cannot release {current}:\n" + "\n".join(found))
    lines = [line for path in paths for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]
    section = f"## {current}\n\n" + "\n".join(lines) + "\n\n"
    (repo / "CHANGELOG.md").write_text(TITLE + section + text[len(TITLE):], encoding="utf-8", newline="\n")
    (repo / "changelog.d" / "released.sha256").write_text(digest(repo) + "  CHANGELOG.md\n", encoding="utf-8", newline="\n")
    for path in paths:
        path.unlink()


def main():
    if sys.argv[1:] == ["release"]:
        release(REPO)
    elif sys.argv[1:]:
        raise SystemExit(__doc__)
    found = findings(REPO)
    for finding in found:
        print(finding, file=sys.stderr)
    sys.exit(1 if found else 0)


if __name__ == "__main__":
    main()
