# Environment discovery and status

Basic `generate` and `check` work offline without a pack or completed inventory. The environment map adds scoped evidence for choosing and grounding telemetry. The assistant interprets source purpose and parser transformations; CQLmatic stores evidence, constructs measurements, tracks gaps, and grounds query fields. A complete map combines telemetry, parser inventory, and confirmed operational context.

Full discovery is the default. Initial full environment discovery is extensive. It examines your selected sources through multiple searches and may take substantial time. Agent-assisted discovery can also consume significant model tokens. Review the scope and limits before starting. Completed discovery is saved and reused for future query writing.

## Start and resume

```sh
cqlmatic env inventory
cqlmatic status
cqlmatic status --json
cqlmatic status --json --summary
cqlmatic env inventory --json --summary
```

Start assistant work with `status --json --summary`; request full status or focused evidence when its counts and next actions show a relevant gap. Use `--state PATH_TO_PRIVATE_STATE/state.json` on both commands for separate environments. Terminal inventory provides guided actions; your assistant can use `--request FILE --json`. Without a terminal or request, inventory reports status and exits without starting a search.

```sh
cqlmatic env inventory --state PATH_TO_PRIVATE_STATE/state.json --request examples/inventory/init.request.json --json
cqlmatic --schema inventory
cqlmatic --schema status
```

Examples use synthetic data. Adapt identities, dates, paths, and sources for live work. Setup actions use `action.type`; inventory actions use `action.kind`. Do not edit state to mark tasks complete: completion is derived from evidence and hashes. See [contracts](CONTRACTS.md) for the versioned machine interface and [request examples](../examples/inventory/README.md) for the synthetic lifecycle.

`status --json` and no-request `env inventory --json` return an `InventoryStatus` directly and do not create state. An inventory action request instead returns `{schema_version, state, status, action}`: `state` is the selected state-file path string, `status` is the checklist, and `action` contains the emitted scout/relay plan or action-specific result and can be `null`. A request can complete with exit 0 while a parser fetch or relay remains incomplete. Inspect `action`, `status.discovery_runs`, `status.parser_runs`, failures, and `full_map_complete`; exit 0 means command completion, not completed discovery. Inventory input/global failures use exit 2. Direct status also exits 0 for a valid pending checklist.

`--summary` requires `--json` and opts into a compact response; existing calls keep their full keys and nesting. Compact status contains completion flags, mode, target identity, progress, counts, source selectors with field counts, active pack and next actions. It omits parser declarations, field histories and run histories. Action responses retain `{schema_version, state, status, action}` with `summary: true`; a relay action reports `pending_probe_count` instead of probe bodies. Submit `discovery-step` with `answers: []` without `--summary` when you need the exact executable probe arguments. Scout arguments remain exact in both shapes.

Inventory state version 2 adds discovery settings, unverified leads and saved-query observations. Version-1 state is readable with Full mode and empty new ledgers. The next successful action writes version 2 atomically; reading status does not rewrite it. Reconciliation must be renewed after migration. Native pack format 1 remains readable and unchanged except for optional source depth metadata.

Status reads only local state and its referenced packs; it does not authenticate, probe clients, call Falcon, or renew evidence. A `status --request FILE` may select its state through the strict status request; do not also select `--state` when the request contains a state path.

## Full inventory steps

