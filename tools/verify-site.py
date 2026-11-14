#!/usr/bin/env python3
"""Verify an approved site artifact without executing its contents."""

import argparse
import hashlib
import io
import json
import os
import re
import stat
import sys
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from datetime import datetime, timezone
from pathlib import Path

REPOSITORY = "mychaelconnolly/cqlmatic"
HOSTS = {"landing": "https://cqlmatic.dev", "docs": "https://docs.cqlmatic.dev"}
# Retained approved artifacts with this contract must remain usable for rollback.
SITE_VERSIONS = ("0.1.4", "0.1.5", "0.1.6")
MAX_ARCHIVE = 100 * 1024 * 1024
MAX_EXPANDED = 200 * 1024 * 1024
MAX_FILE = 25 * 1024 * 1024
SHA = re.compile(r"[0-9a-f]{40}\Z")
DIGEST = re.compile(r"[0-9a-f]{64}\Z")


class InvalidSite(RuntimeError):
    pass


def require(condition, message):
    if not condition:
        raise InvalidSite(message)


def commit(value):
    require(isinstance(value, str) and SHA.fullmatch(value) and value != "0" * 40,
            "A full nonzero source commit is required.")
    return value


def positive_id(value):
    require(re.fullmatch(r"[1-9][0-9]{0,19}", str(value)) is not None,
            "A positive Actions identifier is required.")
    return int(value)


def json_object(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "Duplicate JSON keys are not allowed.")
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique)


def safe_path(value):
    parts = value.split("/")
    require(all(re.fullmatch(r"[A-Za-z0-9_@+.-]+", part) and not part.startswith(".")
                for part in parts), "An artifact path is unsafe.")
    forbidden = {"functions", "_worker.js", "_routes.json", "wrangler.toml", "wrangler.json", "wrangler.jsonc"}
    require(not any(part.lower() in forbidden for part in parts),
            "Executable deployment inputs are not allowed.")
    return parts


def check_site(files, source_sha, manifest_sha256):
    commit(source_sha)
    require(isinstance(manifest_sha256, str) and DIGEST.fullmatch(manifest_sha256),
            "A full site manifest SHA-256 is required.")
    require({name.split("/")[0] for name in files} ==
            {"landing", "docs", "BUILD.json", "SITE-SHA256SUMS"},
            "The site root does not match the approved layout.")
    require("landing/index.html" in files and "docs/index.html" in files,
            "Both site entry points are required.")
    manifest = files.get("SITE-SHA256SUMS", b"")
    require(hashlib.sha256(manifest).hexdigest() == manifest_sha256,
            "The approved site manifest hash does not match.")
    entries = {}
    for line in manifest.decode("utf-8").splitlines():
        require(re.fullmatch(r"[0-9a-f]{64}  [^\r\n]+", line), "The site manifest format is invalid.")
        digest, name = line.split("  ", 1)
        safe_path(name)
        require(name not in entries and name != "SITE-SHA256SUMS", "The site manifest contains duplicate entries.")
        entries[name] = digest
    require(list(entries) == sorted(entries), "The site manifest is not sorted.")
    require(set(entries) == set(files) - {"SITE-SHA256SUMS"}, "The site manifest file set does not match.")
    for name, digest in entries.items():
        require(hashlib.sha256(files[name]).hexdigest() == digest, "A site file digest does not match.")
    build = json_object(files["BUILD.json"])
    require(isinstance(build, dict) and set(build) == {"source_commit", "version", "documentation_pages", "hosts"} and
            build.get("source_commit") == source_sha and
            build.get("version") in SITE_VERSIONS and type(build.get("documentation_pages")) is int and
            build["documentation_pages"] == 29 and build.get("hosts") == HOSTS,
            "The site build identity does not match.")
    return entries


