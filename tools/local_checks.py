#!/usr/bin/env python3
"""Credential-free local Rust gates, schemas and synthetic MCP replay.

Uses the caller's pinned Rust environment and external CARGO_TARGET_DIR.
All scratch output goes under --out, outside the source tree.
"""
import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
SCHEMAS = ("generate", "check", "pack", "discovery", "result", "assess", "setup", "inventory", "status")


def execute(command, out, name, accepted=(0,)):
    start = time.perf_counter()
    proc = subprocess.run(command, cwd=REPO, capture_output=True, timeout=900)
    (out / (name + ".log")).write_bytes(proc.stdout + proc.stderr)
    if proc.returncode not in accepted:
        raise RuntimeError(f"{name} exited {proc.returncode}; see {name}.log")
    return proc.stdout, round(time.perf_counter() - start, 6)


def cli(binary, args, out, name, accepted=(0,)):
    raw, elapsed = execute([str(binary), *args], out, name, accepted)
    return json.loads(raw), elapsed


def fixture(probe):
    kind, field = probe["kind"], probe.get("field")
    if kind == "tag-detect":
        name = "count.json" if field == "event_simpleName" else "zero.json"
    elif kind == "record-kind-facet":
        name = {"#type": "kind.json", "#event_simpleName": "simple-name.json"}[field]
    else:
        name = {"census": "census.json", "volume": "count.json", "dataset-facet": "dataset.json",
                "fieldset": "fieldset.json", "population": "population.json", "value-facet": "value.json"}[kind]
    return REPO / "examples" / "mcp" / name


def synthetic_replay(binary, out):
    state, pack, receipt = (out / name for name in ("state.json", "pack.json", "receipt.json"))
    step, _ = cli(binary, ["env", "discover", "--mcp", "--request", str(REPO / "examples/mcp/discover.request.json"),
                          "--state", str(state), "--pack", str(pack), "--receipt", str(receipt)], out, "init", (0, 3))
    rounds = 0
    while step.get("status") == "needs-probes":
        rounds += 1
        if rounds > 8:
            raise RuntimeError("Replay round bound exceeded")
        answers = []
        for probe in step["probes"]:
            answers.extend(("--answer", f"{probe['probe_id']}={fixture(probe)}"))
        step, _ = cli(binary, ["env", "discover", "--mcp", "--state", str(state), *answers],
                      out, f"step-{rounds}", (0, 3))
    if step.get("status") != "complete" or not pack.is_file() or not receipt.is_file():
        raise RuntimeError("Replay did not activate")
    text = 'show events from synthetic_repo where user equals "synthetic-user" select user limit 20'
    ambiguous, _ = cli(binary, ["generate", "--intent", text, "--pack", str(pack), "--require-observed", "--json"],
                       out, "generate-ambiguous", (1,))
    if ambiguous["outcome"] != "needs-context" or not any(d["code"] == "CQL_RECORD_KIND_AMBIGUOUS" for d in ambiguous["diagnostics"]):
        raise RuntimeError("Mixed-kind context request missing")
    generated, _ = cli(binary, ["generate", "--intent", text, "--pack", str(pack), "--require-observed",
                               "--accept-mixed-record-kinds", "--json"], out, "generate-accepted")
    if generated["outcome"] != "ready":
        raise RuntimeError("Explicit mixed-kind generation was not ready")
    query = out / "query.cql"
    query.write_text(generated["query"], encoding="utf-8", newline="\n")
    assessed, _ = cli(binary, ["assess", "--query-file", str(query), "--result", str(REPO / "examples/mcp/value.json"),
                              "--pack", str(pack), "--json"], out, "assess", (1,))
    if assessed["outcome"] != "advisory" or assessed["rows"] != 1:
        raise RuntimeError("Synthetic assessment changed")
    return {"rounds": rounds, "status": step["status"], "assess": assessed["outcome"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--smoke-only", action="store_true")
    args = parser.parse_args()
    out = args.out.resolve()
    if REPO == out or REPO in out.parents:
        raise RuntimeError("Output must be outside source tree")
    out.mkdir(parents=True, exist_ok=False)
    summary = {"platform": sys.platform, "gates": {}}
    if not args.smoke_only:
        commands = {
            "fmt": ["cargo", "fmt", "--all", "--check"],
            "clippy": ["cargo", "clippy", "--offline", "--locked", "--all-targets", "--", "-D", "warnings"],
            "test": ["cargo", "test", "--offline", "--locked", "--no-fail-fast"],
            "build": ["cargo", "build", "--release", "--offline", "--locked"],
        }
        for name, command in commands.items():
            _, elapsed = execute(command, out, name)
            summary["gates"][name] = {"exit": 0, "seconds": elapsed}
            print(name + " PASS", flush=True)
    target = Path(os.environ.get("CARGO_TARGET_DIR", str(REPO / "target")))
    binary = (args.binary or target / "release" / ("cqlmatic.exe" if os.name == "nt" else "cqlmatic")).resolve()
    summary["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
    for name in SCHEMAS:
        value, _ = cli(binary, ["--schema", name], out, "schema-" + name)
        if value.get("type") != "object":
            raise RuntimeError("Invalid schema root: " + name)
    summary["synthetic_replay"] = synthetic_replay(binary, out)
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, sort_keys=True))


if __name__ == "__main__":
    main()
