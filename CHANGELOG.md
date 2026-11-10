# Changelog

## 0.1.4

- Generation, checking and saved-result assessment automatically reuse a matching confirmed Full or Quick inventory's active pack and otherwise unspecified target. Explicit packs remain primary; matching inventory guards still apply to generation/checking. Assessment adds `--inventory-state` without deriving population rates from the query ledger or saving leads.
- Inventory selection checks metadata and identity before opening the active snapshot, separately for every JSONL row. Foreign inventory is skipped with a notice. A missing default remains benign; explicit missing state and broken matching active evidence fail closed even in ordinary mode. Protected reads do not create directories.
- Ordinary mixed or incompletely sampled record kinds produce informational `CQL_RECORD_KIND_NOTICE`. Strict observed mode retains kind requirements and accepted warnings, including nested queries and `--fail-on-warning` behavior.
- Discovery separates fieldset listings from actual field measurements. Exact population batches preserve untargeted records and dates; current auxiliary zero measurements can retain a null population rate. Current proof and retained history remain distinct in field/status evidence.
- Refresh rejects contradictory backdated, tied or unorderable measurements while preserving the existing newer-snapshot requirement, exact source restrictions and atomic replacement. An empty optional value facet for an absent field no longer aborts other source measurements; malformed nonempty facets still fail.
- Undated negative sources recover only from a qualified source measurement whose window starts after the retained snapshot. Null or missing source observations cannot erase retained negative or historical source evidence.
- Existing request/schema versions, pack shape and executable profile remain unchanged. Legacy evidence and imported metadata retain their compatibility boundaries.

## 0.1.3

- Quick inventory offers a persistent selected-source workflow with an explicit target, UTC window and finite scout budget. Switching to Full retains evidence without marking the full map complete.
- Pack-free generation and checking can use a confirmed Quick target and save bounded source and input-field names as unverified leads. Explicit request or pack identity takes precedence.
- Saved analyst query results can contribute field-specific, caller-attested observations when the query and response qualify; raw values and query text stay outside inventory state. Later negative or uncertain measurements and active-pack record-kind ambiguity still govern strict grounding.
- Type-only sources work through inventory and measurement. Imported packs remain visible but unconfirmed, and compatible historical imports merge without using measurement refresh.
- Optional summaries reduce status, inventory and pack-inspection output while retaining full JSON by default. Partial positive coverage requires explicit acceptance in strict mode; regex and wildcard use on unscoped raw text receives a cost warning.

## 0.1.2

- Setup inspects executable paths with bounded streaming hashes and reports an individual client failure without hiding other detected clients.
- Windows setup reports the path, SID, and missing ACL right, and checks existing directory ancestors before and after creation. Recoverable menu action errors return to the menu.
- External-host setup treats host identity as operator-supplied, carries the chosen host ID into next actions, and identifies additional discovered tools without assuming their capabilities.
- Inventory text status shows readable source selectors beside stable IDs. Mixed `#event.dataset` guidance explains why an exact event-name selector alone does not settle the record kind.
- Release archives and the npm tarball verify extracted versioned install instructions against release metadata.

## 0.1.1

- Native MCP setup for Claude Code and GitHub Copilot CLI, app and VS Code Copilot harness. Copilot CLI launch names share one adapter and configuration.
- Tracked guided setup for other capable agents, with a bound saved tool-discovery report.
- Setup requires the selected session's effective OAuth client ID for Claude and Copilot reports, rejects contradictory IDs in legacy Codex/VS Code reports, and requires exact available `falcon_search_ngsiem` before verifying discovery.
- The built-in `search-all` inventory scout requires separate authorization; selected-source discovery can continue when scouting remains pending.
- Removed the dated source-review line from interactive setup.

## 0.1.0

First public release.

- Native Rust CLI for recipes, typed plans, bounded English, checking, linting, reference lookup, environment packs, and saved-result assessment.
- On-demand `npx cqlmatic@0.1.0 setup` with bundled macOS Apple silicon and Linux x86_64 executables; use a supported, patched Node.js release as specified in the [README](README.md#try-v014).
- Resumable MCP setup for Codex CLI and VS Code, with CIMD preference, manual registration guidance, command previews, and separate configuration, authentication, and tool-discovery states.
- Environment inventory and offline status covering scouting, confirmed context, parser catalog/bindings/semantic review, source and field measurements, and reconciliation.
- Source-bound field batches can measure parser-declared fields absent from event samples. Partial refresh preserves untargeted evidence, original dates, annotations, and historical observations.
- MCP discovery uses a resumable probe/answer loop through `falcon_search_ngsiem`. Native Falcon API discovery remains an explicit, bounded path. Both retain scoped evidence and protected local outputs.
- Record-kind facets and explicit `kind_fields` support mixed-source review. `CQL_RECORD_KIND_AMBIGUOUS` requests context for unconstrained mixed kinds; explicit acceptance retains a warning.
- `head`, `tail`, and `table` accept `limit=max`. `groupBy(limit=max)` reports its remaining ceilings with informational `CQL_GROUPBY_LIMIT` output.
- Saved-result assessment reports `no-findings`, `advisory`, `inconclusive`, or `error` without certifying correctness or completeness. MCP inputs reject duplicate keys and inconsistent result copies.
- Protected files, writer locks, finite network budgets, bounded job cleanup, and recoverable pack/receipt activation. A post-replacement durability failure is reported even when the new complete file is already visible.
- Versioned JSON/JSONL interfaces, bundled schemas, synthetic examples, Windows/Linux CI, and setup, usage, security, and contributor guides.
- Licensed under [Apache-2.0](LICENSE), with third-party attribution in [NOTICE](NOTICE).

### Compatibility

- Package version, capability profile, and JSON schema version are independent. This release uses `cqlmatic-ngsiem-core-v2` and request schema version 1.
- Packs include `record_kinds`; use the bundled schema when consuming them. Supported legacy core-v1 packs retain their original evidence and digest.
- MCP state format 2 binds the discovery algorithm, destinations, and frozen activation bytes. Older state files require a new run. Interrupted activation resumes without merging evidence twice.
- Text files use LF line endings. Keep them as LF when running schema drift checks on Windows.

See [coverage and limits](docs/COVERAGE.md) for the supported syntax and assurance boundaries.