def read_archive(raw, source_sha, manifest_sha256):
    require(len(raw) <= MAX_ARCHIVE, "The site archive exceeds its size budget.")
    files, seen = {}, set()
    with zipfile.ZipFile(io.BytesIO(raw)) as archive:
        members = archive.infolist()
        require(len(members) <= 5000 and sum(info.file_size for info in members) <= MAX_EXPANDED,
                "The site archive exceeds its expansion budget.")
        for info in members:
            require(not info.flag_bits & 1, "Encrypted site archives are not allowed.")
            require(not info.is_dir() or info.file_size == 0, "Archive directories must not contain payloads.")
            require(info.orig_filename == info.filename, "An archive path contains a null byte.")
            name = info.filename[:-1] if info.is_dir() else info.filename
            safe_path(name)
            require(name.casefold() not in seen, "Duplicate archive paths are not allowed.")
            seen.add(name.casefold())
            kind = stat.S_IFMT(info.external_attr >> 16)
            require(kind in ((0, stat.S_IFDIR) if info.is_dir() else (0, stat.S_IFREG)),
                    "Only regular site files and directories are allowed.")
            require((info.is_dir() and name in ("site", "site/dist")) or name.startswith("site/dist/"),
                    "The artifact wrapper is invalid.")
            relative = name.removeprefix("site/dist/")
            if name in ("site", "site/dist"):
                continue
            root = relative.split("/")[0]
            require(root in ("landing", "docs") or
                    (not info.is_dir() and relative in ("BUILD.json", "SITE-SHA256SUMS")),
                    "The artifact contains an unexpected root entry.")
            if not info.is_dir():
                require(info.file_size <= MAX_FILE, "A site file exceeds its size budget.")
                files[relative] = archive.read(info)
        file_paths = {"site/dist/" + path.casefold() for path in files}
        for name in seen:
            parts = name.split("/")
            require(not any("/".join(parts[:end]) in file_paths
                            for end in range(1, len(parts))), "An archive file shadows a directory.")
    check_site(files, source_sha, manifest_sha256)
    return files


def check_directory(dist, source_sha, manifest_sha256):
    require(dist.is_dir() and not dist.is_symlink(), "The extracted site root is invalid.")
    require({path.name for path in dist.iterdir()} == {"landing", "docs", "BUILD.json", "SITE-SHA256SUMS"},
            "The extracted site root contains an unexpected entry.")
    files = {}
    total_size = 0
    for root, dirs, names in os.walk(dist, followlinks=False):
        for name in dirs + names:
            path = Path(root) / name
            relative = path.relative_to(dist).as_posix()
            safe_path(relative)
            require(not path.is_symlink(), "Site links are not allowed.")
            require(path.is_dir() or path.is_file(), "Only regular site files are allowed.")
            if path.is_file():
                size = path.stat().st_size
                total_size += size
                require(size <= MAX_FILE and total_size <= MAX_EXPANDED and len(files) < 5000,
                        "The extracted site exceeds its size budget.")
                files[relative] = path.read_bytes()
    check_site(files, source_sha, manifest_sha256)
    return files


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def read_url(url, headers=None, limit=2 * 1024 * 1024):
    request = urllib.request.Request(url, headers=headers or {})
    with urllib.request.build_opener(NoRedirect()).open(request, timeout=30) as response:
        raw = response.read(limit + 1)
        require(len(raw) <= limit, "An HTTP response exceeded its size budget.")
        return raw


