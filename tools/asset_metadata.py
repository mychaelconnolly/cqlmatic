#!/usr/bin/env python3
"""Fail when a tracked image or binary asset carries embedded metadata.

Images can hold data that is not needed to display them: provenance manifests,
Exif and XMP blocks, text chunks, colour-profile descriptions and editor notes.
Every tracked file that is not plain UTF-8 text must be a PNG made only of the
chunks listed below. Tracked SVG files must not hold metadata elements or
editor namespaces. Any other binary type fails until a check for it exists.

Run from anywhere: python3 tools/asset_metadata.py
"""
import re
import struct
import subprocess
import sys
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
# Chunks that decide how the picture is drawn. Text, time, Exif, colour-profile
# and private chunks (tEXt, zTXt, iTXt, tIME, eXIf, iCCP, caBX, ...) are refused.
PNG_CHUNKS = {b"IHDR", b"PLTE", b"IDAT", b"IEND", b"tRNS", b"gAMA", b"cHRM", b"sRGB", b"sBIT", b"bKGD", b"pHYs"}
SVG_MARKERS = re.compile(
    r"<metadata\b|<\?xpacket|<x:xmpmeta|<rdf:RDF|xmlns:(?:rdf|dc|cc|xmp|inkscape|sodipodi|sketch|figma)\b|<!--",
    re.IGNORECASE,
)


def png_findings(data):
    """Reasons a PNG is refused; empty when it holds display chunks only."""
    if not data.startswith(PNG_SIGNATURE):
        return ["not a PNG file"]
    findings, offset, ended = [], len(PNG_SIGNATURE), False
    while offset < len(data):
        if ended:
            findings.append(f"{len(data) - offset} bytes after the end of the image")
            break
        if offset + 12 > len(data):
            findings.append("truncated chunk")
            break
        length, kind = struct.unpack(">I4s", data[offset:offset + 8])
        end = offset + 12 + length
        if end > len(data):
            findings.append("truncated chunk")
            break
        body = data[offset + 8:end - 4]
        if zlib.crc32(kind + body) != struct.unpack(">I", data[end - 4:end])[0]:
            findings.append(f"chunk {kind!r} fails its checksum")
        if kind not in PNG_CHUNKS:
            findings.append(f"chunk {kind.decode('latin-1')} ({length} bytes) is not needed to display the image")
        ended = kind == b"IEND"
        offset = end
    if not ended and not findings:
        findings.append("no end-of-image chunk")
    return findings


def svg_findings(text):
    """Reasons an SVG is refused; empty when it holds no metadata markers."""
    return sorted({f"contains {match.group(0)!r}" for match in SVG_MARKERS.finditer(text)})


def file_findings(name, data):
    """Reasons one tracked file is refused."""
    try:
        text = None if b"\x00" in data else data.decode("utf-8")
    except UnicodeDecodeError:
        text = None
    if text is not None:
        return svg_findings(text) if name.lower().endswith(".svg") else []
    if data.startswith(PNG_SIGNATURE):
        return png_findings(data)
    return ["binary file of a type with no metadata check; use PNG or add a check for this type"]


def tracked_files(repo=REPO):
    listing = subprocess.run(["git", "ls-files", "-z"], cwd=repo, check=True, capture_output=True).stdout
    return [name for name in listing.decode("utf-8").split("\x00") if name]


def findings(repo=REPO):
    """Map of tracked path to the reasons it is refused."""
    result = {}
    for name in tracked_files(repo):
        path = Path(repo, name)
        if path.is_symlink() or not path.is_file():
            continue
        reasons = file_findings(name, path.read_bytes())
        if reasons:
            result[name] = reasons
    return result


def main():
    refused = findings()
    for name, reasons in sorted(refused.items()):
        for reason in reasons:
            print(f"{name}: {reason}", file=sys.stderr)
    if refused:
        print("Remove the embedded data, or the file, before committing.", file=sys.stderr)
        return 1
    print("Tracked assets carry no embedded metadata.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
