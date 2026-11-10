# Inventory request examples

All identifiers, sources, timestamps, and saved responses here are synthetic. Replace them for live work. Never import the synthetic responses as evidence about a real environment. Keep live requests, entire saved tool results, state, and packs in protected local storage outside this directory.

Run requests from the repository root with a new dedicated private state directory:

```sh
cqlmatic env inventory --state /private/inventory/state.json --request examples/inventory/init.request.json --json
cqlmatic status --state /private/inventory/state.json --json
```

The state directory is created with private permissions. An existing broadly accessible directory is refused. Use the same `--state` for every action below.

## Order

1. Complete `cqlmatic setup` and its MCP connection/tool-discovery steps.
2. Submit `init.request.json` once for a new state.
3. Submit `scout.request.json` with the intended CID/cloud/view and absolute UTC window. Built-in scouting always emits `repository: search-all`, even if that saved view differs. Execute the exact arguments only when explicitly authorized for search-all and the window. If only another view is authorized, leave scouting/full-map completion pending and use confirmed selected sources with lower-level bounded discovery on that view. Never edit the scout's emitted repository. Save the entire unedited result, including its envelope and metadata.
4. Adapt `import-scout.request.json` to that saved result, exact emitted arguments, and retrieval date. `scout.synthetic.json` is an offline demonstration only. Scout import checks shape/errors/incomplete flags but does not reject an unflagged empty result or apply discovery's 10,000-row ceiling. Review such results independently; imported scouting status is not a completeness attestation.
5. Submit `confirm.request.json` after reviewing the seeded target. Confirm full-environment or selected-source scope, operational context, and any expected sources omitted by scouting. Missing or inaccessible sources remain pending.
6. For every expected source, adapt `review.request.json` with its exact `status.sources[].source_id`, purpose, data model, and supporting evidence.
7. Use `fetch-parsers.request.json` with a protected credential-file path and explicit page/parser/time budgets. The separate API client requires NGSIEM Parsers: READ and the same tenant/cloud as the MCP target. Alternatively, adapt `import-parsers.request.json` to saved native API pages and exact-ID details. Preserve complete native responses. `parser-page.synthetic.json` and `parser.synthetic.json` demonstrate the shapes without credentials.
8. Adapt `bind.request.json` using the exact source ID and `status.parsers[].id`/`sha256`. Read the saved definition and review every `status.parsers[].review_lines` entry. The synthetic parser requires line 3 review; its interpretation below is illustrative. A new parser hash requires new binding and semantic review. Use `parser-not-applicable` with a supported reason/evidence for native sources, or `exclude-parser` with a snapshot hash/reason for catalog entries outside the target. Every catalog entry needs classification; names alone establish no binding.
9. Review the discovery warning below, then adapt `discover.request.json` with selected source IDs, absolute UTC windows, explicit field batches where needed, and finite limits. Parser declarations include fields absent from samples; measure those in later `population_fields` batches. Include observed requested kind/value fields in the applicable batch.
10. Execute only each emitted relay probe through the authorized MCP. Save each entire unedited tool result. Adapt `step.request.json` with its exact `probe_id` and saved `result_file`; repeat while the relay reports `needs-probes`. Empty `answers` resumes progress without importing a new answer. Failed/capped/timed-out work remains pending and can require another bounded run.
11. Resolve the status checklist, submit `reconcile.request.json`, then inspect `cqlmatic status --state ... --json` again. Changed evidence invalidates reconciliation. Legacy imports have unknown workflow history.
12. `generate`, `check` and `assess` reuse the matching confirmed inventory's active pack in Full or Quick mode. Use `--inventory-state FILE` for this nondefault state, or select another primary snapshot with `--pack FILE`. Matching inventory safeguards still apply. The active pack accumulates completed sources and field batches; prior snapshots remain unchanged. Covered queries can use that evidence while other inventory work remains pending.

Initial environment discovery is extensive. It examines your selected sources through multiple searches and may take substantial time. Agent-assisted discovery can also consume significant model tokens. Review the scope and limits before starting. Completed discovery is saved and reused for future query writing.

`full_map_complete` requires the confirmed full target, scouting, source context, catalog classification, exact parser review, measurements, and current reconciliation. It applies to stored evidence and account visibility. An explicit zero is a measurement, never an observation.


See [the inventory guide](../../docs/INVENTORY.md) for every action's fields, native-parser classifications, parser exclusion, pack import, failure recording, and completion rules; see [contracts](../../docs/CONTRACTS.md) for output framing. Action requests return `{schema_version, state, status, action}`, where `state` is a state-file path string. In contrast, `status --json` and no-request `env inventory --json` return the status object directly. Exit 0 means the command completed; inspect the relay/parser run and checklist to decide whether work remains.

Saved parser imports replace the intended catalog/definition set. Catalog completion needs contiguous pages from offset 0, stable reported totals matching distinct IDs, and one exact-ID native definition for each catalog entry. Supply the full intended capture set; a partial later import is not an append. Review each status `review_lines` entry against the exact saved script and hash. Changing hashes invalidates old bindings/exclusions and requires fresh review.

Discovery windows are increasing absolute UTC intervals of at most seven days. `population_fields` contains exact `{source, fields}` selections for sources in the run; ordinary and tagged names differ. Explicit zero measurements can satisfy measurement requirements while declarations remain unobserved and in review. The latest run for a source and active cumulative pack determine current coverage; an old successful run does not erase a newly incomplete refresh.

`reconcile` records an evidence fingerprint, not a completion override. `covered_queries_usable` does not prove a particular query is covered and can be true before any pack exists. Inspect the active target and snapshot, then use [query reference](../../docs/QUERY-REFERENCE.md) and [saved-result assessment](../../docs/RESULT-ASSESSMENT.md). Automatic selection validates the protected active reference; it does not establish query correctness or replace Full review.
