#!/usr/bin/env python3
"""Build and verify the npm tarball from a frozen, staged source allowlist."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tempfile
import zlib

ROOT = Path(__file__).resolve().parents[1]


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def read_manifest(data):
    entries = {}
    for line in data.decode("utf-8").splitlines():
        digest, separator, name = line.partition("  ")
        path = PurePosixPath(name)
        require(separator and re.fullmatch(r"[0-9a-f]{64}", digest), "Invalid manifest digest")
        require(name and not path.is_absolute() and ".." not in path.parts
                and path.as_posix() == name and "\\" not in name and "\x00" not in name,
                "Unsafe manifest path")
        require(name not in entries, "Duplicate manifest path")
        entries[name] = digest
    require(entries, "Empty manifest")
    return entries


def staged_source(manifest):
    raw = subprocess.check_output(["git", "ls-files", "--stage", "-z"], cwd=ROOT)
    staged = {}
    for record in raw.decode("utf-8").split("\0")[:-1]:
        metadata, name = record.split("\t", 1)
        mode, _, stage = metadata.split()
        require(mode in ("100644", "100755") and stage == "0", "Nonregular or conflicted index entry")
        staged[name] = int(mode, 8) & 0o777
    require(staged.keys() == manifest.keys(), "Manifest and index file sets differ")
    require(not subprocess.check_output(["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT),
            "Unreviewed source files are present")
    subprocess.run(["git", "diff", "--quiet"], cwd=ROOT, check=True)
    files = {}
    for name, digest in manifest.items():
        source = ROOT / name
        require(source.is_file() and not source.is_symlink(), "Nonregular source: " + name)
        data = subprocess.check_output(["git", "show", ":" + name], cwd=ROOT)
        require(sha256(data) == digest == sha256(source.read_bytes()), "Source changed: " + name)
        files[name] = data
    source_sums = read_manifest(files["SOURCE-SHA256SUMS"])
    require(source_sums == {name: digest for name, digest in manifest.items() if name != "SOURCE-SHA256SUMS"},
            "SOURCE-SHA256SUMS does not cover the complete index")
    return files, staged


def build(args):
    manifest = read_manifest(args.manifest.read_bytes())
    files, modes = staged_source(manifest)
    metadata = json.loads(files["package.json"])
    native = json.loads(files["npm/native-manifest.json"])
    require(metadata["name"] == "cqlmatic" and metadata["version"] == native["version"] == "0.1.1",
            "Unexpected npm package identity")
    require(metadata["bin"] == {"cqlmatic": "npm/cqlmatic.cjs"}, "Unexpected launcher")
    selected = {"package.json", "README.md", "LICENSE", "NOTICE"}
    for entry in metadata["files"]:
        require(isinstance(entry, str) and entry and not any(c in entry for c in "*?[]\\"),
                "Package files must be exact paths or directory prefixes")
        path = PurePosixPath(entry.rstrip("/"))
        require(not path.is_absolute() and ".." not in path.parts and path.as_posix() == entry.rstrip("/"),
                "Unsafe package file entry")
        matches = {name for name in files if name == entry or name.startswith(entry.rstrip("/") + "/")}
        require(matches or entry.rstrip("/") == "native", "Package entry matches no source: " + entry)
        selected.update(matches)
    packed = {name: files[name] for name in selected}
    package_modes = {name: modes[name] for name in selected}
    require(set(native["binaries"]) == {"linux-x64", "darwin-arm64"}, "Unexpected native platform set")
    for platform, binary, magic in (("linux-x64", args.linux, b"\x7fELF"),
                                    ("darwin-arm64", args.macos, b"\xcf\xfa\xed\xfe")):
        require(binary.is_file() and not binary.is_symlink(), "Native binary must be a regular file")
        data = binary.read_bytes()
        require(data.startswith(magic) and sha256(data) == native["binaries"][platform]["sha256"],
                "Native binary differs from the reviewed manifest: " + platform)
        name = "native/" + platform + "/cqlmatic"
        packed[name], package_modes[name] = data, 0o755
    package_modes["npm/cqlmatic.cjs"] = 0o755
    packed["NPM-SHA256SUMS"] = "".join(sha256(data) + "  " + name + "\n"
                                              for name, data in sorted(packed.items())).encode()
    package_modes["NPM-SHA256SUMS"] = 0o644
    output = args.out.resolve()
    require(output != ROOT and ROOT not in output.parents, "Output must be outside the source tree")
    output.mkdir(parents=True, exist_ok=True)
    destination = output / "cqlmatic-0.1.1.tgz"
    with tempfile.NamedTemporaryFile(dir=output, prefix=".cqlmatic-", delete=False) as temporary:
        pending = Path(temporary.name)
    try:
        with pending.open("wb") as raw:
            with gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                    for name, data in sorted(packed.items()):
                        info = tarfile.TarInfo("package/" + name)
                        info.size, info.mode, info.mtime = len(data), package_modes[name], 0
                        archive.addfile(info, io.BytesIO(data))
        decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
        decoder.decompress(pending.read_bytes())
        require(decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail, "Invalid gzip stream")
        with tempfile.TemporaryDirectory(dir=output, prefix=".verify-npm-") as scratch:
            with tarfile.open(pending, "r:gz") as archive:
                require(not archive.pax_headers, "Unexpected tar metadata")
                seen = set()
                for info in archive:
                    name = info.name.removeprefix("package/")
                    require(info.name == "package/" + name and name in packed and name not in seen,
                            "Missing, duplicate or unexpected archive member")
                    require(info.isfile() and not info.linkname and not info.pax_headers
                            and info.uid == info.gid == 0 and not info.uname and not info.gname
                            and info.mode == package_modes[name], "Unsafe archive member")
                    data = archive.extractfile(info).read()
                    require(data == packed[name], "Stale archive member: " + name)
                    target = Path(scratch) / name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(data)
                    seen.add(name)
                require(seen == packed.keys(), "Archive is incomplete")
            extracted = {p.relative_to(scratch).as_posix(): sha256(p.read_bytes())
                         for p in Path(scratch).rglob("*") if p.is_file()}
            require(extracted == {name: sha256(data) for name, data in packed.items()}, "Extracted digests differ")
        os.replace(pending, destination)
    finally:
        pending.unlink(missing_ok=True)
    print(json.dumps({"file": str(destination), "sha256": sha256(destination.read_bytes()),
                      "source_manifest_sha256": sha256(args.manifest.read_bytes()), "members": len(packed)}, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--linux", type=Path, required=True)
    parser.add_argument("--macos", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    build(parser.parse_args())
