#!/usr/bin/env python3
"""Compare CLI exit status and exact output against a reference executable.

Pass a JSON list of cases with a name and CLI args. Cases with an intentional
difference supply a category and the approved SHA-256 signatures of both
executions. Fail on any unapproved difference or an obsolete expectation.
"""

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def execute(binary, args, scratch):
    env = dict(os.environ)
    for name in ("HOME", "USERPROFILE", "LOCALAPPDATA", "APPDATA",
                 "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME",
                 "XDG_CACHE_HOME", "TMPDIR", "TEMP", "TMP"):
        env[name] = scratch
    proc = subprocess.run([binary, *args], env=env, cwd=scratch,
                          capture_output=True, timeout=30, check=False)
    return proc.returncode, proc.stdout, proc.stderr


def signature(output):
    code, stdout, stderr = output
    return [code, hashlib.sha256(stdout).hexdigest(), hashlib.sha256(stderr).hexdigest()]


def compare(case, baseline, candidate, scratch):
    args = case["args"]
    if args[0] not in ("check", "generate") or "--json" not in args:
        raise ValueError(f"expected JSON check or generate: {args!r}")
    with tempfile.TemporaryDirectory(dir=scratch) as detached:
        args = list(args)
        if "--inventory-state" in args:
            index = args.index("--inventory-state") + 1
            template = Path(args[index])
            state = Path(detached) / "inventory" / template.name
            state.parent.mkdir(mode=0o700)
            args[index] = str(state)
            original = template.read_bytes()
        else:
            state = None
        outputs = []
        for binary in (baseline, candidate):
            if state is not None:
                state.write_bytes(original)
                state.chmod(0o600)
            outputs.append(execute(binary, args, detached))
        before, after = outputs
    expected = case.get("difference")
    if expected is None and before == after:
        return "identical", None
    if expected and before != after and (
        signature(before), signature(after)
    ) == (expected["baseline"], expected["candidate"]):
        return expected["category"], None
    return "unexpected", {
        "name": case["name"],
        "args": case["args"],
        "baseline": signature(before),
        "candidate": signature(after),
        "before": [part.decode(errors="replace") for part in before[1:]],
        "after": [part.decode(errors="replace") for part in after[1:]],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline", type=Path)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("cases", type=Path)
    parser.add_argument("--differences", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    cases = json.loads(args.cases.read_text())
    baseline, candidate = args.baseline.resolve(), args.candidate.resolve()
    if not cases or len({case["name"] for case in cases}) != len(cases):
        parser.error("supply a nonempty list of uniquely named cases")
    counts = Counter()
    with tempfile.TemporaryDirectory() as scratch, args.differences.open("w") as output:
        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            for category, difference in pool.map(
                lambda case: compare(case, baseline, candidate, scratch), cases
            ):
                counts[category] += 1
                if difference is not None:
                    output.write(json.dumps(difference) + "\n")
    print(f"Cases run: {len(cases)}")
    for category in ("identical", "(a)", "(b)", "unexpected"):
        print(f"{category}: {counts.pop(category, 0)}")
    for category, count in sorted(counts.items()):
        print(f"{category}: {count}")
    return int(args.differences.stat().st_size != 0)


if __name__ == "__main__":
    raise SystemExit(main())