1. **Scout visible telemetry.** In Full mode, `prepare-scout` emits exact query bytes, `repository: search-all`, and absolute UTC start/end, regardless of `environment.view`. Execute only with explicit authorization for **search-all** and that window. Quick mode uses an explicitly selected `environment.view`; the guided flow requires either a chosen-view name or an explicit `search-all` selection, plus a host time budget. If authorization is restricted to another view, use Quick or confirmed selected sources with bounded discovery on that view. Do not edit emitted scout arguments. `import-scout` records the entire unedited result, exact arguments, retrieval date, and hash; selectors/counts seed the source list, subject to the completeness limits below.
2. **Confirm the target.** Confirm CID/cloud/view, full-environment or selected-source scope, context, and additional expected sources. Use `review-source` to explain each source's purpose and data model. Expected but quiet or inaccessible sources remain on the checklist. Record known omissions and access boundaries rather than dropping them silently.
3. **Inventory parsers.** `fetch-parsers` uses a separate protected API client with explicit page/parser/time limits. Alternatively, `import-parsers` accepts saved native catalog pages and exact-ID definitions. Hashes, dates, and definitions are retained. Caps, failures, or missing details leave work pending.
4. **Review and bind.** `bind-parser` associates a source with an exact parser ID and snapshot hash, supported by evidence. Model-reviewed semantics record a line, interpretation, and evidence. Review dynamic/conditional logic. Native sources can use `parser-not-applicable` with a supported reason. Every catalog entry must be bound or explicitly excluded with a reason. Names alone never establish a binding.
5. **Measure sources and fields.** `prepare-discovery` emits a finite MCP relay run for selected sources and absolute windows, within query/time budgets. Run only its emitted probes. `discovery-step` imports complete saved answers by probe ID. Parser declarations guide fields to measure, including fields absent from event samples.
6. **Reconcile.** Resolve remaining items and use `reconcile`. Changed evidence invalidates the reconciliation fingerprint. Recheck status before treating the map as complete.

## Action inputs and evidence boundaries

All requests use `schema_version: 1` and `action.kind`; unknown fields fail. Dates are absolute timestamps; search `start` and `end` must be valid absolute UTC windows with start before end and a window of at most seven days. Use exact IDs from status, never names reconstructed by the assistant. A source selector requires nonempty `repo` or `type` (or both), and no event selector; tagged and ordinary field namespaces remain distinct.

| Action | Required inputs and effect |
| --- | --- |
| `init` | No action fields. Creates a new inventory; refuses an existing state. Resume with other actions instead. |
| `set-discovery-mode` | `mode` (`full` or `quick`), optional `quick_depth` (`sources-only` or default `sources-and-fields`). Saves the preference without starting searches. |
| `import-query-observations` | Protected `query_file`, complete `result_file`, exact `environment`, `source`, UTC `start`/`end`, `retrieved_at`, and `caller_assertion: "exact-query-result-target-window"`. Stores a separate field observation ledger and protected audit summary; never changes the pack. |
| `prepare-scout` | `environment`, `start`, `end`; Quick additionally requires `max_seconds` (`1..600`). Saves one exact census query and emits MCP arguments plus a separate host-enforced budget. No search is executed. Full requests may omit `max_seconds`. |
| `import-scout` | `result_file`, exact emitted `arguments` (`repository`, `query_string`, `start`, `end`), `retrieved_at`. Retains complete result bytes/hash and seeds selectors/counts. Arguments must match the prepared plan. |
| `confirm-target` | `environment`, `scope` (`full-environment` or `selected-sources`), `context`, `expected_sources`. Confirms the identity and intended sources. Selected scope requires explicit expected sources. |
| `review-source` | `source_id`, `purpose`, `data_model`, `reviewed_at`, `evidence`. Records the operator/assistant's source interpretation; does not measure telemetry. |
| `fetch-parsers` | Protected `credentials_file`, `max_pages`, `max_parsers`, `max_seconds`. Uses native parser API reads and records successful captures and run limits/failures. |
| `import-parsers` | `pages` containing `offset`, `limit`, `result_file`; `details` containing exact `parser_id`, `result_file`; and `retrieved_at`. Replaces the saved catalog/definitions with validated native captures. |
| `bind-parser` | `source_id`, `parser_id`, `parser_sha256`, `reviewed_at`, `evidence`, `semantics`. Binds an exact saved snapshot. Each semantic entry contains `line`, `interpretation`, `evidence`. |
| `parser-not-applicable` | `source_id`, `reason` (`falcon-sensor-native` or `platform-internal`), `reviewed_at`, `evidence`. Explicitly classifies a supported native source; does not skip source review or measurements. |
| `exclude-parser` | `parser_id`, `parser_sha256`, `reviewed_at`, `reason`. Classifies an exact catalog snapshot outside the target. A bound parser cannot also be excluded. Binding removes its prior exclusion. |
| `prepare-discovery` | `start`, `end`, `max_queries`, `max_seconds`; optional `source_ids`, `value_fields`, `kind_fields`, `population_fields` arrays. Creates a finite relay run for confirmed expected sources. Empty/omitted `source_ids` selects all expected sources. |
| `discovery-step` | Optional `answers` containing exact `probe_id` and saved `result_file`. Advances the latest inventory discovery run; empty answers resumes without adding a result. |
| `import-pack` | `pack_file`. Requires a valid pack matching the confirmed environment, adds compatible history and activates a retained snapshot. Registers valid selectors as visible, unconfirmed sources; missing workflow history stays unknown. |
| `reconcile` | `reviewed_at`. Saves the current evidence fingerprint; it does not mark pending work done. |
| `record-failure` | `stage`, nullable `source_id`, `message`, `recorded_at`. Appends an explicit local failure; a supplied source ID must exist. |

