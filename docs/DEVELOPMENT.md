# Development

Use Rust 1.99.1 and the checked-in lockfile. `./dev` resolves the pinned compiler or an identically versioned stable installation, keeps dependencies under ignored `.local/cargo`, and does not change global toolchain settings.

```sh
./dev fetch --locked
./dev fmt --all --check
./dev clippy --offline --locked --all-targets -- -D warnings
./dev test --offline --locked
./dev build --release --locked --offline
```

The build produces `cqlmatic`. Tests run locally without credentials or network access.

The optional npm launcher uses Node.js 22+ built-ins and has no npm dependencies. Use a [supported, patched Node.js release](../README.md#try-v017); the package's engine field describes API compatibility, not security-patch status. Run its isolated native-fixture checks with `node --test npm/launcher.test.cjs`. Native executables are added only when producing the reviewed npm archive; they are not stored in the source tree. See [release preparation](RELEASING.md#npm-package) for packaging and platform requirements.

`CQLMATIC_CARGO_HOME` overrides the wrapper's project-local Cargo cache; ordinary Cargo settings such as `CARGO_TARGET_DIR` can place build artifacts elsewhere. This does not change the pinned compiler requirement.

Protected-path tests reject symlinked ancestors. If the OS temporary path traverses a symlink, select a canonical private temporary directory. For example on macOS:

```sh
test_tmp=$(mktemp -d /private/tmp/cqlmatic-tests.XXXXXX)
TMPDIR="$test_tmp" ./dev test --offline --locked
```

## Native Windows

Use PowerShell from the repository root; `./dev` requires a POSIX shell. Keep checkout line endings as LF so schema drift tests compare the checked-in bytes. `.gitattributes` sets LF for text files; Windows CI also disables Git's automatic CRLF conversion before checkout and runs the following commands with a project-local Cargo cache:

```powershell
$env:CARGO_HOME = Join-Path $PWD '.local/cargo'
rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo +1.99.1 fetch --locked
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo +1.99.1 fmt --all --check
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo +1.99.1 clippy --offline --locked --all-targets -- -D warnings
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo +1.99.1 test --offline --locked
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
cargo +1.99.1 build --release --locked --offline
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
.\target\release\cqlmatic.exe --version
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
.\target\release\cqlmatic.exe generate --request examples/generate.request.json --json
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
foreach ($schema in 'generate', 'check', 'pack', 'discovery', 'result', 'assess', 'setup', 'inventory', 'status') {
    $output = .\target\release\cqlmatic.exe --schema $schema
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    $contract = $output | ConvertFrom-Json -ErrorAction Stop
    if ($contract.type -ne 'object') { throw "Invalid $schema schema root." }
}
```

Keep the immediate exit-code checks when running native commands in PowerShell scripts so a later command cannot hide an earlier failure. Hosted checks use synthetic fixtures without credentials. A passing Windows job covers the exercised paths; it does not establish guarantees for every Windows filesystem or power-loss recovery.

For a distributable Windows build, use PowerShell 7, Python 3 and the release helper rather than the ordinary development build above:

```powershell
./tools/windows-release.ps1 -OutDir "$env:RUNNER_TEMP/windows-artifact"
```

Set dedicated absolute `CARGO_HOME`, `CARGO_TARGET_DIR` and `RUNNER_TEMP` build directories first. The helper installs the pinned build-time toolchain, runs native checks, applies target-only static CRT/path-remapping flags and inspects PE imports/strings before writing the executable and build metadata. End users of the ZIP do not run this helper or install build tools.

## Contract changes

Rust request/result types are the source of truth. Test-only `schemars` derives the nine bundled JSON schemas (generate, check, pack, discovery, result, assess, setup, inventory and status); ordinary tests detect drift. For an intentional contract update:

```sh
UPDATE_SCHEMAS=1 ./dev test --lib schemas --offline --locked
```

Review the schemas, adapters, examples and result semantics together. Do not add a second runtime schema engine or duplicate request models.

## Tests

Unit and property tests cover parser contexts/budgets, escaping, strict English consumption, missing policies, placement, regex syntax, schema drift, JSONL recovery, source-aware grounding, record-kind ambiguity, MCP relay/result normalization and assessment, pack integrity, writer locks, process termination, I/O failures and snapshot commit phases. Network lifecycle tests use fake transports and synthetic responses. CI replays the synthetic MCP fixtures offline; it does not attest search completeness or tenant behavior.

Use minimal synthetic fixtures with explicit expected results. Cover unsupported inputs and operational errors as well as successful requests. Add a focused regression when changing non-trivial behavior.

For release tooling, run the standard-library checks:

```sh
python3 -m unittest discover -s tools -p 'test_*.py'
```

For website changes, use a supported, patched Node.js release and the locked private build package:

```sh
node --test site/theme.test.cjs
npm --prefix site ci
npm --prefix site test
npm --prefix site run build -- --commit "$(git rev-parse HEAD)"
```

The build renders reviewed Markdown to `site/dist/docs` for `docs.cqlmatic.dev` and copies the landing page to `site/dist/landing` for `cqlmatic.dev`. Both outputs are ignored and carry one source-commit binding and full file manifest. Preview them with `npm --prefix site run preview:landing` and `npm --prefix site run preview:docs` in separate terminals. Check light/dark/auto themes, keyboard focus, narrow-screen tables/code, links/anchors and HTTP 404 behavior on both hosts. The site dependencies are build tools; they are not dependencies of the published npm CLI.

## Source maintenance

`data/knowledge.json` contains catalog records; `src/catalog.rs` defines executable function contracts. [The function ledger](../data/function-scope.json) records official-page membership, applicability and prerequisites; [parameter facts](../data/function-parameters.json) record qualified choices, bounds and defaults. These files do not replace argument types, contextual validation or typed lowering. Standards data lives under `data/corpus/standards/`. `data/provenance.json` records reference revisions, source hashes, attribution and primary documentation links. Retain imported license text and notices. Changes to language rules need the corresponding reference, a focused regression and updated coverage documentation.

Changes to bundled reference material must preserve authorship, licenses and source meaning. Keep credentials, tenant data and internal working notes outside the source tree and distributed archives.

### Function-reference membership

The [primary function index](https://library.humio.com/crowdstrike-query-language/functions.html) and linked official pages were reviewed on 2026-11-14. The ledger has 196 function pages: 193 current Query candidates plus deprecated Query use of `lowercase`, with `copyEvent` excluded as parser-only and `math:spherical2cartesian` excluded as unreleased. There are 194 candidate names. Preserve deprecation separately from Query applicability and preserve existing catalog reference IDs, including legacy dot IDs; `lowercase` and `lower` remain distinct functions.

Reference refreshes must inspect linked navigation pages as well as the index table, article operation forms as well as summary signatures, and raw-page availability metadata as well as extracted article text. Preserve explicit restrictions, version/feature prerequisites, resource requirements and conflicting documentation as qualified facts. Do not infer requiredness, scalar use, complete output shape or executable support from a function name or summary alone. The [published grammar subset](https://library.humio.com/lql-grammar/syntax-grammar-guide.html) is intended for generation and explicitly is insufficient for parsing all vendor queries; documented special forms can exceed its simplified productions.

Update source hashes from the reviewed public bytes, keep imported pinned repository fields as ancestry, and maintain independent summaries instead of redistributing full manuals. When bundled knowledge changes, update the knowledge revision with the associated runtime metadata and stored-state compatibility checks. A source-page hash identifies reference bytes; it is not an execution receipt.

The core-v3 knowledge revision is the SHA-256 of the file-digest manifest described by `knowledge_revision_algorithm` and `knowledge_revision_inputs` in provenance. `knowledge_ancestry` retains the imported repository revision and original source hashes. Legacy profile admission uses that original pin; changing a pack's profile or revision does not migrate its evidence.

Build the current source, then check membership offline. Set `CQLMATIC_BIN` if the current build uses a different target directory:

```sh
./dev build --offline --locked
./dev test --offline --locked --test function_scope
./dev test --offline --locked --test function_coverage
python3 - <<'PY'
import json
import os
import re
import subprocess
from pathlib import Path

scope = json.loads(Path('data/function-scope.json').read_text())
expected = {f['canonical_name'] for f in scope['functions']
            if f['status'] == 'query_candidate'}
assert len(expected) == scope['counts']['query_candidates'] == 194
binary = os.environ.get('CQLMATIC_BIN', './target/debug/cqlmatic')
functions = json.loads(subprocess.check_output(
    [binary, 'catalog', '--functions', '--limit', '250']))
assert len(functions) == len({f['name'] for f in functions}) == 194
assert {f['name'] for f in functions} == expected
records = json.loads(Path('data/knowledge.json').read_text())
by_id = {r['id']: r for r in records}
assert len(by_id) == len(records)
for function in functions:
    record = by_id[function['reference_id']]
    url = ('https://library.humio.com/crowdstrike-query-language/functions-'
           + function['name'].lower().replace(':', '-') + '.html')
    assert url in record['sources']
parameters = json.loads(Path('data/function-parameters.json').read_text())
assert {f['name'] for f in parameters['functions']} == expected
provenance = json.loads(Path('data/provenance.json').read_text())
current = {r['url']: r for r in provenance['additional_references']
           if r['reviewed_at'] == '2026-11-14' and r.get('sha256')}
assert len(current) >= 349
assert all(re.fullmatch('[0-9a-f]{64}', r['sha256'])
           for r in current.values())
print('Function, parameter, guidance and public-reference membership agree.')
PY
```

Membership checks do not replace representative typed/raw positive and negative cases for each family, recursive context/output checks, result-assessment checks or the independent review gates in [releasing](RELEASING.md). An unavailable required reviewer leaves its gate pending.

### Optional capability observations and refresh

Field discovery can finish before the optional function-check phase. Maintain that separation: selecting Later must preserve the activated field pack. Review the capability cases in `src/capability.rs` and inventory action contracts together when changing the phase. Preparation, one-attempt reservation, exact-result/termination submission, pause/resume and unresolved-attempt blocking share the original finite allowance; rereading protected state replays expected-output checks.

Positive capability evidence is scoped to the exact environment/CID and view, caller-attested authorization context, case revision and resource binding. It expires after 30 days; a newer unknown attempt within that same scope supersedes an older positive result. Another authorization context neither inherits nor revokes it. Refresh only the applicable scope under current authorization, with complete captures and a verified finite search lifetime. A successful no-op or generic beta marker does not confirm a capability. Keep ordinary inventory refresh and field grounding separate from these observations, and use synthetic offline tests to exercise identity mismatch, expiry, incomplete output, budget exhaustion and unresolved termination without tenant access.

The capability case revision binds the fixed query, expected output and verifier-semantics version. Bump that version when normalization or verification acceptance changes, retaining historical-revision readability without a current capability claim. Refactoring with identical semantics must preserve existing revision digests.

## Documentation maintenance

Use [the documentation index](README.md) as the entry point, [USAGE](../USAGE.md) as the command reference, and [contracts](CONTRACTS.md) for machine framing. Check changes against the current Rust types, adapters, CLI `--help`, all nine schema exports, `catalog --functions --limit 250`, `catalog --recipes`, and `catalog --profile`. Schema shape alone does not describe runtime constraints or output/exit policy.

Keep [query examples](../examples/query-reference/README.md) runnable and canonical; that guide includes a standard-library verification block for request outcomes, diagnostic severities, warning policy, and emitted CQL. Reuse the synthetic [setup](../examples/setup/README.md), [inventory](../examples/inventory/README.md), and [MCP](../examples/mcp/README.md) walkthroughs. Verify negative/error paths and explicit limits as well as ready examples. Never substitute operational captures or authenticated testing claims for synthetic examples.

Check local Markdown paths/anchors and links from README, USAGE, guides, and agent instructions. The analyst workflow is embedded in the catalog through `include_str!`; a documentation edit can change the binary, so run the Rust checks and verify `catalog cql:workflow:analyst --full --limit 1` after rebuilding. Keep the full guide, snippet, and optional agent skill consistent about exact query/result bytes, confirmed target, authorization, and evidence limits.

Update capability and mechanism statements when behavior changes, without promoting static acceptance to execution/completeness claims. Keep package, request/schema, profile, knowledge, and stored-state versions distinct. Validate release checksums and rebuild archives through the release procedure when shipping documentation changes; older packages do not automatically gain new guide files.
