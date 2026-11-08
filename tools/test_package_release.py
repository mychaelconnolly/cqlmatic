"""Fixture-free release membership, provenance, and tamper checks."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
import zipfile

SPEC = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)


def pe_image(dependency="kernel32.dll", delayed=None, codeview=False):
    image = bytearray(0xa00)
    image[:2] = b"MZ"
    struct.pack_into("<I", image, 0x3c, 0x80)
    image[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", image, 0x84, 0x8664, 1, 0, 0, 0, 240, 0)
    struct.pack_into("<H", image, 0x98, 0x20b)
    struct.pack_into("<I", image, 0x98 + 60, 0x200)
    struct.pack_into("<I", image, 0x98 + 108, 16)
    struct.pack_into("<IIII", image, 0x188 + 8, 0x800, 0x1000, 0x800, 0x200)
    struct.pack_into("<II", image, 0x98 + 120, 0x1000, 40)
    struct.pack_into("<I", image, 0x200 + 12, 0x1080)
    image[0x280:0x280 + len(dependency) + 1] = dependency.encode() + b"\0"
    if delayed:
        struct.pack_into("<II", image, 0x98 + 112 + 13 * 8, 0x1100, 64)
        struct.pack_into("<II", image, 0x300, 1, 0x1180)
        image[0x380:0x380 + len(delayed) + 1] = delayed.encode() + b"\0"
    if codeview:
        struct.pack_into("<II", image, 0x98 + 112 + 6 * 8, 0x1200, 28)
        struct.pack_into("<I", image, 0x400 + 12, 2)
    return bytes(image)


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="cqlmatic-release-test-")
        self.base = Path(self.temporary.name)
        self.repo = self.base / "repo"
        self.repo.mkdir()
        self.old_root, RELEASE.ROOT = RELEASE.ROOT, self.repo
        self.git("init", "--quiet", "--initial-branch=main")
        self.files = {"README.md": b"Synthetic release fixture.\n", "LICENSE": b"Apache License\nVersion 2.0\n",
                      "NOTICE": b"Synthetic fixture attribution.\n", "dev": b"#!/bin/sh\nexit 0\n"}
        self.files["SOURCE-SHA256SUMS"] = self.manifest(self.files)
        for name, data in self.files.items():
            (self.repo / name).write_bytes(data)
        (self.repo / "dev").chmod(0o755)
        self.git("add", "--", *self.files)
        self.git("-c", "user.name=Release Fixture", "-c", "user.email=release@example.invalid", "commit", "--quiet", "-m", "Synthetic release fixture")
        self.commit = self.git("rev-parse", "HEAD").decode().strip()
        self.manifest_path = self.base / "manifest.sha256"
        self.manifest_path.write_bytes(self.manifest(self.files))
        self.linux = self.base / "linux"
        elf = bytearray(64)
        elf[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<H", elf, 18, 62)
        self.linux.write_bytes(elf)
        self.windows = self.base / "windows.exe"
        self.windows.write_bytes(pe_image(delayed="api-ms-win-core-synch-l1-2-0.dll"))
        self.metadata = self.base / "BUILD-WINDOWS.json"
        self.meta = {"schema_version": 1, "product": "cqlmatic", "version": "0.1.0", "source_commit": self.commit,
                     "target": RELEASE.TARGET, "toolchain": "1.99.1",
                     "rustc": "rustc 1.99.1 (synthetic)\nhost: " + RELEASE.TARGET,
                     "runner": {"os": "Microsoft Windows Server", "version": "10.0.26100", "build": "26100",
                                "image": "win25", "image_version": "20261101.1"},
                     "binary": {"name": "cqlmatic.exe", "sha256": RELEASE.sha256(self.windows.read_bytes()),
                                "imports": ["kernel32.dll"], "delay_imports": ["api-ms-win-core-synch-l1-2-0.dll"]},
                     "checks": {"native_tests": True, "synthetic_setup_inventory": True,
                                "system_dlls_resolved": True, "no_codeview": True, "public_paths": True}}
        self.metadata.write_text(json.dumps(self.meta), encoding="utf-8")
        self.args = SimpleNamespace(manifest=self.manifest_path, linux=self.linux, windows=self.windows,
                                    linux_sha256=RELEASE.sha256(self.linux.read_bytes()),
                                    windows_sha256=RELEASE.sha256(self.windows.read_bytes()),
                                    windows_metadata=self.metadata, out=self.base / "out")

    def tearDown(self):
        RELEASE.ROOT = self.old_root
        self.temporary.cleanup()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, stderr=subprocess.PIPE)

    @staticmethod
    def manifest(files):
        return "".join(RELEASE.sha256(data) + "  " + name + "\n" for name, data in sorted(files.items())).encode()

    def build(self):
        with contextlib.redirect_stdout(io.StringIO()):
            RELEASE.build(self.args)

    def test_reproducible_archives_have_exact_sources_binaries_modes_and_notices(self):
        self.build()
        first = {path.name: path.read_bytes() for path in self.args.out.iterdir()}
        self.args.out = self.base / "second"
        self.build()
        self.assertEqual(first, {path.name: path.read_bytes() for path in self.args.out.iterdir()})
        for suffix, binary in (("source.zip", None), ("windows-x86_64.zip", "cqlmatic.exe")):
            with zipfile.ZipFile(self.args.out / ("cqlmatic-v0.1.0-" + suffix)) as archive:
                expected = {RELEASE.PREFIX + name for name in self.files}
                if binary:
                    expected.add(RELEASE.PREFIX + binary)
                    self.assertEqual(archive.read(RELEASE.PREFIX + binary), self.windows.read_bytes())
                self.assertEqual(set(archive.namelist()), expected)
                for name, data in self.files.items():
                    self.assertEqual(archive.read(RELEASE.PREFIX + name), data)
        with tarfile.open(self.args.out / "cqlmatic-v0.1.0-linux-x86_64.tar.gz") as archive:
            native = archive.getmember(RELEASE.PREFIX + "cqlmatic-linux-x86_64")
            self.assertEqual(native.mode, 0o755)
            self.assertEqual(archive.extractfile(native).read(), self.linux.read_bytes())
            self.assertEqual(set(archive.getnames()), {RELEASE.PREFIX + name for name in self.files} | {native.name})
        summary = json.loads(first["RELEASE-BUILD.json"])
        self.assertEqual(summary["source_commit"], self.commit)
        self.assertNotIn(str(self.base), first["RELEASE-BUILD.json"].decode())

    def test_worktree_manifest_and_source_checksum_tampering_are_rejected(self):
        (self.repo / "README.md").write_bytes(b"Changed source\n")
        with self.assertRaisesRegex(ValueError, "clean and committed"):
            self.build()
        (self.repo / "README.md").write_bytes(self.files["README.md"])
        original = self.manifest_path.read_bytes()
        self.manifest_path.write_bytes(original.replace(b"README.md", b"MISSING.md"))
        with self.assertRaisesRegex(ValueError, "file sets differ"):
            self.build()
        self.manifest_path.write_bytes(original)
        (self.repo / "unexpected.txt").write_text("Unreviewed", encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "clean and committed"):
            self.build()
        (self.repo / "unexpected.txt").unlink()
        self.files["SOURCE-SHA256SUMS"] = self.manifest({"LICENSE": self.files["LICENSE"]})
        (self.repo / "SOURCE-SHA256SUMS").write_bytes(self.files["SOURCE-SHA256SUMS"])
        self.git("add", "--", "SOURCE-SHA256SUMS")
        self.git("-c", "user.name=Release Fixture", "-c", "user.email=release@example.invalid", "commit", "--quiet", "-m", "Synthetic incomplete checksums")
        self.manifest_path.write_bytes(self.manifest(self.files))
        with self.assertRaisesRegex(ValueError, "complete index"):
            self.build()

    def test_changed_native_provenance_and_nonregular_inputs_are_rejected(self):
        self.windows.write_bytes(self.windows.read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "Native digest differs"):
            self.build()
        self.windows.write_bytes(pe_image(delayed="api-ms-win-core-synch-l1-2-0.dll"))
        self.meta["source_commit"] = "0" * 40
        self.metadata.write_text(json.dumps(self.meta), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "identity differs"):
            self.build()
        self.meta["source_commit"] = self.commit
        self.meta["checks"]["native_tests"] = False
        self.metadata.write_text(json.dumps(self.meta), encoding="utf-8")
        with self.assertRaisesRegex(ValueError, "gate is incomplete"):
            self.build()
        self.linux.unlink()
        self.linux.symlink_to(self.windows)
        with self.assertRaisesRegex(ValueError, "regular file"):
            self.build()

    def test_import_delay_codeview_and_path_gates_reject_unsafe_images(self):
        self.assertEqual(RELEASE.pe_imports(pe_image(delayed="ucrtbase.dll")), [["kernel32.dll"], ["ucrtbase.dll"]])
        for data, message in ((pe_image("vcruntime140.dll"), "Non-system"),
                              (pe_image(delayed="msvcp140.dll"), "Non-system"),
                              (pe_image(codeview=True), "CodeView"),
                              (pe_image() + b"C:\\Users\\synthetic\\build\\main.rs", "workstation path"),
                              (pe_image() + "/home/synthetic/build/main.rs".encode("utf-16-le"), "workstation path")):
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                RELEASE.pe_imports(data)
        truncated = bytearray(pe_image())
        struct.pack_into("<II", truncated, 0x98 + 120, 0x1000, 20)
        with self.assertRaisesRegex(ValueError, "Unterminated"):
            RELEASE.pe_imports(truncated)

    def test_unsafe_manifest_and_tampered_archive_members_are_rejected(self):
        for name in (".", "../escape", "/absolute", "dir\\file", "dir/../file", "CON.txt", "trailing."):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "Unsafe"):
                RELEASE.read_manifest(("0" * 64 + "  " + name + "\n").encode())
        self.build()
        archive = self.args.out / "cqlmatic-v0.1.0-source.zip"
        modes = {name: 0o755 if name == "dev" else 0o644 for name in self.files}
        with zipfile.ZipFile(archive, "a") as output:
            output.writestr(RELEASE.PREFIX + "unexpected.txt", b"Unexpected")
        with self.assertRaisesRegex(ValueError, "Unexpected ZIP member"):
            RELEASE.verify_zip(archive, self.files, modes)
        archive = self.args.out / "cqlmatic-v0.1.0-linux-x86_64.tar.gz"
        archive.write_bytes(archive.read_bytes() + b"unexpected gzip tail")
        with self.assertRaisesRegex(ValueError, "Invalid gzip stream"):
            RELEASE.verify_tar(archive, {}, {})


if __name__ == "__main__":
    unittest.main()