An `environment` identifies cloud, account and searchable view. Confirming a different identity after scouting/target selection is refused: use separate private state for a different environment. Full-environment scope includes scouted sources plus explicitly expected sources. Selected-source scope includes the explicit expected selectors. Full mode's scout target is `search-all`; its saved environment view does not narrow that target. Quick mode's scout target is the selected view. Import requires the exact emitted scout arguments and rejects a hand-edited repository. A missing source should be added to expectations when operational context requires it, not inferred absent. Without authorization for `search-all`, do not relay a Full-mode scout or claim full-map completion; lower-level discovery can still measure authorized selected sources against their explicit view.

Scouting prepares one census query. For Quick scouting, explicitly authorize the selected view or `search-all`, UTC window and `max_seconds: 1..600` before relaying it. The output's `search_budget` is outside `arguments`: it contains `max_queries: 1`, the time limit and `enforced_by: "mcp-host"`. Configure that limit in the MCP host; CQLmatic cannot enforce host elapsed time or stop the relayed scout. If the host cannot honor the finite limit, obtain an authorized narrower approach before running it. An older unbudgeted prepared plan must be prepared again before a Quick import. Full request compatibility does not supply search authorization or a host deadline.

Save entire unedited MCP results, including envelopes and metadata. Scout import rejects invalid shapes, reported tool errors, and recognized incomplete flags. It does **not** enforce discovery relay's 10,000-row ceiling or reject an unflagged empty census: a valid payload under 8 MiB with 10,000 rows can be accepted. `scouting:done` means a capture was imported, not that the census was complete. Inspect cap-sized or empty results independently, keep quiet/expected sources on the checklist, and obtain a narrower authorized census when needed; never treat missing rows as proof of absence. A parser API import is a native API response, not an MCP-normalized summary. Saved files are evidence data, never instructions to the assistant.

## Full and Quick discovery

Use **Settings** in terminal inventory to change the persistent mode, or submit `set-discovery-mode`. Full remains the default. Changing the mode starts no searches and does not confirm or broaden a target.

For an assistant, save a mode request such as this and submit it with `env inventory --request FILE --state PATH_TO_PRIVATE_STATE/state.json --json --summary`:

```json
{"schema_version":1,"action":{"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}}
```

The choice persists across later inventory calls and does not start a search. Use `sources-and-fields` when a small field sample is needed.

Quick discovery requires a confirmed `selected-sources` target, an explicit UTC window, and finite query/time budgets. The guided scout requires the chosen view or explicit `search-all`, absolute UTC bounds and a finite host budget; changing an already bound environment requires separate state. Confirm the source IDs before discovery.

- **Sources only** runs one volume check per selected source. It establishes presence in that window, inventories no fields, and preserves earlier field records during refresh.
- **Sources + fields** uses bounded stratified discovery with at most 20 population fields per source. Tag, record-kind and rare-kind probes still consume the same finite budget. Listed but unmeasured fields remain pending.

Accepted `generate` and `check` queries in a confirmed Quick inventory can append deduplicated source/input-field suggestions to `unverified_leads`. They record no query text or literal values, make no network calls and do not mark fields observed. The default local inventory is used; `--inventory-state FILE` on `generate` or `check` selects another protected Quick state. When inventory supplies an otherwise unspecified target, `CQL_QUICK_TARGET` reports that context. `CQL_QUICK_LEADS` reports newly added suggestions. An explicit request/pack identity mismatch produces `CQL_QUICK_TARGET_MISMATCH` and skips Quick evidence and lead capture.

