"""Fixture-free release membership, provenance, and tamper checks."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
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
        version = RELEASE.VERSION
        guide = (f"npx cqlmatic@{version} setup\n"
                 f"cqlmatic-v{version}-windows-x86_64.zip\n"
                 f"https://github.com/example/cqlmatic/releases/tag/v{version}\n").encode()
        self.files = {"README.md": guide, "docs/SETUP.md": guide,
                      "Cargo.toml": f'[package]\nname = "cqlmatic"\nversion = "{version}"\n'.encode(),
                      "Cargo.lock": f'[[package]]\nname = "cqlmatic"\nversion = "{version}"\n'.encode(),
                      "package.json": json.dumps({"name": "cqlmatic", "version": version}).encode(),
                      "npm/native-manifest.json": json.dumps({"version": version}).encode(),
                      "LICENSE": b"Apache License\nVersion 2.0\n",
                      "NOTICE": b"Synthetic fixture attribution.\n", "dev": b"#!/bin/sh\nexit 0\n"}
        self.files["SOURCE-SHA256SUMS"] = self.manifest(self.files)
        for name, data in self.files.items():
            (self.repo / name).parent.mkdir(parents=True, exist_ok=True)
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
        self.meta = {"schema_version": 1, "product": "cqlmatic", "version": RELEASE.VERSION, "source_commit": self.commit,
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
            with zipfile.ZipFile(self.args.out / ("cqlmatic-v" + RELEASE.VERSION + "-" + suffix)) as archive:
                expected = {RELEASE.PREFIX + name for name in self.files}
                if binary:
                    expected.add(RELEASE.PREFIX + binary)
                    self.assertEqual(archive.read(RELEASE.PREFIX + binary), self.windows.read_bytes())
                self.assertEqual(set(archive.namelist()), expected)
                for name, data in self.files.items():
                    self.assertEqual(archive.read(RELEASE.PREFIX + name), data)
        with tarfile.open(self.args.out / ("cqlmatic-v" + RELEASE.VERSION + "-linux-x86_64.tar.gz")) as archive:
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

    def test_extracted_archive_rejects_stale_install_guide_and_metadata(self):
        files = dict(self.files)
        old = b"0.0.1"
        files["docs/SETUP.md"] = files["docs/SETUP.md"].replace(RELEASE.VERSION.encode(), old)
        modes = {name: 0o755 if name == "dev" else 0o644 for name in files}
        archive = self.base / "stale.zip"
        RELEASE.write_zip(archive, files, modes)
        with self.assertRaisesRegex(ValueError, "Current install instructions missing"):
            RELEASE.verify_zip(archive, files, modes)
        files = dict(self.files)
        files["npm/native-manifest.json"] = b'{"version":"0.0.1"}'
        with self.assertRaisesRegex(ValueError, "metadata versions differ"):
            RELEASE.release_metadata(files)

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

    def test_public_urls_pass_but_absolute_and_embedded_private_paths_fail(self):
        public = ('https://example.com/reference', 'http://example.com/reference',
                  'ftp://example.com/reference', 'git+https://example.com/reference',
                  'References: https://example.com/reference and mailto:public@example.com')
        paths = ('C:/synthetic/build/main.rs', r'C:\synthetic\build\main.rs',
                 'C://synthetic/build/main.rs', 'file:///C:/synthetic/build/main.rs',
                 'embeddedC:/synthetic/build/main.rs', 'embeddedC://synthetic/build/main.rs',
                 r'embeddedC:\synthetic\build\main.rs',
                 'https://example.com/?path=C:/synthetic/build/main.rs',
                 '/home/synthetic/build/main.rs')
        for encoding in ('ascii', 'utf-16-le'):
            for value in public:
                with self.subTest(encoding=encoding, kind='public URL'):
                    RELEASE.public_bytes(value.encode(encoding), 'synthetic public URL')
            for value in paths:
                with self.subTest(encoding=encoding, kind='absolute path'), self.assertRaisesRegex(ValueError, 'workstation path'):
                    RELEASE.public_bytes(value.encode(encoding), 'synthetic absolute path')
            with patch.dict(os.environ, {'CARGO_HOME': 'synthetic-private-cache'}):
                for value in ('embeddedSYNTHETIC-PRIVATE-CACHE/artifact',
                              'https://example.com/?path=synthetic-private-cache/artifact'):
                    with self.subTest(encoding=encoding, kind='private prefix'), self.assertRaisesRegex(ValueError, 'Private build path'):
                        RELEASE.public_bytes(value.encode(encoding), 'synthetic private prefix')

    def test_only_exact_public_literals_are_normalized_without_hiding_adjacent_paths(self):
        neutral = ('/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.229/src/private/de.rs',
                   '/cargo-registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.229/src/private/de.rs')
        literals = ('git+https://', 'https://', 'http://', 'ftp://', 'file://', 'radr://5614542',
                    'called `Result::unwrap()` on an `Err` value:', *neutral)
        for encoding in ('ascii', 'utf-16-le'):
            for literal in literals:
                with self.subTest(encoding=encoding, kind='exact literal'):
                    RELEASE.public_bytes(('merged' + literal + 'public').encode(encoding), 'synthetic literal')
                for path in ('C:/tmp', r'embeddedC:\synthetic\file', 'embeddedC://synthetic/file', '/private/synthetic/file'):
                    for value in (path + literal, literal + path):
                        with self.subTest(encoding=encoding, kind='adjacent path'), self.assertRaisesRegex(ValueError, 'workstation path'):
                            RELEASE.public_bytes(value.encode(encoding), 'synthetic adjacent path')
                with patch.dict(os.environ, {'CARGO_HOME': literal}):
                    with self.subTest(encoding=encoding, kind='unconditional private prefix'), self.assertRaisesRegex(ValueError, 'Private build path'):
                        RELEASE.public_bytes(literal.encode(encoding), 'synthetic known prefix')
            for literal in neutral:
                for value in ('C:' + literal, '/private' + literal):
                    with self.subTest(encoding=encoding, kind='neutral path prefix'), self.assertRaisesRegex(ValueError, 'workstation path'):
                        RELEASE.public_bytes(value.encode(encoding), 'synthetic prefixed path')
            for value in ('radr://5614543', 'radr://56145420',
                          neutral[0].replace('1.0.229', '1.0.230'),
                          'called `Result::expect()` on an `Err` value:/assertion'):
                with self.subTest(encoding=encoding, kind='unrecognized literal'), self.assertRaisesRegex(ValueError, 'workstation path'):
                    RELEASE.public_bytes(value.encode(encoding), 'synthetic unrecognized literal')
            for scheme in ('git+https://', 'https://', 'http://', 'ftp://', 'file://'):
                for remainder in ('example.com/?path=C:/synthetic/file', '/private/synthetic/file'):
                    with self.subTest(encoding=encoding, kind='URI remainder'), self.assertRaisesRegex(ValueError, 'workstation path'):
                        RELEASE.public_bytes((scheme + remainder).encode(encoding), 'synthetic URI remainder')

    def test_only_complete_embedded_knowledge_is_decoded_and_adjacent_paths_survive(self):
        knowledge = self.repo / 'data/knowledge.json'
        knowledge.parent.mkdir()
        raw = json.dumps([{'body': 'Syntax:\n\n```\ncount()\n```'}]).encode()
        knowledge.write_bytes(raw)
        RELEASE.public_bytes(b'prefix' + raw + b'suffix' + raw, 'synthetic embedded knowledge')
        RELEASE.public_bytes(b'C:' + raw + b'/tmp', 'synthetic nonadjacent path')
        for encoding in ('ascii', 'utf-16-le'):
            path = 'C:/tmp'.encode(encoding)
            for value in (path + raw, raw + path):
                with self.subTest(encoding=encoding, kind='short adjacent path'), self.assertRaisesRegex(ValueError, 'workstation path'):
                    RELEASE.public_bytes(value, 'synthetic adjacent path')
        other = json.dumps([{'body': 'Example:\n\n```\ncount()\n```'}]).encode()
        with self.assertRaisesRegex(ValueError, 'workstation path'):
            RELEASE.public_bytes(other, 'synthetic different JSON')

    def test_decoded_knowledge_checks_duplicate_keys_values_and_escaped_private_prefixes(self):
        knowledge = self.repo / 'data/knowledge.json'
        knowledge.parent.mkdir()
        for raw in (b'{"body":"C:\\\\synthetic\\\\file","body":"public"}',
                    b'{"C:\\\\synthetic\\\\file":"public","body":"public"}',
                    b'{"nested":[{"body":"C\\u003a/synthetic/file"}]}'):
            knowledge.write_bytes(raw)
            with self.subTest(kind='decoded private path'), self.assertRaisesRegex(ValueError, 'workstation path'):
                RELEASE.public_bytes(raw, 'synthetic embedded knowledge')
        private = r'Z:\synthetic-private-cache'
        raw = json.dumps({'nested': [{'body': private}]}).encode()
        knowledge.write_bytes(raw)
        with patch.dict(os.environ, {'CARGO_HOME': private}):
            with self.assertRaisesRegex(ValueError, 'Private build path'):
                RELEASE.public_bytes(raw, 'synthetic escaped private prefix')

    def test_malformed_matching_knowledge_fails_before_serialized_bytes_are_excluded(self):
        knowledge = self.repo / 'data/knowledge.json'
        knowledge.parent.mkdir()
        for raw in (b'', b'{"body":', b'{"body":NaN}', b'\xff'):
            knowledge.write_bytes(raw)
            with self.subTest(kind='malformed exact knowledge'), self.assertRaises(ValueError):
                RELEASE.public_bytes(b'prefix' + raw + b'suffix', 'synthetic malformed knowledge')

    def test_unsafe_manifest_and_tampered_archive_members_are_rejected(self):
        for name in (".", "../escape", "/absolute", "dir\\file", "dir/../file", "CON.txt", "trailing."):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "Unsafe"):
                RELEASE.read_manifest(("0" * 64 + "  " + name + "\n").encode())
        self.build()
        archive = self.args.out / ("cqlmatic-v" + RELEASE.VERSION + "-source.zip")
        modes = {name: 0o755 if name == "dev" else 0o644 for name in self.files}
        with zipfile.ZipFile(archive, "a") as output:
            output.writestr(RELEASE.PREFIX + "unexpected.txt", b"Unexpected")
        with self.assertRaisesRegex(ValueError, "Unexpected ZIP member"):
            RELEASE.verify_zip(archive, self.files, modes)
        archive = self.args.out / ("cqlmatic-v" + RELEASE.VERSION + "-linux-x86_64.tar.gz")
        archive.write_bytes(archive.read_bytes() + b"unexpected gzip tail")
        with self.assertRaisesRegex(ValueError, "Invalid gzip stream"):
            RELEASE.verify_tar(archive, {}, {})


if __name__ == "__main__":
    unittest.main()
