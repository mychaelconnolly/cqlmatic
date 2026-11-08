# Changelog

## 0.1.0

First public release.

- Native Rust CLI for recipes, typed plans, bounded English, checking, linting, reference lookup, environment packs, and saved-result assessment.
- On-demand `npx cqlmatic@0.1.0 setup` with bundled macOS Apple silicon and Linux x86_64 executables; use a supported, patched Node.js release as specified in the [README](README.md#try-v010).
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