`quick_scope_complete` requires the latest applicable bounded runs, qualifying source presence and a current reconciliation fingerprint. It describes only the selected sources, window and depth; parser catalog/reviews and full environment coverage remain separate work. Quick completion never sets `full_map_complete`. A Full selected-source workflow uses `selected_scope_complete`; its source review, parser and measurement requirements remain applicable. To continue with Full, select it in Settings or submit `{"schema_version":1,"action":{"kind":"set-discovery-mode","mode":"full"}}`. Existing packs, source records, parser snapshots, leads and saved-query observations are retained. Full requirements and reconciliation are reevaluated, so work remains pending when only Quick evidence exists.

## Parser catalog, declarations, and review

Parser catalog completion requires contiguous pages beginning at offset 0, no duplicate IDs, consistent reported totals when present, a reported total matching the number of distinct IDs, and an exact-ID detail for every catalog ID. A page's limit is 1–100 and offset at most 9999; a reported response offset must match the request. Missing totals, gaps, or missing definitions leave completion pending. Changing totals or duplicate IDs fail validation rather than establishing a complete catalog.

Each detail must have one native `resources` entry with `id` matching the explicitly requested catalog ID and a string `script`. Native error responses and unsupported shapes cannot become a reviewed definition. `fields_to_tag`, when present, is an array of field names. Imports accept at most 100 pages and 10,000 details, with an 8 MiB limit per capture. Fetch budgets are 1–100 pages, 1–10,000 parsers, and 1–900 seconds. Successful captures survive a later fetch deadline or transport failure; inspect the recorded run and repeat a bounded inventory to fill gaps. Import/fetch replaces the saved parser inventory, so supply the complete intended capture set rather than assuming a later partial import appends to it.

Declaration extraction is conservative lexical analysis, not execution or full parser interpretation. It identifies supported assignment targets, tagged output names, and array paths. Calls, braces, case/match/arrow syntax, and lines without recognized assignments are conservatively flagged in `status.parsers[].review_lines`; this is not an exhaustive interpretation of dynamic behavior. Read the actual saved script and supply a distinct semantic review for each listed 1-based line. Reviews explain interpretation and supporting evidence; they cannot assert telemetry presence. Missing line reviews leave parser status `REVIEW`.

Bindings and exclusions name the exact snapshot hash. A changed/missing parser snapshot clears obsolete bindings and exclusions; rebind and review the new bytes. Names alone do not establish which parser processes a source. Native `N/A` classification is restricted to the two supported reasons and requires evidence. It is not a general escape for missing API permissions or an unknown parser. Classify every catalog parser as bound or explicitly excluded, including catalog entries outside selected-source scope.

## Read the checklist

Status is offline: it performs no Falcon authentication or searches. It reports dates, source coverage, field counts, parser declarations, record kinds, unfinished runs, failures, and next actions. Human output uses the labels below; JSON serializes them as `done`, `pending`, `review`, `unknown`, and `n-a`. Read each source's `interpretation`, `parser`, `measurements`, and `observation` separately from its aggregate `status`.

- `DONE`: the applicable evidence/review requirement is satisfied.
- `PENDING`: known work remains, such as a detail, field batch, or quiet source measurement.
- `REVIEW`: an interpretation, purpose, or binding needs confirmation.
- `UNKNOWN`: evidence or workflow history is unavailable.
- `N/A`: an explicit supported classification makes a source-specific task inapplicable.

`full_map_complete` requires a confirmed `full-environment` target, an active pack, saved scouting, complete parser catalog and classification, at least one source, all known selectors confirmed, every expected source `DONE`, and a current reconciliation fingerprint. Each source needs purpose/data-model review, complete exact-snapshot parser review or supported `N/A`, qualifying observed source evidence, and completed measurements. A quiet or inaccessible expected source therefore prevents a full-map completion claim. Selected-source work cannot set `full_map_complete`, even when `selected_scope_complete` or `quick_scope_complete` is true. Completion describes stored evidence and the confirmed visibility; it cannot rule out unknown sources beyond that visibility.

