#!/usr/bin/env python3
"""The credential-free checks as one command for a workstation and for CI.

Runs the asset, changelog, Rust, launcher and tooling gates, lints the Windows
build from macOS or Linux, then smoke-tests the release executable, its schemas
and the synthetic MCP replay. Site checks are separate. Cargo runs through
./dev except on Windows. Scratch output goes under --out, or a new temporary
folder when --out is not given; either way outside the source tree.
"""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
SCHEMAS = ("generate", "check", "pack", "discovery", "result", "assess", "setup", "inventory", "status")
CARGO = ["cargo", "+1.99.1"] if os.name == "nt" else [str(REPO / "dev")]
WINDOWS_TARGET = "x86_64-pc-windows-msvc"
CLIPPY = ["clippy", "--offline", "--locked", "--all-targets", "--", "-D", "warnings"]
# clippy.toml lists reads with no size limit. Tests may use them, so they are denied for shipped code only.
CLIPPY_SHIPPED = ["clippy", "--offline", "--locked", "--lib", "--bins", "--",
                  "-D", "warnings", "-D", "clippy::disallowed_methods"]


def execute(command, out, name, accepted=(0,), env=None):
    start = time.perf_counter()
    try:
        proc = subprocess.run(command, cwd=REPO, capture_output=True, timeout=900, env=env)
        log, code = proc.stdout + proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as expired:
        log, code = (expired.stdout or b"") + (expired.stderr or b""), "none after 900 seconds"
    (out / (name + ".log")).write_bytes(log)
    if code not in accepted:
        sys.stderr.write(log.decode("utf-8", "replace"))
        raise SystemExit(f"{name} FAIL: exit {code}; log in {out / (name + '.log')}")
    return proc.stdout, round(time.perf_counter() - start, 6), code


def cli(binary, args, out, name, accepted=(0,)):
    # The executable must not find, read or add to the caller's own inventory or setup state.
    home = out / "home"
    home.mkdir(mode=0o700, exist_ok=True)
    env = {**os.environ, **dict.fromkeys(
        ("HOME", "USERPROFILE", "LOCALAPPDATA", "XDG_DATA_HOME", "XDG_STATE_HOME"), str(home))}
    raw, _, code = execute([str(binary), *args], out, name, accepted, env)
    return json.loads(raw), code


