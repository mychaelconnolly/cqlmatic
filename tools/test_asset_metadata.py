#!/usr/bin/env python3
"""Tracked images and binary assets carry no embedded metadata."""

import importlib.util
import struct
import subprocess
import tempfile
import unittest
import zlib
from pathlib import Path


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


assets = load("asset_metadata")


def chunk(kind, body=b""):
    return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))


def png(*extra, tail=b""):
    header = chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 0, 0, 0, 0))
    pixels = chunk(b"IDAT", zlib.compress(b"\x00\x00"))
    return assets.PNG_SIGNATURE + header + b"".join(extra) + pixels + chunk(b"IEND") + tail


class AssetMetadataTests(unittest.TestCase):
    def test_display_chunks_pass(self):
        self.assertEqual(assets.png_findings(png()), [])
        self.assertEqual(assets.png_findings(png(chunk(b"pHYs", b"\x00" * 9), chunk(b"sRGB", b"\x00"))), [])

    def test_metadata_chunks_are_refused_by_name(self):
        for kind in (b"tEXt", b"zTXt", b"iTXt", b"eXIf", b"caBX", b"iCCP", b"tIME"):
            with self.subTest(kind=kind):
                findings = assets.png_findings(png(chunk(kind, b"generator")))
                self.assertEqual(len(findings), 1, findings)
                self.assertIn(kind.decode(), findings[0])

    def test_spare_bytes_inside_allowed_chunks_are_refused(self):
        signature, header = assets.PNG_SIGNATURE, chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 0, 0, 0, 0))
        pixels = zlib.compress(b"\x00\x00")
        cases = {
            "nonempty IEND": signature + header + chunk(b"IDAT", pixels) + chunk(b"IEND", b"private provenance"),
            "oversized pHYs": png(chunk(b"pHYs", b"\x00" * 9 + b"note")),
            "bytes after the compressed image": signature + header + chunk(b"IDAT", pixels + b"note") + chunk(b"IEND"),
            "a second data chunk of notes": signature + header + chunk(b"IDAT", pixels) + chunk(b"IDAT", b"note") + chunk(b"IEND"),
        }
        for name, data in cases.items():
            with self.subTest(name=name):
                self.assertTrue(assets.png_findings(data))
        split = signature + header + chunk(b"IDAT", pixels[:3]) + chunk(b"IDAT", pixels[3:]) + chunk(b"IEND")
        self.assertEqual(assets.png_findings(split), [])
        # More image data than one decoding step holds.
        large = signature + header + chunk(b"IDAT", zlib.compress(bytes(1 << 20))) + chunk(b"IEND")
        self.assertEqual(assets.png_findings(large), [])
        self.assertTrue(assets.png_findings(large.replace(chunk(b"IEND"), chunk(b"IDAT", b"note") + chunk(b"IEND"))))

    def test_damaged_or_extended_pngs_are_refused(self):
        self.assertTrue(assets.png_findings(png(tail=b"hidden")))
        self.assertTrue(assets.png_findings(png()[:-6]))
        self.assertTrue(assets.png_findings(png()[:-12]))
        damaged = bytearray(png())
        damaged[-1] ^= 1
        self.assertTrue(assets.png_findings(bytes(damaged)))

    def test_svg_metadata_and_editor_namespaces_are_refused(self):
        clean = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><path d="M0 0h1v1z"/></svg>'
        self.assertEqual(assets.file_findings("icon.svg", clean.encode()), [])
        for addition in ("<metadata>x</metadata>", "<!-- made with a tool -->", '<rdf:RDF xmlns:rdf="urn:x"/>',
                         '<s:metadata xmlns:s="http://www.w3.org/2000/svg">provenance</s:metadata>',
                         '<e:layer xmlns:e="urn:editor"/>', '<g xmlns:e="urn:editor" e:label="layer"/>',
                         '<g data-name="layer"/>', "<?editor version 1?>",
                         '<g xmlns:x="http://www.w3.org/1999/xlink" x:data-note="layer"/>'):
            with self.subTest(addition=addition):
                changed = clean.replace("<path", addition + "<path")
                self.assertTrue(assets.file_findings("icon.svg", changed.encode()))
        for document in ("<svg", '<!DOCTYPE svg [<!ENTITY note "x">]>' + clean):
            with self.subTest(document=document[:20]):
                self.assertTrue(assets.file_findings("icon.svg", document.encode()))
        linked = clean.replace("<path", '<use xmlns:x="http://www.w3.org/1999/xlink" x:href="#a"/><path')
        self.assertEqual(assets.file_findings("icon.svg", linked.encode()), [])
        quoted = clean.replace("<path", "<text><![CDATA[<?php echo 1; ?> <!-- x -->]]></text><path")
        self.assertEqual(assets.file_findings("icon.svg", quoted.encode()), [])

    def test_text_passes_and_unknown_binary_types_are_refused(self):
        self.assertEqual(assets.file_findings("notes.md", "Exif and XMP are named here.\n".encode()), [])
        self.assertEqual(assets.file_findings("picture.png", png()), [])
        for name, data in (("photo.jpg", b"\xff\xd8\xff\xe1\x00\x10Exif\x00\x00"), ("blob.bin", b"\x00\x01\x02")):
            with self.subTest(name=name):
                self.assertTrue(assets.file_findings(name, data))

    def test_findings_cover_tracked_files_only(self):
        with tempfile.TemporaryDirectory() as folder:
            run = lambda *args: subprocess.run(["git", *args], cwd=folder, check=True, capture_output=True)
            run("init", "-q")
            Path(folder, "clean.png").write_bytes(png())
            Path(folder, "tagged.png").write_bytes(png(chunk(b"tEXt", b"Software\x00tool")))
            Path(folder, "untracked.png").write_bytes(png(chunk(b"tEXt", b"Software\x00tool")))
            run("add", "clean.png", "tagged.png")
            self.assertEqual(list(assets.findings(folder)), ["tagged.png"])

    def test_this_repository_has_no_embedded_asset_metadata(self):
        if not (assets.REPO / ".git").exists():
            self.skipTest("not a Git checkout")
        self.assertEqual(assets.findings(), {})


if __name__ == "__main__":
    unittest.main()