`reconcile` can record a fingerprint before the other requirements are complete, so `reconciliation: done` alone is insufficient. Any evidence/state change covered by that fingerprint makes it pending again. Old successful discovery runs and retained snapshots remain history; current status uses the cumulative active pack when present, and the latest run involving each source must be complete with a retained run snapshot for its measurements to be done. A newly incomplete run can therefore reopen source work even when an earlier run completed.

Complete discovery and parser inventory for the confirmed target, then resolve the quiet, inaccessible, or unfinished sources on the checklist. Unknown sources outside that target or your account's visibility cannot be ruled out.

## Field evidence and batches

**Declared** means a parser can output a field. **Observed** means qualifying telemetry supports it in a stated source/window. **Measured** includes explicit zero presence; zero does not turn a declaration into an observation. Previously observed but not reobserved fields retain historical evidence.

Explicit `population_fields` batches contain `{source, fields}` objects bound to exact repository/type selectors. A declared field absent from the fieldset sample can be measured over the full run window. Sample and full-window counts keep distinct provenance. Tagged `#field` and ordinary `field` remain distinct.

Subset refreshes preserve untargeted evidence. Selected fields not reobserved become historical; default full-source refresh retains its established behavior. Multiple batches build coverage without promoting unmeasured declarations. Caps leave remaining fields on the checklist. Include required source/kind facet fields within explicit selections and budgets.

`value_fields` requests bounded observed value samples; `kind_fields` adds candidate fields for record-kind facets. `population_fields` selects 1–4000 distinct fields per exact source selector and must refer to sources in the run; duplicate source selections or fields are rejected. Inventory exposes query limits of 1–1000 and wall-clock limits of 1–3600 seconds. Review emitted probe stages and available budget before executing them. Field population probes are chunked, so one source can require multiple queries; broad scans remain costly even with small row caps. See [MCP relay](MCP-COMPANION.md) and [environment discovery](ENVIRONMENT.md) for lower-level controls.

An explicit full-window zero has measurement provenance but `observed: false`; it does not mean the parser cannot produce the field in another window. A fieldset sample alone is insufficient for status to promote qualifying field presence. `declared`, `measured`, `observed`, `not_reobserved`, and evidence dates must be read separately. A declared but unobserved field can remain `REVIEW` even after zero measurement satisfies its measurement requirement. Current zero/not-reobserved evidence is not overwritten by an older successful snapshot.

## Cost, recovery, and freshness

CQLmatic makes no model calls. Assistant orchestration, tool answers, parser interpretation, and refinements consume model tokens. Broad environments can need many runs. No universal ETA or dollar estimate has been measured. Narrow source/time scope controls scan cost; row caps alone do not.

Review query/page/parser/time limits before starting. Progress, receipts, and snapshots are saved locally. Resume after fixing a permission or input problem. Discovery runs have wall-clock deadlines; start a new bounded run for unfinished work after expiry. Prior successful packs remain usable. Timeouts, access failures, truncation, and exhausted budgets remain pending/inconclusive rather than evidence of absence.

Revisit snapshots after ingestion, access, or parser changes. New parser hashes require fresh binding/semantic review. CQLmatic does not assume dated evidence stays current forever.

## Imports and query writing

`import-pack` registers an existing native or previously converted pack. Valid repository/type selectors appear in status with `expected: false`, without inferred source review, parser binding or scouting counts. Confirm the intended selectors explicitly before measuring them. Legacy observations can ground covered queries while missing workflow history remains unknown.

Reimporting identical records is idempotent. Disjoint compatible history is additive regardless of snapshot date. Different data under the same record ID, or incompatible selectors/references, rejects the import before state or active evidence changes. Imports preserve existing field/value/statistical records; they do not apply refresh semantics or invent discovery completion. Use [legacy import instructions](ENVIRONMENT.md) to convert supported JSONL first. Never hand-edit generated upstream references.