def windows_lint_environment():
    """Clippy only type-checks, but a dependency's build script still compiles C
    for the target. Nothing is linked, so any C compiler for 64-bit Windows will
    do, with an archiver that takes MSVC `lib` arguments, such as llvm-lib."""
    env = dict(os.environ)
    llvm = [shutil.which("llvm-lib"), "/opt/homebrew/opt/llvm/bin/llvm-lib", "/usr/local/opt/llvm/bin/llvm-lib"]
    tools = {"CC": shutil.which("x86_64-w64-mingw32-gcc"),
             "AR": next((path for path in llvm if path and Path(path).is_file()), None)}
    for kind, found in tools.items():
        name = f"{kind}_{WINDOWS_TARGET.replace('-', '_')}"
        if name not in env:
            if not found:
                raise SystemExit(
                    f"clippy-windows FAIL: no {kind} for {WINDOWS_TARGET}. Install mingw-w64 and LLVM "
                    f"(x86_64-w64-mingw32-gcc and llvm-lib) or set {name}, and run "
                    f"`rustup target add {WINDOWS_TARGET}`. Windows-only code is linted here so that "
                    "it does not first fail in the hosted Windows job.")
            env[name] = found
    return env


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
    step, code = cli(binary, ["env", "discover", "--mcp", "--request", str(REPO / "examples/mcp/discover.request.json"),
                             "--state", str(state), "--pack", str(pack), "--receipt", str(receipt)], out, "init", (3,))
    rounds = 0
    while step.get("status") == "needs-probes":
        rounds += 1
        if rounds > 4:
            raise RuntimeError("Replay round bound exceeded")
        answers = [(probe, fixture(probe)) for probe in step["probes"]]
        if not answers:
            raise RuntimeError("Replay offered no probe")
        for index, (probe, answer) in enumerate(answers, 1):
            cli(binary, ["env", "discover", "--mcp", "--state", str(state),
                         "--dispatch", probe["probe_id"]], out, f"dispatch-{rounds}-{index}", (3,))
            step, code = cli(binary, ["env", "discover", "--mcp", "--state", str(state),
                                     "--answer", f"{probe['probe_id']}={answer}"],
                             out, f"step-{rounds}-{index}", (0, 3))
            if code == 0:
                break
    if code != 0 or step.get("status") != "complete" or not all(
            path.is_file() and path.stat().st_size for path in (pack, receipt)):
        raise RuntimeError("Replay did not activate")
    text = 'show events from synthetic_repo where user equals "synthetic-user" select user limit 20'
    ambiguous, _ = cli(binary, ["generate", "--intent", text, "--pack", str(pack), "--require-observed", "--json"],
                       out, "generate-ambiguous", (1,))
    if ambiguous["outcome"] != "needs-context" or not any(d["code"] == "CQL_RECORD_KIND_AMBIGUOUS" for d in ambiguous["diagnostics"]):
        raise RuntimeError("Mixed-kind context request missing")
    generated, _ = cli(binary, ["generate", "--intent", text, "--pack", str(pack), "--require-observed",
                               "--accept-mixed-record-kinds", "--json"], out, "generate-accepted")
    if generated["outcome"] != "ready" or not any(
            d["code"] == "CQL_RECORD_KIND_AMBIGUOUS" and d["severity"] == "warning" for d in generated["diagnostics"]):
        raise RuntimeError("Explicit mixed-kind generation was not ready with its warning kept")
    query = out / "query.cql"
    query.write_text(generated["query"], encoding="utf-8", newline="\n")
    assessed, _ = cli(binary, ["assess", "--query-file", str(query), "--result", str(REPO / "examples/mcp/value.json"),
                              "--pack", str(pack), "--json"], out, "assess", (1,))
    findings = {(f["code"], f["severity"]) for f in assessed["findings"]}
    if assessed["outcome"] != "advisory" or assessed["rows"] != 1 or not findings >= {
            ("RESULT_COMPLETENESS_UNATTESTED", "info"), ("RESULT_KEY_COVERAGE", "warning")}:
        raise RuntimeError("Synthetic assessment changed")
    return {"rounds": rounds, "status": step["status"], "assess": assessed["outcome"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, help="new scratch folder outside the source tree (default: a temporary one)")
    parser.add_argument("--binary", type=Path)
    parser.add_argument("--smoke-only", action="store_true")
    parser.add_argument("--no-windows-lint", action="store_true",
                        help="skip linting the Windows build; only for a run paired with the hosted Windows job")
    args = parser.parse_args()
    if args.out:
        out = args.out.resolve()
        if REPO == out or REPO in out.parents:
            raise RuntimeError("Output must be outside source tree")
        out.mkdir(mode=0o700, parents=True, exist_ok=False)
    else:
        out = Path(tempfile.mkdtemp(prefix="cqlmatic-checks-")).resolve()
    summary = {"platform": sys.platform, "gates": {}}
    # In a target folder shared with another checkout, Cargo takes that checkout's results
    # for this one and reports success without compiling. Give this checkout its own part.
    metadata, _, _ = execute([*CARGO, "metadata", "--format-version=1", "--no-deps", "--offline", "--locked"],
                             out, "metadata")
    target = Path(json.loads(metadata)["target_directory"])
    if REPO not in target.parents:
        target = target / ("cqlmatic-" + hashlib.sha256(str(REPO).encode("utf-8")).hexdigest()[:16])
    os.environ["CARGO_TARGET_DIR"] = str(target)
    if not args.smoke_only:
        python = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}
        commands = {
            "assets": ([sys.executable, "tools/asset_metadata.py"], None),
            "changelog": ([sys.executable, "tools/changelog.py"], None),
            "fmt": ([*CARGO, "fmt", "--all", "--check"], None),
            "clippy": ([*CARGO, *CLIPPY], None),
            "clippy-shipped": ([*CARGO, *CLIPPY_SHIPPED], None),
        }
        if os.name != "nt" and not args.no_windows_lint:
            env = windows_lint_environment()
            for name, lint in (("clippy-windows", CLIPPY), ("clippy-windows-shipped", CLIPPY_SHIPPED)):
                commands[name] = ([*CARGO, lint[0], "--target", WINDOWS_TARGET, *lint[1:]], env)
        commands.update({
            "test": ([*CARGO, "test", "--offline", "--locked", "--no-fail-fast"], None),
            "launcher": (["node", "--test", "npm/launcher.test.cjs"], None),
            "tooling": ([sys.executable, "-m", "unittest", "discover", "-s", "tools", "-p", "test_*.py"], python),
            "build": ([*CARGO, "build", "--release", "--offline", "--locked",
                       "--message-format=json-render-diagnostics"], None),
        })
        for name, (command, env) in commands.items():
            built, elapsed, _ = execute(command, out, name, env=env)
            summary["gates"][name] = {"exit": 0, "seconds": elapsed}
            print(name + " PASS", flush=True)
        # Smoke-test the executable this build produced, wherever Cargo was configured to put it.
        artifacts = [json.loads(line) for line in built.decode("utf-8").splitlines() if line.startswith("{")]
        args.binary = args.binary or Path(next(
            a["executable"] for a in artifacts
            if a.get("reason") == "compiler-artifact" and a.get("executable") and a["target"]["name"] == "cqlmatic"))
    binary = (args.binary or target / "release" / ("cqlmatic.exe" if os.name == "nt" else "cqlmatic")).resolve()
    summary["binary_sha256"] = hashlib.sha256(binary.read_bytes()).hexdigest()
    execute([str(binary), "--version"], out, "version")
    cli(binary, ["generate", "--request", str(REPO / "examples/generate.request.json"), "--json"], out, "generate")
    for name in SCHEMAS:
        value, _ = cli(binary, ["--schema", name], out, "schema-" + name)
        if value.get("type") != "object":
            raise RuntimeError("Invalid schema root: " + name)
    summary["synthetic_replay"] = synthetic_replay(binary, out)
    (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, sort_keys=True))


if __name__ == "__main__":
    main()
