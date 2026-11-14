"""Check npm package membership, native privacy, and install instructions."""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
from types import SimpleNamespace
import unittest

SPEC = importlib.util.spec_from_file_location("package_npm", Path(__file__).with_name("package-npm.py"))
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)


class NpmBuildTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="cqlmatic-npm-test-")
        self.base = Path(self.temporary.name)
        self.repo = self.base / "repo"
        self.repo.mkdir()
        self.old_root, PACKAGE.ROOT = PACKAGE.ROOT, self.repo
        self.old_release_root, PACKAGE.RELEASE.ROOT = PACKAGE.RELEASE.ROOT, self.repo
        self.git("init", "--quiet", "--initial-branch=main")
        self.linux, self.macos = self.base / "linux", self.base / "macos"
        self.linux.write_bytes(b"\x7fELF\0synthetic Linux image")
        self.macos.write_bytes(b"\xcf\xfa\xed\xfe\0synthetic macOS image")
        self.native = {"version": "0.0.1", "binaries": {
            "linux-x64": {"sha256": PACKAGE.sha256(self.linux.read_bytes())},
            "darwin-arm64": {"sha256": PACKAGE.sha256(self.macos.read_bytes())}}}
        self.files = {
            "package.json": json.dumps({"name": "cqlmatic", "version": "0.0.1",
                                        "bin": {"cqlmatic": "npm/cqlmatic.cjs"},
                                        "files": ["npm/", "native/"]}).encode(),
            "README.md": b"npx cqlmatic@0.0.1 setup\n",
            "LICENSE": b"Apache License\nVersion 2.0\n",
            "NOTICE": b"Synthetic fixture attribution.\n",
            "npm/cqlmatic.cjs": b"#!/usr/bin/env node\nprocess.exit(0);\n"}
        self.manifest_path = self.base / "manifest.sha256"
        self.stage_source()
        self.args = SimpleNamespace(manifest=self.manifest_path, linux=self.linux,
                                    macos=self.macos, out=self.base / "out")

    def tearDown(self):
        PACKAGE.ROOT = self.old_root
        PACKAGE.RELEASE.ROOT = self.old_release_root
        self.temporary.cleanup()

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo, stderr=subprocess.PIPE)

    @staticmethod
    def manifest(files):
        return "".join(PACKAGE.sha256(data) + "  " + name + "\n"
                       for name, data in sorted(files.items())).encode()

    def stage_source(self):
        self.files["npm/native-manifest.json"] = json.dumps(self.native).encode()
        self.files["SOURCE-SHA256SUMS"] = self.manifest(
            {name: data for name, data in self.files.items() if name != "SOURCE-SHA256SUMS"})
        for name, data in self.files.items():
            target = self.repo / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
        self.git("add", "--", *self.files)
        self.manifest_path.write_bytes(self.manifest(self.files))

    def build(self):
        with contextlib.redirect_stdout(io.StringIO()):
            PACKAGE.build(self.args)

    def test_reproducible_archive_has_exact_sources_and_natives(self):
        self.build()
        first = (self.args.out / "cqlmatic-0.0.1.tgz").read_bytes()
        self.args.out = self.base / "second"
        self.build()
        self.assertEqual(first, (self.args.out / "cqlmatic-0.0.1.tgz").read_bytes())
        expected = {name: data for name, data in self.files.items() if name != "SOURCE-SHA256SUMS"}
        expected.update({"native/linux-x64/cqlmatic": self.linux.read_bytes(),
                         "native/darwin-arm64/cqlmatic": self.macos.read_bytes()})
        expected["NPM-SHA256SUMS"] = self.manifest(expected)
        with tarfile.open(self.args.out / "cqlmatic-0.0.1.tgz") as archive:
            self.assertEqual({info.name: archive.extractfile(info).read() for info in archive},
                             {"package/" + name: data for name, data in expected.items()})

    def test_private_native_paths_with_reviewed_hashes_are_rejected_before_archiving(self):
        for platform, binary in (("linux-x64", self.linux), ("darwin-arm64", self.macos)):
            clean = binary.read_bytes()
            for encoding in ("utf-8", "utf-16-le"):
                with self.subTest(platform=platform, encoding=encoding):
                    binary.write_bytes(clean + b"\0" + "/home/synthetic/build/main.rs".encode(encoding))
                    self.native["binaries"][platform]["sha256"] = PACKAGE.sha256(binary.read_bytes())
                    self.stage_source()
                    self.args.out = self.base / ("out-" + platform + "-" + encoding)
                    with self.assertRaisesRegex(ValueError, "workstation path"):
                        self.build()
                    self.assertFalse(self.args.out.exists())
            binary.write_bytes(clean)
            self.native["binaries"][platform]["sha256"] = PACKAGE.sha256(clean)


class NpmGuideTests(unittest.TestCase):
    def test_current_install_command_is_required_without_stale_commands(self):
        PACKAGE.install_guide(b"npx cqlmatic@0.1.4 setup\n", "0.1.4")
        with self.assertRaisesRegex(ValueError, "Current npm install instructions missing"):
            PACKAGE.install_guide(b"npx cqlmatic@0.1.1 setup\n", "0.1.4")
        with self.assertRaisesRegex(ValueError, "Stale npm install instructions"):
            PACKAGE.install_guide(b"npx cqlmatic@0.1.4 setup\nnpx cqlmatic@0.1.1 check\n", "0.1.4")


if __name__ == "__main__":
    unittest.main()
