#!/usr/bin/env python3
"""Verify native Windows output and build release archives from a reviewed commit."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import struct
import subprocess
import tarfile
import tempfile
import zipfile
import zlib

ROOT = Path(__file__).resolve().parents[1]
VERSION = "0.1.0"
PREFIX = "cqlmatic-v" + VERSION + "/"
TARGET = "x86_64-pc-windows-msvc"
SYSTEM_DLLS = set("""advapi32 bcrypt bcryptprimitives cfgmgr32 combase crypt32 cryptbase
    dbghelp dnsapi gdi32 iphlpapi kernel32 kernelbase mswsock ncrypt normaliz nsi ntdll
    ole32 oleaut32 powrprof profapi psapi rpcrt4 sechost secur32 setupapi shell32 shlwapi
    ucrtbase user32 userenv version winhttp wininet winmm winspool wintrust ws2_32 wtsapi32""".split())


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def regular(path):
    require(path.is_file() and not path.is_symlink(), "Input must be a regular file")
    return path.read_bytes()


def safe_path(name):
    path = PurePosixPath(name)
    require(name and path.parts and not path.is_absolute() and path.as_posix() == name
            and not any(c in name for c in "\\:\x00\r\n\t")
            and all(part not in (".", "..") and not part.endswith((".", " "))
                    and not re.fullmatch(r"(?i)(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\..*)?", part)
                    for part in path.parts), "Unsafe source path")


def read_manifest(data):
    entries = {}
    for line in data.decode("utf-8").splitlines():
        digest, separator, name = line.partition("  ")
        require(separator and re.fullmatch(r"[0-9a-f]{64}", digest), "Invalid manifest digest")
        safe_path(name)
        require(name not in entries, "Duplicate manifest path")
        entries[name] = digest
    require(entries and len({name.casefold() for name in entries}) == len(entries),
            "Empty manifest or case-colliding source paths")
    folded = {name.casefold() for name in entries}
    require(not any(parent.as_posix().casefold() in folded for name in entries
                    for parent in PurePosixPath(name).parents), "File and directory paths collide")
    return entries


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT)


def committed_source(manifest):
    commit = git("rev-parse", "--verify", "HEAD").decode().strip()
    require(re.fullmatch(r"[0-9a-f]{40}", commit), "Invalid source commit")
    require(not git("status", "--porcelain", "--untracked-files=normal"), "Source tree must be clean and committed")
    staged = {}
    for record in git("ls-files", "--stage", "-z").decode("utf-8").split("\0")[:-1]:
        metadata, name = record.split("\t", 1)
        mode, _, stage = metadata.split()
        require(mode in ("100644", "100755") and stage == "0", "Nonregular or conflicted index entry")
        safe_path(name)
        staged[name] = int(mode, 8) & 0o777
    require(staged.keys() == manifest.keys(), "Manifest and index file sets differ")
    files = {}
    for name, digest in manifest.items():
        require(not any((ROOT / parent).is_symlink() for parent in PurePosixPath(name).parents),
                "Source directory is a symlink")
        data = git("show", "HEAD:" + name)
        require(sha256(data) == digest == sha256(regular(ROOT / name)), "Source changed: " + name)
        files[name] = data
    require("SOURCE-SHA256SUMS" in files and "LICENSE" in files and "NOTICE" in files,
            "Source checksums or license notices missing")
    require(b"Apache License" in files["LICENSE"] and files["NOTICE"].strip(), "License notices missing")
    require(read_manifest(files["SOURCE-SHA256SUMS"]) ==
            {name: digest for name, digest in manifest.items() if name != "SOURCE-SHA256SUMS"},
            "SOURCE-SHA256SUMS does not cover the complete index")
    return commit, files, staged


def public_bytes(data, label):
    prefixes = [str(ROOT), *(os.environ.get(key, "") for key in
                ("CARGO_HOME", "RUSTUP_HOME", "USERPROFILE", "HOME", "RUNNER_TEMP"))]
    lowered = data.lower()
    for prefix in filter(None, prefixes):
        for value in {prefix, prefix.replace("\\", "/"), prefix.replace("/", "\\")}:
            require(value.lower().encode() not in lowered
                    and value.lower().encode("utf-16-le") not in lowered,
                    "Private build path in " + label)
    strings = [match.group().decode("ascii") for match in re.finditer(rb"[\x20-\x7e]{7,}", data)]
    strings += [match.group().decode("utf-16-le") for match in re.finditer(rb"(?:[\x20-\x7e]\x00){7,}", data)]
    for value in strings:
        require(not re.search(r"(?i)[a-z]:[\\/][^\s\x00\"<>|]{3,}|/(?:Users|home|private|Volumes)/", value),
                "Absolute workstation path in " + label)


def pe_imports(data):
    def unpack(fmt, offset):
        require(0 <= offset <= len(data) - struct.calcsize(fmt), "Truncated PE image")
        return struct.unpack_from(fmt, data, offset)

    require(data[:2] == b"MZ", "Windows input is not a PE image")
    header, = unpack("<I", 0x3c)
    require(data[header:header + 4] == b"PE\0\0", "Invalid PE signature")
    machine, sections, _, _, _, optional_size, _ = unpack("<HHIIIHH", header + 4)
    optional = header + 24
    require(machine == 0x8664 and 1 <= sections <= 96 and optional_size >= 112,
            "Windows image must target x86_64 MSVC")
    require(unpack("<H", optional)[0] == 0x20b, "Windows image must be PE32+")
    directory_count, = unpack("<I", optional + 108)
    require(directory_count <= 16 and optional_size >= 112 + directory_count * 8, "Invalid PE directories")
    size_headers, = unpack("<I", optional + 60)
    mappings = []
    for index in range(sections):
        section = optional + optional_size + index * 40
        _, address, size, offset = unpack("<IIII", section + 8)
        require(offset + size <= len(data), "Truncated PE section")
        mappings.append((address, size, offset))

    def location(rva, size):
        if rva < size_headers and rva + size <= min(size_headers, len(data)):
            return rva
        for address, length, offset in mappings:
            if address <= rva and rva + size <= address + length:
                return offset + rva - address
        raise ValueError("PE address is outside file-backed sections")

    def directory(index):
        rva, size = unpack("<II", optional + 112 + index * 8) if index < directory_count else (0, 0)
        require(bool(rva) == bool(size), "Invalid PE directory")
        return (location(rva, size), size) if rva else (0, 0)

    def dll_name(rva):
        start = location(rva, 1)
        end = data.find(b"\0", start, min(start + 256, len(data)))
        require(end > start, "Invalid PE import name")
        location(rva, end - start + 1)
        name = data[start:end].decode("ascii").lower()
        require(name.endswith(".dll") and (name[:-4] in SYSTEM_DLLS or re.fullmatch(
            r"(?:api|ext)-ms-win-(?:[a-z0-9]+-)+l[0-9]+-[0-9]+-[0-9]+\.dll", name)),
            "Non-system Windows dependency")
        return name

    results = []
    for index, width, name_index in ((1, 20, 3), (13, 32, 1)):
        offset, size = directory(index)
        names, terminated = [], not size
        for position in range(offset, offset + size - width + 1, width):
            descriptor = unpack("<" + "I" * (width // 4), position)
            if not any(descriptor):
                terminated = True
                break
            require(index != 13 or descriptor[0] == 1, "Delay imports must use RVAs")
            names.append(dll_name(descriptor[name_index]))
        require(terminated, "Unterminated PE import table")
        results.append(sorted(set(names)))
    require(results[0], "Windows image has no imports")
    offset, size = directory(6)
    require(size % 28 == 0, "Invalid PE debug directory")
    for position in range(offset, offset + size, 28):
        require(unpack("<I", position + 12)[0] != 2, "CodeView/PDB records are not distributable")
    public_bytes(data, "Windows binary")
    return results


def check_system_dlls(names):
    import ctypes
    require(os.name == "nt", "Windows system-DLL resolution requires native Windows")
    system = (Path(os.environ["SystemRoot"]) / "System32").resolve()
    for name in names:
        library = ctypes.WinDLL(name, winmode=0x00000800)
        path = ctypes.create_unicode_buffer(32768)
        function = ctypes.windll.kernel32.GetModuleFileNameW
        function.argtypes = (ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_uint32)
        function.restype = ctypes.c_uint32
        require(function(library._handle, path, len(path)) and Path(path.value).resolve().parent == system,
                "Import did not resolve to a Windows system DLL")


def inspect_windows(args):
    data = regular(args.binary)
    imports, delayed = pe_imports(data)
    check_system_dlls(imports + delayed)
    commit = git("rev-parse", "--verify", "HEAD").decode().strip()
    require(commit == args.commit and re.fullmatch(r"[0-9a-f]{40}", commit), "Source commit differs")
    rustc = subprocess.check_output(["rustc", "+1.99.1", "--version", "--verbose"]).decode().strip()
    require(rustc.startswith("rustc 1.99.1 ") and "host: " + TARGET in rustc, "Unexpected Rust toolchain")
    public_bytes(regular(args.build_log), "build output")
    metadata = {"schema_version": 1, "product": "cqlmatic", "version": VERSION,
                "source_commit": commit, "target": TARGET, "toolchain": "1.99.1", "rustc": rustc,
                "runner": {"os": args.runner_os, "version": args.runner_version, "build": args.runner_build,
                           "image": args.runner_image, "image_version": args.runner_image_version},
                "binary": {"name": "cqlmatic.exe", "sha256": sha256(data),
                           "imports": imports, "delay_imports": delayed},
                "checks": {"native_tests": True, "synthetic_setup_inventory": True,
                           "system_dlls_resolved": True, "no_codeview": True, "public_paths": True}}
    raw = (json.dumps(metadata, indent=2, sort_keys=True) + "\n").encode()
    public_bytes(raw, "Windows metadata")
    args.metadata.write_bytes(raw)
    print(json.dumps({"binary_sha256": sha256(data), "imports": imports, "delay_imports": delayed}))


def windows_metadata(raw, commit, digest, imports):
    public_bytes(raw, "Windows metadata")
    value = json.loads(raw)
    require(set(value) == {"schema_version", "product", "version", "source_commit", "target", "toolchain",
                           "rustc", "runner", "binary", "checks"}, "Unexpected Windows metadata fields")
    require(value["schema_version"] == 1 and value["product"] == "cqlmatic" and value["version"] == VERSION
            and value["source_commit"] == commit and value["target"] == TARGET and value["toolchain"] == "1.99.1"
            and value["rustc"].startswith("rustc 1.99.1 ") and "host: " + TARGET in value["rustc"],
            "Windows metadata identity differs")
    require(set(value["runner"]) == {"os", "version", "build", "image", "image_version"}
            and all(isinstance(item, str) and item for item in value["runner"].values()), "Missing runner identity")
    require(value["binary"] == {"name": "cqlmatic.exe", "sha256": digest,
                                "imports": imports[0], "delay_imports": imports[1]}, "Windows metadata binary differs")
    require(value["checks"] == {"native_tests": True, "synthetic_setup_inventory": True,
                               "system_dlls_resolved": True, "no_codeview": True, "public_paths": True},
            "Windows validation gate is incomplete")


def write_zip(destination, files, modes):
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED, allowZip64=False) as archive:
        for name, data in sorted(files.items()):
            info = zipfile.ZipInfo(PREFIX + name, date_time=(1980, 1, 1, 0, 0, 0))
            info.create_system, info.external_attr = 3, (stat.S_IFREG | modes[name]) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, data)


def verify_zip(destination, files, modes):
    raw = destination.read_bytes()
    require(len(raw) >= 22 and raw[-22:-18] == b"PK\x05\x06" and raw[-2:] == b"\0\0", "Invalid ZIP trailer")
    with zipfile.ZipFile(destination) as archive:
        require(not archive.comment, "Unexpected ZIP comment")
        seen = set()
        for info in archive.infolist():
            name = info.filename.removeprefix(PREFIX)
            require(info.filename == PREFIX + name and name in files and name not in seen, "Unexpected ZIP member")
            require(not info.is_dir() and info.create_system == 3 and info.external_attr == (stat.S_IFREG | modes[name]) << 16
                    and info.date_time == (1980, 1, 1, 0, 0, 0) and not info.extra and not info.comment
                    and not info.flag_bits & 1 and info.compress_type == zipfile.ZIP_DEFLATED, "Unsafe ZIP metadata")
            require(archive.read(info) == files[name], "ZIP member digest differs")
            seen.add(name)
        require(seen == files.keys(), "ZIP is incomplete")


def write_tar(destination, files, modes):
    with destination.open("wb") as raw:
        with gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                for name, data in sorted(files.items()):
                    info = tarfile.TarInfo(PREFIX + name)
                    info.size, info.mode, info.mtime = len(data), modes[name], 0
                    archive.addfile(info, io.BytesIO(data))


def verify_tar(destination, files, modes):
    raw = destination.read_bytes()
    require(raw[:8] == b"\x1f\x8b\x08\x00\x00\x00\x00\x00", "Unexpected gzip metadata")
    decoder = zlib.decompressobj(16 + zlib.MAX_WBITS)
    unpacked = decoder.decompress(raw)
    require(decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail, "Invalid gzip stream")
    with tarfile.open(fileobj=io.BytesIO(unpacked), mode="r:") as archive:
        require(not archive.pax_headers, "Unexpected tar metadata")
        seen, end = set(), 0
        for info in archive:
            name = info.name.removeprefix(PREFIX)
            require(info.name == PREFIX + name and name in files and name not in seen, "Unexpected tar member")
            require(info.isfile() and not info.linkname and not info.pax_headers and info.uid == info.gid == 0
                    and not info.uname and not info.gname and info.mode == modes[name] and info.mtime == 0,
                    "Unsafe tar metadata")
            require(archive.extractfile(info).read() == files[name], "Tar member digest differs")
            end = info.offset_data + (info.size + 511) // 512 * 512
            seen.add(name)
        require(seen == files.keys() and len(unpacked) - end >= 1024 and not any(unpacked[end:]), "Tar is incomplete")


def build(args):
    manifest_raw = regular(args.manifest)
    commit, files, modes = committed_source(read_manifest(manifest_raw))
    linux, windows = regular(args.linux), regular(args.windows)
    require(linux[:6] == b"\x7fELF\x02\x01" and len(linux) >= 20 and struct.unpack_from("<H", linux, 18)[0] == 62,
            "Linux input must be an x86_64 ELF image")
    require(sha256(linux) == args.linux_sha256 and sha256(windows) == args.windows_sha256, "Native digest differs")
    imports = pe_imports(windows)
    metadata_raw = regular(args.windows_metadata)
    windows_metadata(metadata_raw, commit, args.windows_sha256, imports)
    require("cqlmatic.exe" not in files and "cqlmatic-linux-x86_64" not in files, "Native name collides with source")
    output = args.out.resolve()
    require(output != ROOT and ROOT not in output.parents, "Output must be outside the source tree")
    output.mkdir(parents=True, exist_ok=True)
    summary = {"source_commit": commit, "source_manifest_sha256": sha256(manifest_raw),
               "windows_metadata_sha256": sha256(metadata_raw), "archives": []}
    with tempfile.TemporaryDirectory(dir=output, prefix=".release-") as scratch:
        for suffix, binary_name, data, writer, verifier in (
                ("source.zip", None, None, write_zip, verify_zip),
                ("linux-x86_64.tar.gz", "cqlmatic-linux-x86_64", linux, write_tar, verify_tar),
                ("windows-x86_64.zip", "cqlmatic.exe", windows, write_zip, verify_zip)):
            members, member_modes = dict(files), dict(modes)
            if binary_name:
                members[binary_name], member_modes[binary_name] = data, 0o755
            name = "cqlmatic-v" + VERSION + "-" + suffix
            pending = Path(scratch) / name
            writer(pending, members, member_modes)
            verifier(pending, members, member_modes)
            summary["archives"].append({"file": name, "sha256": sha256(pending.read_bytes()), "members": len(members)})
        for archive in summary["archives"]:
            os.replace(Path(scratch) / archive["file"], output / archive["file"])
    (output / "RELEASE-BUILD.json").write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2, sort_keys=True))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    release = subparsers.add_parser("build", help="Build source, Linux, and Windows release archives")
    for option in ("manifest", "linux", "windows", "windows-metadata", "out"):
        release.add_argument("--" + option, type=Path, required=True)
    for option in ("linux-sha256", "windows-sha256"):
        release.add_argument("--" + option, required=True)
    release.set_defaults(run=build)
    windows = subparsers.add_parser("inspect-windows", help="Gate a native Windows build before uploading")
    for option in ("binary", "metadata", "build-log"):
        windows.add_argument("--" + option, type=Path, required=True)
    for option in ("commit", "runner-os", "runner-version", "runner-build", "runner-image", "runner-image-version"):
        windows.add_argument("--" + option, required=True)
    windows.set_defaults(run=inspect_windows)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