Inventory activates a cumulative pack across successful source/field batches and retains earlier snapshots. Status reports its `active_pack.path` and digest. Use that path with `generate --pack` or `check --pack` to select its measurements explicitly. Selected inventory evidence can also read the active pack for identity, contradiction and record-kind safeguards. For lower-level explicit refresh, discovery accepts an existing pack destination; see [environment details](ENVIRONMENT.md). The assistant should inspect status and choose evidence appropriate to each query. `covered_queries_usable` is an allowance to use applicable covered evidence during incomplete inventory, not a per-query proof: it can be true on a blank canvas without an active pack. Confirm the active pack exists, passes digest/identity validation, and actually covers the requested sources/fields/window. A referenced pack that is missing or has changed digest makes status fail; import a new valid snapshot explicitly instead of editing stored references. Follow [query writing](QUERY-REFERENCE.md) and [saved-result assessment](RESULT-ASSESSMENT.md) after selecting evidence.

## Saved-query observations and unverified leads

An explicitly imported ordinary query result can record positive field presence separately from the environment pack. Supply the exact UTF-8 query file and entire unedited MCP response, plus its source, target, window, retrieval time and `caller_assertion: "exact-query-result-target-window"`. This assertion belongs to the caller; the response alone does not attest which query or target produced it.

After confirming the exact environment and expected source, an illustrative saved-query action is:

```json
{
  "schema_version": 1,
  "action": {
    "kind": "import-query-observations",
    "query_file": ".local/private-query/query.cql",
    "result_file": ".local/private-query/result.json",
    "environment": {
      "cloud": "us1",
      "tenant": "00000000000000000000000000000000",
      "view": "synthetic_view"
    },
    "source": {"type": "synthetic_type"},
    "start": "2026-01-01T00:00:00Z",
    "end": "2026-01-01T00:15:00Z",
    "retrieved_at": "2026-01-01T00:16:00Z",
    "caller_assertion": "exact-query-result-target-window"
  }
}
```

Submit it through `env inventory --request FILE --state PATH_TO_PRIVATE_STATE/state.json --json --summary`. This synthetic identity and window are examples, not execution settings. The referenced originals must be protected files under private ancestry. The saved query must explicitly constrain the attested source; the selected target must match the confirmed inventory. Retrieval must follow the stated completed window.

Only accepted direct event pipelines qualify: exact conjunctive source selectors, direct data filters, unchanged `select` projections, and `head`, `tail` or `sort` row selection. Derived fields, aliases, aggregates, joins and synthetic pipelines fail closed. Empty, failed, incomplete, malformed or cap-sized responses are rejected before counts. Present nonnull, nonempty values count as positive; numeric `0` and Boolean `false` count. An observation does not establish field population rates, values, global availability or complete discovery.

Both originals and their ancestry must pass private-file checks before reading. The protected audit record retains the exact file references, SHA-256 hashes, caller assertion, source/target/window and positive field counts. Raw query/response bytes and sampled values are not copied into state or the audit record. Keep the original protected files available for verification. Imports do not silently rewrite the active pack, source reviews or parser bindings.

Full status exposes `unverified_leads` and caller-attested `query_observations`, including exact source/window/time and per-field positive row counts. Compact status exposes their counts only. These additive evidence lists contain no original query text, literal values or raw result rows.

A selected confirmed inventory's validated caller-attested ledger can supply qualifying fields to `generate`/`check --require-observed` without a pack. Quick mode can supply missing target context. In Full mode, an explicit request `context.environment` or selected `--pack` must match the confirmed identity exactly; Full mode never fills an absent target or captures new query leads. `--inventory-state FILE` selects the inventory for either mode. Other fields still need evidence, and later or unordered contradictory pack measurements and record-kind safeguards remain applicable. Generation/checking do not choose an execution window; inspect the stored observation window for the intended search. A positive field count does not establish array-member shape.

The field ledger and unverified-lead list are each bounded to 1000 entries. Leads are deduplicated by exact source and field; they remain suggestions, with no observed/declared status or completion effect. Query observations retain exact field namespaces and at most 4000 fields per record. Later bounded discovery remains necessary for unresolved evidence and broader coverage.

See [setup](SETUP.md), [MCP relay details](MCP-COMPANION.md), [application flow](FLOW.md), and [troubleshooting](TROUBLESHOOTING.md).