class GitHub:
    def __init__(self, token):
        require(bool(token), "A read-only GitHub token is required.")
        self.headers = {"Authorization": "Bearer " + token, "Accept": "application/vnd.github+json",
                        "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "cqlmatic-site-verifier"}

    def get(self, path):
        return json_object(read_url("https://api.github.com/repos/" + REPOSITORY + path, self.headers))

    def download(self, artifact_id):
        url = f"https://api.github.com/repos/{REPOSITORY}/actions/artifacts/{artifact_id}/zip"
        for attempt in range(5):
            try:
                return read_url(url, self.headers if attempt == 0 else {}, MAX_ARCHIVE)
            except urllib.error.HTTPError as error:
                status = error.code
                location = error.headers.get("Location", "")
                error.close()
                require(status in (301, 302, 303, 307, 308), "The Actions artifact could not be downloaded.")
                parsed = urllib.parse.urlsplit(location)
                allowed = bool(re.fullmatch(r"productionresultssa[a-z0-9]*\.blob\.core\.windows\.net",
                                            parsed.hostname or "")) or (parsed.hostname or "").endswith(".actions.githubusercontent.com")
                require(parsed.scheme == "https" and allowed and parsed.port in (None, 443) and
                        parsed.username is None and parsed.password is None and not parsed.fragment,
                        "An Actions artifact redirect is outside approved storage.")
                url = location
        raise InvalidSite("The Actions artifact exceeded its redirect budget.")


def verify_provenance(api, source_sha, run_id, artifact_id, now=None):
    commit(source_sha)
    run_id, artifact_id = positive_id(run_id), positive_id(artifact_id)
    run = api.get(f"/actions/runs/{run_id}")
    workflow = api.get("/actions/workflows/ci.yml")
    positive_id(workflow.get("id"))
    positive_id(run.get("repository", {}).get("id"))
    positive_id(run.get("head_repository", {}).get("id"))
    require(run.get("id") == run_id and run.get("repository", {}).get("full_name") == REPOSITORY and
            run.get("head_repository", {}).get("full_name") == REPOSITORY and
            run.get("path") == ".github/workflows/ci.yml" and
            workflow.get("path") == ".github/workflows/ci.yml" and workflow.get("state") == "active" and
            run.get("workflow_id") == workflow.get("id") and run.get("name") == "CI" and
            run.get("event") == "push" and run.get("head_branch") == "main" and
            run.get("head_sha") == source_sha and run.get("status") == "completed" and
            run.get("conclusion") == "success", "The artifact requires a successful main push of the exact CI workflow.")
    artifact = api.get(f"/actions/artifacts/{artifact_id}")
    binding = artifact.get("workflow_run", {})
    require(artifact.get("id") == artifact_id and artifact.get("name") == "site-" + source_sha and
            artifact.get("expired") is False and binding.get("id") == run_id and
            binding.get("head_sha") == source_sha and binding.get("head_branch") == "main" and
            binding.get("repository_id") == run.get("repository", {}).get("id") and
            binding.get("head_repository_id") == run.get("head_repository", {}).get("id"),
            "The Actions artifact does not belong to the approved CI run.")
    timestamp = lambda value: datetime.fromisoformat(value.replace("Z", "+00:00"))
    require(timestamp(artifact["expires_at"]) > (now or datetime.now(timezone.utc)) and
            timestamp(run["run_started_at"]) <= timestamp(artifact["created_at"]) <= timestamp(run["updated_at"]),
            "The Actions artifact is expired or belongs to an earlier run attempt.")
    require(isinstance(artifact.get("digest"), str) and
            re.fullmatch(r"sha256:[0-9a-f]{64}", artifact["digest"]), "The Actions artifact digest is unavailable.")
    main = api.get("/branches/main")["commit"]["sha"]
    commit(main)
    comparison = api.get(f"/compare/{source_sha}...{main}")
    require(comparison.get("status") in ("ahead", "identical") and
            comparison.get("merge_base_commit", {}).get("sha") == source_sha,
            "The approved source commit is not reachable from main.")
    return artifact


def extract_archive(raw, out, source_sha, manifest_sha256):
    files = read_archive(raw, source_sha, manifest_sha256)
    out.mkdir(parents=True, exist_ok=False)
    for name, content in files.items():
        path = out / "site" / "dist" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("prepare", "extract"))
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--run-id")
    parser.add_argument("--artifact-id")
    parser.add_argument("--archive", type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if args.operation == "extract":
        require(args.archive is not None, "An approved site archive is required.")
        extract_archive(args.archive.read_bytes(), args.out, args.source_sha, args.manifest_sha256)
    else:
        api = GitHub(os.environ.get("GITHUB_TOKEN"))
        artifact = verify_provenance(api, args.source_sha, args.run_id, args.artifact_id)
        raw = api.download(positive_id(args.artifact_id))
        require("sha256:" + hashlib.sha256(raw).hexdigest() == artifact["digest"],
                "The Actions artifact archive digest does not match.")
        read_archive(raw, args.source_sha, args.manifest_sha256)
        args.out.mkdir(parents=True, exist_ok=False)
        (args.out / "site.zip").write_bytes(raw)
        proof = {"repository": REPOSITORY, "source_commit": args.source_sha,
                 "ci_run_id": positive_id(args.run_id), "site_artifact_id": positive_id(args.artifact_id),
                 "site_manifest_sha256": args.manifest_sha256, "archive_sha256": artifact["digest"].split(":")[1]}
        (args.out / "provenance.json").write_text(json.dumps(proof, sort_keys=True) + "\n", encoding="utf-8")
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary:
            with open(summary, "a", encoding="utf-8") as output:
                output.write("Verified site awaiting production approval\n\n```json\n" +
                             json.dumps(proof, sort_keys=True, indent=2) + "\n```\n")
    print("Approved site artifact verified.")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, TypeError, AttributeError, zipfile.BadZipFile) as error:
        print(str(error) if isinstance(error, InvalidSite) else "Site artifact verification failed.", file=sys.stderr)
        sys.exit(1)
