# CQLmatic v0.1.4

CQLmatic now reuses your saved environment map when you write, check or assess a query, in both Full and Quick inventory modes. Ordinary query writing can continue with a notice when a source mixes different event types. Small refreshes keep earlier evidence for fields they did not measure and avoid treating a field listing as proof that the field contains data.

## What changed

- A matching confirmed inventory supplies its active pack and otherwise unspecified target automatically. `--inventory-state FILE` selects another protected inventory, including for `assess`. An explicit `--pack FILE` stays primary; matching active-pack and saved-query guards still apply to generation/checking.
- Inventory identity is checked before opening its active snapshot, separately for each JSONL request. Foreign inventory is skipped with a notice. A missing default state leaves basic commands available and creates nothing. An explicitly selected missing state or broken matching snapshot blocks use even in ordinary mode or with an older explicit pack. A malformed default can be ignored with a notice for a valid explicit pack; unsafe protected paths still fail closed.
- Normal mixed-kind queries report informational `CQL_RECORD_KIND_NOTICE`, including truncated kind facets and nested queries. With `--require-observed`, unresolved kinds still require context or explicit acceptance. Accepted strict ambiguity retains a warning and triggers `--fail-on-warning`.
- Fieldset-only fields remain unmeasured with null observation/rate. Exact population batches preserve untargeted records and dates. Actual zero measurements stay current negative evidence; auxiliary checks need not have a population rate. Full source and parser review remains separate.
- Refresh still requires a newer snapshot and now also rejects contradictory older, tied or unorderable field measurements. Undated negative sources and stale source markers can recover from a qualified source measurement whose window starts after the retained snapshot; null or missing observations cannot erase that source history. Rejected activation preserves the prior pack. Empty optional value facets for a field absent from a source do not abort other source measurements; malformed nonempty results still fail.
- Saved-query observations must start after the snapshot to supersede negative source or field evidence without a qualifying record window and matching measurement provenance. Qualified dated negatives keep their original ordering when repackaged; stale markers retain the snapshot boundary.
- Only Quick generation/checking saves unverified source/input-field leads. Assessment uses pack rates without converting the saved-query ledger into population evidence. Requests, schemas, pack shape and executable profile keep their existing versions.

## Before using it

Basic query writing needs no setup or discovery. Start with [setup](docs/SETUP.md), then inspect [inventory and status](docs/INVENTORY.md) when environment evidence is needed. Authorize the target, window and finite budgets before any search. Full discovery can take substantial time and assistant tokens; parser inventory requires a separate Falcon API client with NGSIEM Parsers: READ.

The npm package targets macOS Apple silicon and Linux x86_64 without install scripts or additional binary downloads. Standalone packaging supports Linux x86_64 and Windows x64. Verify release archives against `SHA256SUMS` before extraction. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`; the unsigned Windows executable uses a static CRT. Source builds require Rust 1.99.1.

`ready` describes local construction and the selected grounding policy. Neither it nor saved-result assessment proves vendor compilation, execution completeness or analyst-answer correctness. Saved-query observations retain the caller's assertion about query, target, window and response. Confirm operational results through your authorized CrowdStrike client; see [coverage and limits](docs/COVERAGE.md).

## Feedback

Include the version, platform and a minimal synthetic reproduction when reporting a problem. Keep credentials, tenant identifiers, packs and raw telemetry out of public issues. Use the [tester feedback form](https://github.com/mychaelconnolly/cqlmatic/issues/new/choose).
