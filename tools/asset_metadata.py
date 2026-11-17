#!/usr/bin/env python3
"""Fail when a tracked image or binary asset carries embedded metadata.

Images can hold data that is not needed to display them: provenance manifests,
Exif and XMP blocks, text chunks, colour-profile descriptions and editor notes.
Every tracked file that is not plain UTF-8 text must be a PNG made only of the
chunks listed below, at the sizes the format allows. Tracked SVG files must be
well-formed, hold SVG and XLink names only, and carry no metadata element,
comment, processing instruction or document type. Any other binary type fails
until a check for it exists.

Run from anywhere: python3 tools/asset_metadata.py
"""
import re
import struct
import subprocess
import sys
import xml.etree.ElementTree as ElementTree
import zlib
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
# Chunks that decide how the picture is drawn. Text, time, Exif, colour-profile
# and private chunks (tEXt, zTXt, iTXt, tIME, eXIf, iCCP, caBX, ...) are refused.
# Each maps to the body sizes the format allows, so a chunk cannot carry spare bytes.
PNG_CHUNKS = {
    b"IHDR": lambda size: size == 13,
    b"PLTE": lambda size: 3 <= size <= 768 and size % 3 == 0,
    b"IDAT": lambda size: True,
    b"IEND": lambda size: size == 0,
    b"tRNS": lambda size: 1 <= size <= 256,
    b"gAMA": lambda size: size == 4,
    b"cHRM": lambda size: size == 32,
    b"sRGB": lambda size: size == 1,
    b"sBIT": lambda size: 1 <= size <= 4,
    b"bKGD": lambda size: size in (1, 2, 6),
    b"pHYs": lambda size: size == 9,
}
# Names are compared by namespace, so a renamed prefix changes nothing.
SVG_NAMESPACES = {"http://www.w3.org/2000/svg", "http://www.w3.org/1999/xlink", "http://www.w3.org/XML/1998/namespace"}
SVG_TEXT_MARKERS = re.compile(r"<!--|<!DOCTYPE|<!ENTITY|<\?(?!xml\s)", re.IGNORECASE)


def png_findings(data):
    """Reasons a PNG is refused; empty when it holds display chunks only."""
    if not data.startswith(PNG_SIGNATURE):
        return ["not a PNG file"]
    findings, offset, ended, pixels = [], len(PNG_SIGNATURE), False, b""
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
        elif not PNG_CHUNKS[kind](length):
            findings.append(f"chunk {kind.decode('latin-1')} has a size the format does not allow ({length} bytes)")
        if kind == b"IDAT":
            pixels += body
        ended = kind == b"IEND"
        offset = end
    if not ended and not findings:
        findings.append("no end-of-image chunk")
    if not findings:
        stream = zlib.decompressobj()
        try:
            stream.decompress(pixels)
        except zlib.error:
            findings.append("image data does not decompress")
        else:
            if not stream.eof or stream.unused_data:
                findings.append("image data chunks hold bytes beyond the compressed image")
    return findings


def split_name(name):
    """Namespace and local name of an ElementTree tag or attribute name."""
    return name[1:].split("}", 1) if name.startswith("{") else ("", name)


def svg_findings(text):
    """Reasons an SVG is refused; empty when it holds drawing markup only."""
    findings = {f"contains {match.group(0)!r}" for match in SVG_TEXT_MARKERS.finditer(text)}
    if findings:
        # Comments, processing instructions, document types and entities are refused unparsed.
        return sorted(findings)
    try:
        root = ElementTree.fromstring(text)
    except ElementTree.ParseError:
        return ["not well-formed XML"]
    for element in root.iter():
        namespace, local = split_name(element.tag)
        if namespace not in SVG_NAMESPACES:
            findings.add(f"element {local!r} is outside the SVG namespace ({namespace or 'none'})")
        elif local == "metadata":
            findings.add("contains a metadata element")
        for attribute in element.attrib:
            namespace, local = split_name(attribute)
            if namespace and namespace not in SVG_NAMESPACES:
                findings.add(f"attribute {local!r} is outside the SVG namespaces ({namespace})")
            elif not namespace and local.startswith("data-"):
                findings.add(f"attribute {local!r} carries application data")
    return sorted(findings)


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
