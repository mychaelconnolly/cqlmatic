# Environment discovery and status

Basic `generate` and `check` work offline without a pack or completed inventory. The environment map adds scoped evidence for choosing and grounding telemetry. The assistant interprets source purpose and parser transformations; CQLmatic stores evidence, constructs measurements, tracks gaps, and grounds query fields. A complete map combines telemetry, parser inventory, and confirmed operational context.

See [hosted search outcomes](MCP-COMPANION.md#how-to-tell-whether-a-hosted-search-finished) for completion and error shapes, retry guidance and the positive control required for zero results. For refresh and capability settlement, use `not-started` for a recognized start rejection and `timed-out` for a hosted cutoff. Keep lost responses, client waits without a tool answer, transport errors and unrecognized errors `unresolved`. A recognized poll failure from the hosted search tool shows the search had started by the time the answer arrived. Use that arrival time for `search_started_at` and submit the exact bound `lifetime-expired` settlement with `recorded_at` at least the retained provider lifetime later. Record the saved answer's SHA-256 and trace ID in `evidence`. For capability, also supply the saved answer as `result_file`. The refresh imports no rows and the capability case stays `unknown`.

Full discovery is the default. Initial full environment discovery is extensive. It examines your selected sources through multiple searches and may take substantial time. Agent-assisted discovery can also consume significant model tokens. Review the scope and limits before starting. Completed discovery is saved and reused for future query writing.

## Start and resume

```sh
cqlmatic env inventory
cqlmatic status
cqlmatic status --json
cqlmatic status --json --brief
cqlmatic env inventory --json --brief
```

Start assistant work with `status --json --brief`; obtain focused details only when its fixed flags and counts show a relevant gap. Use `--state PATH_TO_PRIVATE_STATE/state.json` on both commands for separate environments. Terminal inventory provides guided actions; your assistant can use `--request FILE --json`. Without a terminal or request, inventory reports status and exits without starting a search.

New discovery checkpoints created through inventory record their owning inventory state. Use inventory's `next-probe`, `dispatch-discovery` and `resume-discovery` actions for these bound runs so outstanding searches in other inventory phases remain guarded. Standalone `env discover --mcp --dispatch` and `--resume` reject bound checkpoints; pause, answer submission and cancellation settlement remain available. Earlier releases can leave unowned inventory checkpoints; their recovery path is described below.

The discovery actions `next-probe`, `resume-discovery`, `dispatch-discovery`, `discovery-step` and `pause-discovery` accept an optional zero-based `run_index`, the position in the saved state's `discovery_runs` array. Omission selects the latest run. Full, summary and brief action replies retain the resolved index; use it in subsequent requests for the same run. The request `{"schema_version":1,"action":{"kind":"resume-discovery","run_index":0}}` resumes the first retained run through the inventory guards.

Inventory resume and dispatch require the run's verified starting pack to match the current cumulative inventory. If another run or pack import has changed it, prepare a new bounded run from the current inventory; the old run's captures and allowance stay retained. Pause, polling, answer submission and exact cancellation settlement remain available. Valid interrupted activation can still recover its frozen artifacts. If the inventory changes during an attempt, a verified completion is recorded as `superseded`: its pack and receipt are retained, the active inventory stays unchanged, and the run grants no pending-work credit. Full and summary actions report `status: "incomplete"` with `DISCOVERY_SUPERSEDED` while the verified relay still reports `complete`; brief reports the fixed `incomplete` outcome. Exit 0 still means the action completed. Repeated polls and resume validate the same artifacts without renewing the allowance or changing the saved outcome.

Retained checkpoints from the running producer gain this association when inventory next prepares or dispatches discovery, capability, refresh or scout work, or resumes discovery or capabilities. The association preserves accepted captures and the original allowance. Before association, an older checkpoint opened directly has the same format as an independent standalone run; ownership cannot be inferred from its filename or package version. Continue current-producer and owner-bound work through inventory. Unowned work from an earlier producer can use [standalone recovery](#recover-an-unowned-earlier-release-run).

Checkpoint inspection supports format 2 from producers 0.1.0–0.1.8 and format 3 from producers 0.1.6–0.1.8, in addition to the running producer's supported formats. Inspection validates every lifecycle status using the same request, capture, artifact and ownership checks. A different supported producer is not evidence of tampering. Unknown, future, malformed or unsupported producer/format combinations remain rejected.

Dispatch, resume, step, pause, settlement, activation and cleanup remain pinned to the checkpoint's exact producing version. Selecting a run from another producer returns `MCP_STATE_PRODUCER` before inventory migration, failure recording or association of sibling checkpoints. That refusal preserves the inventory and every retained checkpoint. Foreign checkpoints are never rewritten to add ownership metadata.

For non-selected retained format-3 runs, all nine guarded discovery, refresh, scout and capability entry points use these rules. Other target, budget and predecessor checks still apply.

| Retained state | Effect on unrelated guarded work |
| --- | --- |
| Any uncertain attempt, including earlier attempts | Block until the exact search is safely settled or the last attempt qualifies for the earlier-release rule below. |
| `needs-probes`, `activation-blocked`, `activating` | Block as unfinished. |
| `cancel-requested`, `cleanup-pending` | Block until termination and cleanup are resolved, subject to the next row. |
| Exhausted 0.1.6 or 0.1.7 MCP run with a verified final answer | Allow unrelated guarded work under the checks below. |
| `paused` | Allow when safely paused; an identical request with the same predecessor must reuse its original allowance. |
| `activated` | Allow after verifying the frozen artifacts. |
| `failed` | Allow only when no attempt remains uncertain. |

An unfinished foreign resumable run without an owner binding to this inventory ordinarily blocks with `MCP_STATE_PRODUCER`, even when safely paused. Its producing executable could otherwise resume it independently. A retained owner binding must match the locked inventory. Format-2 history supplies no resumable semantics and blocks guarded work unless activated.

A verified format-3 MCP checkpoint written by 0.1.6 or 0.1.7 can clear its inventory guard after spending its original active-time allowance. The run must be `cleanup-pending` or `cancel-requested`, with a last `cleanup-pending` attempt that has no terminal evidence, job ID, receipt or confirmed non-start. All earlier attempts must have known outcomes. The last attempt must retain one to sixteen byte-identical rejected results, all carrying a recognized final error from the hosted search tool or a complete result list. An answer already held by an earlier attempt blocks, as does missing, conflicting, unrecognized or invalid evidence. Checkpoints from 0.1.6 must be unowned.

Submitting a preparing action such as `prepare-scout` safely rechecks this evidence without executing a search. On success the inventory saves the run as `failed` and records one failure at that transition, with an info notice naming its run index. A brief reply names the first newly recorded run if several qualify. The checkpoint remains available to its producing release. The inventory rechecks the saved bytes and elapsed time on each guarded action. Outstanding refresh or capability searches and other blocked runs still prevent relief.

Cancellation is established from evidence, not an edited outcome label. MCP cancellations require the exact saved request, probe, query, target, window and dispatch settlement. Older native cancellation or failure records can lack sufficient cleanup proof; they remain readable but uncertain, even if a later attempt finished. Impossible or inconsistent records are rejected.

Association of current-producer checkpoints can still be saved on a refusal unrelated to foreign history. Older builds can reject added owner metadata as unsupported or tampered; that diagnostic alone does not establish file corruption. Package version alone does not establish compatibility between different distributed builds.

### Recover an unowned earlier-release run

Preserve the inventory, checkpoints and their artifacts before upgrading. A producing release can finish a run through inventory only while it can read that inventory and select the required run. Released 0.1.6 reads inventory through version 3 and selects only its latest discovery run. Once an action or an ordinary hint-saving `check` or `generate` writes version 4, that release cannot read the inventory. With multiple paused runs, its inventory actions cannot select an earlier run even while the inventory remains version 3.

For a valid **paused format-3 MCP checkpoint without `session.inventory_owner`**, the producing release can resume the checkpoint directly, independently of the inventory format or run position. Use full inventory output to find the affected `discovery_runs` entry's `state_path`, then read the protected checkpoint's format, transport, status, producer and ownership metadata. Settle outstanding searches across all inventory phases first, and prevent concurrent inventory execution during standalone recovery: these commands bypass inventory's cross-phase search guards. Recover one checkpoint at a time.

Use the executable from the checkpoint's exact producing release. Replace the placeholders below; `--version` must match its `producer_version`. Resume returns the next probe and its exact `mcp.tool` and `mcp.arguments`:

```sh
CQLMATIC_PRODUCER="PATH_TO_PRODUCING_RELEASE/cqlmatic"
"$CQLMATIC_PRODUCER" --version
"$CQLMATIC_PRODUCER" env discover --mcp --state "CHECKPOINT" --resume
"$CQLMATIC_PRODUCER" env discover --mcp --state "CHECKPOINT" --dispatch PROBE_ID
```

`PROBE_ID` comes from that response. Dispatch reserves the attempt; it does not execute the search. Have the authorized MCP host run only the emitted call under the original finite budget and save its entire original JSON result privately as `RESULT_FILE`. Then submit that result:

```sh
"$CQLMATIC_PRODUCER" env discover --mcp --state "CHECKPOINT" --answer "PROBE_ID=RESULT_FILE"
```

Repeat dispatch, execution and answer submission for each next probe until the output reports `complete` with exit 0. Intermediate `needs-probes` output uses exit 3. Successful recovery leaves the checkpoint `activated` and saves its own pack and receipt. Original captures and attempt/active-time allowances remain in force. Uncertain searches need their original workflow's exact settlement first; exhausted budgets or missing proof can prevent completion. Safely pausing alone does not remove the upgrade blocker. See [MCP discovery](MCP-COMPANION.md) for the host and settlement requirements.

The new build then treats that checkpoint as settled retained history on guarded routes, once all other blockers are resolved. **Standalone completion does not update its inventory run, answered count, active pack or pending-work credit.** The inventory entry can still say `paused`; the standalone response's `active_pack_updated` refers to the checkpoint's pack destination. The new build still refuses to step or resume the foreign run through inventory, and reconciliation does not promote its pack automatically.

The recovered measurements are retained in that pack and can be supplied explicitly for covered queries. Importing the pack is possible only when its history is compatible with retained records; conflicting data under the same record ID is rejected. Import does not mark the recovered inventory run complete or restore its workflow credit. A producer-readable inventory can still finalize its latest run through that release when its ordinary predecessor and artifact checks pass; otherwise prepare a new bounded discovery run to establish inventory completion and applicable pending-work credit. Standalone recovery preserves the data but loses automatic inventory integration of that run.

To continue a run with an inventory owner binding, use its owning inventory and the producing release with compatible files, or a tested compatible recovery build. Do not remove the binding or relabel the checkpoint to force recovery. Downgrade recovery is not guaranteed: older builds can reject newer inventory versions or owner metadata, and missing job identity can prevent automatic cleanup. No version change renews the allowance or proves termination. If an earlier-release run cannot be recovered and all outstanding searches have been reconciled, a separate inventory can import a compatible pack as a last resort. Target confirmation, source reviews, parser bindings, pending work and capability evidence stay with the original inventory. An imported pack carries its fields, with run history recorded as unknown and Full completion pending.

```sh
cqlmatic env inventory --state PATH_TO_PRIVATE_STATE/state.json --request examples/inventory/init.request.json --json
cqlmatic --schema inventory
cqlmatic --schema status
```

Examples use synthetic data. Adapt identities, dates, paths, and sources for live work. Setup actions use `action.type`; inventory actions use `action.kind`. Do not edit state to mark tasks complete: completion is derived from evidence and hashes. See [contracts](CONTRACTS.md) for the versioned machine interface and [request examples](../examples/inventory/README.md) for the synthetic lifecycle.

`status --json` and no-request `env inventory --json` return an `InventoryStatus` directly and do not create state. Most inventory action requests return `{schema_version, state, status, action}` with the selected state-file path, checklist and action-specific result. The `next-probe` action returns only `{schema_version, action}`. A request can complete with exit 0 while a parser fetch or relay remains incomplete. Inspect `action`, `status.discovery_runs`, `status.parser_runs`, failures, and `full_map_complete` where present. Inventory input/global failures use exit 2. Direct status also exits 0 for a valid pending checklist.

`--summary` requires `--json` and opts into a compact response. Compact status contains completion flags, mode, target identity, progress, counts, source selectors with field counts, active pack and next actions. It omits parser declarations, field histories and run histories. Except for `next-probe`, action responses retain `{schema_version, state, status, action}` with `summary: true`, and relay output reports `pending_probe_count`. Use `next-probe` or a full `discovery-step` poll with `answers: []` to obtain the exact discovery arguments. Scout arguments remain exact in full and summary output.

### Bounded agent handoff

`--brief` requires `--json` and conflicts with `--summary`. Both `status` and `env inventory` support it, including action responses. Brief projections are at most **4,096 UTF-8 bytes including the envelope and final newline**. The `next-probe` action always emits its single-probe response, whose exact arguments can exceed this limit. Runtime errors use a short fixed diagnostic and exit 2. CLI argument errors and help retain their ordinary behavior. Summary size can grow with inventory size.

The brief includes fixed readiness/completion flags, counts, recorded refresh status, a bounded `refresh_reminder`, Settings attention and at most five total opaque work/attempt references with omission counts. It excludes target/session text, paths, selectors, raw queries/results, parser scripts, freeform explanations and complete lists. References describe unacknowledged revisions; critical unresolved-search flags remain visible even when routine references are omitted. A reserved call is not proof of a running backend search.

A nonempty notification snapshot commits to the entire unresolved, unacknowledged work set and exact environment. After presenting a concise aggregate message, the host can submit `acknowledge-inventory-notice` with that snapshot's SHA-256 and `defer: false`, using the same explicit state. Use `defer: true` only after the user chooses later. The action recomputes the current snapshot under the writer; concurrent additions, revisions, resolutions or another acknowledgement reject a stale snapshot without partial changes. Empty snapshots cannot be acknowledged. Existing `acknowledge-inventory-work` remains available for exact individual revisions.

The snapshot is a content commitment, not authorization or a globally unique workflow ID; identical cloned states may share it. The response identifies the acknowledged digest/count separately from the remaining snapshot. Acknowledgement suppresses notification repetition only: it does not inventory data, resolve work, confer search permission or hide unresolved-search safety flags. A lost response can require rereading brief status rather than repeating an acknowledgement blindly.

Generation/checking persist new hints but do not acknowledge notices, even when their output succeeds. Background imports and status reads do not acknowledge delivery. The foreground host presents the message before acknowledging the exact snapshot. Until then a reminder may recur; exactly-once delivery is not guaranteed. Settings attention remains until applicable inventory or explicit exclusion resolves the work.

Use brief mode for routine action responses as well as initial status so acknowledgement and refresh bookkeeping cannot return a large inventory to the model. Use the exact arguments in `next-probe` output for its one search. For other actions, obtain exact arguments from the protected full response and consume it locally. Do not reconstruct arguments from abbreviated output or automatically page through all omitted details.

Inventory state version **4** adds separate optional capability history. Version-1–3 states remain readable; a successful state-writing action migrates them atomically while retaining their field evidence and original reconciliation basis. Optional producer-managed `field_evidence_schema_version` preserves that basis; capability history does not change the field-evidence fingerprint or complete field discovery. Legacy states cannot attest capability runs. Earlier binaries may reject newly written version-4 state; preserve the original before moving between builds. Request/status contracts remain version **1**, and native pack format remains **1**. Reading status does not migrate or change state; no-op replays preserve current-version timestamps and reconciliation.

Continuing refresh settings, attempts, source-type metadata and pending inventory work remain intact. New refresh settings default to `automatic: false` with a 24-hour lookback and empty ledgers. Saved automatic preferences cannot enable automatic refresh. The environment-bound reminder ledger is initialized by target confirmation; legacy states use the existing confirmation date when needed. Accepted legacy packs preserve their original profile, knowledge pin and digest; migration does not relabel them as core-v3 or upgrade their evidence. See [compatibility](COVERAGE.md#compatibility-and-exclusions).

Status reads only local state and its referenced packs; it does not authenticate, probe clients, call Falcon, or renew evidence. A `status --request FILE` may select its state through the strict status request; do not also select `--state` when the request contains a state path.

## Full inventory steps

1. **Scout visible telemetry.** In Full mode, `prepare-scout` emits exact query bytes, `repository: search-all`, and absolute UTC start/end, regardless of `environment.view`. Execute only with explicit authorization for **search-all** and that window. Quick mode uses an explicitly selected `environment.view`; the guided flow requires either a chosen-view name or an explicit `search-all` selection, plus a host time budget. If authorization is restricted to another view, use Quick or confirmed selected sources with bounded discovery on that view. Do not edit emitted scout arguments. `import-scout` records the entire unedited result, exact arguments, retrieval date, and hash; selectors/counts seed the source list, subject to the completeness limits below.
2. **Confirm the target.** Confirm CID/cloud/view, full-environment or selected-source scope, context, and additional expected sources. Use `review-source` to explain each source's purpose and data model. Expected but quiet or inaccessible sources remain on the checklist. Record known omissions and access boundaries rather than dropping them silently.
3. **Inventory parsers.** `fetch-parsers` uses a separate protected API client with explicit page/parser/time limits. Alternatively, `import-parsers` accepts saved native catalog pages and exact-ID definitions. Hashes, dates, and definitions are retained. Caps, failures, or missing details leave work pending.
4. **Review and bind.** `bind-parser` associates a source with an exact parser ID and snapshot hash, supported by evidence. Model-reviewed semantics record a line, interpretation, and evidence. Review dynamic/conditional logic. Native sources can use `parser-not-applicable` with a supported reason. Every catalog entry must be bound or explicitly excluded with a reason. Names alone never establish a binding.
5. **Measure sources and fields.** `prepare-discovery` creates a finite MCP relay run for selected sources and absolute windows, within query/time budgets. Use `next-probe` to relay its measurements one at a time. Parser declarations guide fields to measure, including fields absent from event samples.
6. **Reconcile.** Resolve remaining items and use `reconcile`. Changed evidence invalidates the reconciliation fingerprint. Recheck status before treating the map as complete.

### Relay one probe at a time

After `prepare-discovery`, send `{"schema_version":1,"action":{"kind":"next-probe"}}` to `cqlmatic env inventory --request - --json` with the same state selection. When `action.state` is `probe`, run `action.probe.tool` once with its exact `arguments` and save the whole unedited result. Submit `next-probe` again with the returned `run_index` and `answer` containing the issued `probe_id` and saved `result_file`. Repeat until `complete`, then submit `reconcile`. Other run states return no probe.

A probe with `dispatched: false` is the existing reservation. Read it again without an answer if its descriptor was lost. If the host already made the call, recover its result or settle that exact attempt before making another call. A refused answer returns fixed diagnostic text and leaves the details in protected storage.

## Optional function capability checks

After a field pack is active, terminal inventory offers **capabilities (optional)**. It is off by default: empty input or **later** prepares no run and preserves the field map. Capability observations have a separate ledger and finite attempt/active-time allowance; they do not change field-discovery completion or its reconciliation fingerprint. Preparation and reservation make no MCP call. The authorized host executes each exact reserved query once, retains the full response when available, and submits an exact-attempt settlement. Provider-enforced search-lifetime evidence is required; a client wait timeout is insufficient.

The fixed cases are `array-union`, `array-intersection`, `array-reduce-row`, `array-reduce-column`, and `math-sqrt`. Their observations qualify only the implemented argument shapes, including `sum(var)` for the reductions. Matching also requires the exact environment/CID/view, caller-attested authorization context, current case revision and resource binding. Positive observations expire exactly 30 days after result retrieval; a newer unknown attempt supersedes an older positive. Errors, timeouts, unexpected output and unresolved termination remain unknown. These cases do not verify all function forms, beta availability or historical no-op behavior. See [exact CLI actions](../USAGE.md#optional-function-capability-checks) and [coverage limits](COVERAGE.md#optional-target-capability-checks).

Full and summary status expose `capabilities` and `capability_blocked`; brief status reports `capability_run_count` and `capability_blocked` without case payloads. A retained `verified` case outcome is historical; generation/checking reevaluate current eligibility with `context.execution_context_id` or the overriding `--execution-context-id`. The CID alone cannot supply that authorization context. Unresolved termination blocks later inventory searches while the committed field pack remains usable for offline query writing.

## Action inputs and evidence boundaries

All requests use `schema_version: 1` and `action.kind`; unknown fields fail. Dates are absolute timestamps; search `start` and `end` must be valid absolute UTC windows with start before end and a window of at most seven days. Use exact IDs from status, never names reconstructed by the assistant. A source selector requires nonempty `repo` or `type` (or both), and no event selector; tagged and ordinary field namespaces remain distinct.

| Action | Required inputs and effect |
| --- | --- |
| `init` | No action fields. Creates a new inventory; refuses an existing state. Resume with other actions instead. |
| `set-discovery-mode` | `mode` (`full` or `quick`), optional `quick_depth` (`sources-only` or default `sources-and-fields`). Saves the preference without starting searches. |
| `set-refresh-settings` | `automatic` and `lookback` (`24h`, `7d`, `14d`, `30d`). Saves settings independently of discovery mode. Automatic refresh is unavailable; attempts to enable it fail. Manual refresh and encountered-source notices remain available. |
| `prepare-refresh` | Exact host `session_id`, protected `host_evidence_file`, and optional `automatic` (default false). Retains a shallow manual census attempt for the confirmed view. Automatic requests fail regardless of saved preferences. Preparation neither dispatches nor executes a search. |
| `dispatch-refresh` | Exact `attempt_id` and `session_id`. Durably reserves one manual dispatch before emitting the exact MCP arguments and retained provider search-lifetime budget (1–600 seconds). New automatic dispatches are unavailable, including retained automatic preparations. Never retry the dispatch after a lost response. |
| `settle-refresh` | Exact `attempt_id` and protected `evidence_file` containing an exact-attempt host settlement. Uncertain termination keeps the attempt unresolved and blocks another attempt. |
| `import-refresh` | Exact `attempt_id`, protected complete `result_file`, and actual UTC `retrieved_at`. Requires a completed settlement. Retains original bytes and queues new selectors or metadata without updating the pack or completing discovery. |
| `acknowledge-refresh-reminder` | Exact `cycle_sha256` from `refresh_reminder`, plus optional `defer` (default false). Acknowledges a displayed due reminder; `defer: true` postpones it for 30 days after the user chooses later. Stale cycles fail atomically. Neither choice completes a refresh. |
| `record-inventory-hints` | Exact confirmed `environment` and `hints` containing `source` selectors and optional `fields`. Stores bounded unverified suggestions; it never establishes observed fields or parser bindings. |
| `acknowledge-inventory-work` | `work` entries containing exact `id` and `revision`, plus optional `defer`. Acknowledges that revision's notice; deferred work remains visible in Settings. |
| `acknowledge-inventory-notice` | Exact nonempty `snapshot_sha256` from brief status, plus optional `defer`. Atomically acknowledges the entire still-matching notification snapshot without sending every revision back through the conversation. |
| `exclude-inventory-work` | Exact work revisions, a nonempty `reason` and `reviewed_at`. Explicitly resolves the identified pending revision without claiming inventory coverage. |
| `import-query-observations` | Protected `query_file`, complete `result_file`, exact `environment`, `source`, UTC `start`/`end`, actual UTC `retrieved_at`, and `caller_assertion: "exact-query-result-target-window"`. Stores a separate field observation ledger and protected audit summary. Returns `uncredited_fields` and guidance in `next_action` when fields lack positive returned values. |
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
| `discovery-step` | Optional `run_index` and `answers` containing exact `probe_id` and saved `result_file`. Advances the selected inventory discovery run; empty answers polls without adding a result or resuming a paused run. |
| `dispatch-discovery` | Exact emitted `probe_id`; optional `run_index`. New resumable runs require a durable dispatch before the host executes each probe and submits its saved answer. |
| `next-probe` | Optional `run_index` and single `answer` with the issued `probe_id` and saved `result_file`. Settles the answer, then reserves the first ready probe. Without an answer it can recover the outstanding reservation. |
| `pause-discovery` | Optional `run_index`. Requests cancellation/pause of the selected resumable run. Retains accepted captures; an in-flight host search must be reconciled before another dispatch. |
| `resume-discovery` | Optional `run_index`. Continues a safely paused resumable run with its accepted captures and remaining budgets. It does not repeat completed probes or start a fresh budget. |
| `prepare-capabilities` | `execution_context_id`, `session_id`, distinct nonempty `case_ids`, protected `host_evidence_file`, `max_queries`, `max_seconds`. Requires an active field pack and retains a separate fixed-case allowance without executing a search. |
| `dispatch-capability` | Exact `run_id`, `case_id`, `execution_context_id`, `session_id`. Reserves one attempt and returns its exact MCP arguments; the host executes it once. |
| `submit-capability` | Exact `run_id`, `attempt_id`, `execution_context_id`, protected `settlement_file`, UTC `retrieved_at`; optional protected `result_file`, required for a completed attempt. Imports the exact-attempt settlement and entire matching capture when available. Unresolved termination remains blocked. |
| `pause-capabilities` | Exact `run_id`. Safely settled work pauses; unresolved termination retains its active-time charge and blocks later inventory dispatch. |
| `resume-capabilities` | Exact `run_id`, `execution_context_id`. Resumes the retained run with its original remaining allowance. |
| `import-pack` | `pack_file`. Requires a valid pack matching the confirmed environment, adds compatible history and activates a retained snapshot. Registers valid selectors as visible, unconfirmed sources; missing workflow history stays unknown. |
| `reconcile` | `reviewed_at`. Saves the current evidence fingerprint; it does not mark pending work done. |
| `record-failure` | `stage`, nullable `source_id`, `message`, `recorded_at`. Appends an explicit local failure; a supplied source ID must exist. |

An `environment` identifies cloud, account and searchable view. Confirming a different identity after scouting/target selection is refused: use separate private state for a different environment. Full-environment scope includes scouted sources plus explicitly expected sources. Selected-source scope includes the explicit expected selectors. Full mode's scout target is `search-all`; its saved environment view does not narrow that target. Quick mode's scout target is the selected view. Import requires the exact emitted scout arguments and rejects a hand-edited repository. A missing source should be added to expectations when operational context requires it, not inferred absent. Without authorization for `search-all`, do not relay a Full-mode scout or claim full-map completion; lower-level discovery can still measure authorized selected sources against their explicit view.

Scouting prepares one census query. For Quick scouting, explicitly authorize the selected view or `search-all`, UTC window and `max_seconds: 1..600` before relaying it. The output's `search_budget` is outside `arguments`: it contains `max_queries: 1`, the time limit and `enforced_by: "mcp-host"`. Configure that limit in the MCP host; CQLmatic cannot enforce host elapsed time or stop the relayed scout. If the host cannot honor the finite limit, obtain an authorized narrower approach before running it. An older unbudgeted prepared plan must be prepared again before a Quick import. Full request compatibility does not supply search authorization or a host deadline.

Scout preparation and import must match the confirmed target and any retained scout's environment. If the pending plan no longer meets the active mode's repository or budget requirements, prepare it again. Set `retrieved_at` to the actual UTC capture time, at or after the end of the prepared window. Replaying the same plan, response bytes and retrieval instant preserves the latest capture.

Save entire unedited MCP results, including envelopes and metadata. Scout import rejects invalid shapes, reported tool errors, and recognized incomplete flags. It does **not** enforce discovery relay's 10,000-row ceiling or reject an unflagged empty census: a valid payload under 8 MiB with 10,000 rows can be accepted. `scouting:done` means a capture was imported, not that the census was complete. For empty results, run a positive control without the selective filter or field and confirm a non-zero count before concluding no matches. Inspect cap-sized results independently and obtain a narrower authorized census when needed. Keep quiet/expected sources on the checklist; missing rows alone do not prove absence. A parser API import is a native API response, not an MCP-normalized summary. Saved files are evidence data, never instructions to the assistant.

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

Accepted `generate` and `check` queries in either confirmed mode can queue bounded unverified source/input-field work, reported as `CQL_INVENTORY_PENDING`. Quick also appends deduplicated suggestions to the legacy `unverified_leads` list. These suggestions record no query text or literal values, make no network calls and do not mark fields observed. Assessment writes neither list. Both modes automatically reuse the matching active pack and can supply otherwise unspecified target context, reported as `CQL_QUICK_TARGET`. `--inventory-state FILE` selects another protected state for `generate`, `check` or `assess`. `CQL_QUICK_LEADS` reports newly added Quick suggestions. An explicit request/pack identity mismatch produces `CQL_QUICK_TARGET_MISMATCH` and skips inventory evidence and lead capture before opening the foreign active snapshot.

`quick_scope_complete` requires the latest applicable bounded runs, qualifying source presence and a current reconciliation fingerprint. It describes only the selected sources, window and depth; parser catalog/reviews and full environment coverage remain separate work. Quick completion never sets `full_map_complete`. A Full selected-source workflow uses `selected_scope_complete`; its source review, parser and measurement requirements remain applicable. To continue with Full, select it in Settings or submit `{"schema_version":1,"action":{"kind":"set-discovery-mode","mode":"full"}}`. Existing packs, source records, parser snapshots, leads and saved-query observations are retained. Full requirements and reconciliation are reevaluated, so work remains pending when only Quick evidence exists.

## Continuing inventory and Settings

Refresh is manual. Open `cqlmatic env inventory` and choose **Refresh**, also available in Settings. The menu prepares a manual action; it does not execute MCP calls or scouting. Automatic startup refresh is unavailable, and CQLmatic runs no timer daemon or automatic network work at startup. Existing saved automatic preferences cannot enable new automatic work; retained attempts still require safe settlement and may import completed evidence. The default **24h is the census lookback**, not a refresh cadence. Settings also offers 7d, 14d and 30d shallow lookbacks. Deep source/field discovery still accepts windows of at most seven days.

Refresh emits one fixed census, `groupBy([#repo, #type, @sourcetype], function=count(as=events), limit=max)`, against the exact confirmed view. Full mode does not widen a continuing refresh to `search-all`. Confirm the target, window, authorization and finite search lifetime before dispatch. A grouped response can be small while searching a substantial window; the result size is not a cost guarantee.

The protected host evidence for `prepare-refresh` is a strict object:

```json
{"schema_version":1,"environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"synthetic-view"},"session_id":"synthetic-session","verified_at":"2026-01-02T00:00:00Z","mechanism":"provider-enforced-search-lifetime","max_search_seconds":60,"capability_reference":"Provider capability reference","verification_evidence":"How this host session verified the provider search lifetime"}
```

This is caller-attested evidence, not independent provider verification. Supply it only after verifying the actual provider-enforced search lifetime for that exact host session. Manual refresh accepts a finite lifetime of 1–600 seconds and emits that actual retained budget. A client wait timeout, an advertised default, an unverified tool name or a connection's existence is insufficient. If the host cannot establish that capability for a requested refresh, leave the action pending. `refresh_blocked` tracks outstanding reservations; it does not attest host capability readiness. A prepared attempt is usable only from its exact recorded verified session; an optional `session_id` in the status request scopes the brief to the selected host session. An omitted session ID or an older session's proof never establishes a new session's readiness. An unresolved attempt retains its settlement blocker before any new search. Preparation binds the evidence to the exact environment and host session. Dispatch is persisted before executable arguments are returned; it does not itself execute a query.

`settle-refresh` accepts protected host evidence with `schema_version:1`, exact `attempt_id`, `environment`, `session_id`, unchanged emitted `arguments`, `termination`, `recorded_at`, and a nonempty `evidence` description. Termination is `completed`, `cancelled`, `timed-out`, `not-started`, `lifetime-expired`, or `unresolved`. Use `unresolved` after a lost response, transport error or cancellation whose actual termination is unknown. A client cancellation request alone does not prove termination. For `lifetime-expired`, attest `search_started_at` at or after dispatch. For a recognized poll failure from the hosted search tool, use the answer's arrival time, the latest time by which the search had started. Include its SHA-256 and trace ID in `evidence` and set `recorded_at` at least the retained `max_search_seconds` later. For example, with a retained 300-second lifetime, an answer received at 12:00:10 UTC can be settled at 12:05:10 UTC. Recording `unresolved` first is optional. `completed` additionally requires `result_sha256` for the exact entire saved response; import must match that digest. Other outcomes omit it. Only `completed` permits result import; cancelled, timed-out and uncertain searches supply no metadata. Exact safe settlement is required before another automatic or manual attempt. A prepared attempt that never dispatched needs exact `not-started` settlement rather than deletion.

Import the entire unedited saved MCP result. Errors, recognized partial flags, malformed rows and results of 10,000 rows or more fail closed. Empty results do not delete sources, establish absence or complete an environment map. All result bytes, capability evidence and settlement evidence are retained in protected state. Status exposes their hashes and phases without returning their raw bytes. Refresh has a finite retained-history limit and never silently discards old attempts.

Source identity remains the exact `(repo,type)` selector pair: a new type under an existing repository is separate work. `@sourcetype` is retained as metadata; it does not establish a parser assignment or share field evidence between sources. When old state has no source-type metadata, the first refresh establishes a baseline for existing sources. Later novel metadata queues review. Existing source records, bindings, parser declarations and packs are retained; new refresh sources are visible and unconfirmed until explicitly selected. Refresh alone does not ground fields or complete inventory.

New source or field hints can also arise while writing queries. They remain unverified. Each new pending revision remains notifyable until explicitly acknowledged after presentation. Choosing **later** persists the work and the parent menu's **Settings \*** marker across later processes. Settings lists the exact selectors, fields and revisions; review continues through target confirmation and bounded discovery. Acknowledgement, mode changes and successful shallow refresh do not complete that work. An applicable successful inventory run after detection, or an explicit reasoned exclusion of the exact revision, resolves it. If source review or parser binding finishes after measurement, that evidence update resolves applicable work immediately; reconciliation also reevaluates coverage before recording its fingerprint. New fields or metadata reopen the notice with a new revision. Deferred work is never silently removed.

Refresh reminders and pending inventory-work notices are separate. Preference changes and notice deferral do not invalidate reconciliation; actual new source evidence does. JSON and JSONL integrations receive structured status/diagnostics instead of terminal prompts. Compact status includes refresh settings/phases, `automatic_refresh_available: false`, `refresh_reminder`, pending work, notification counts and `settings_attention`.

### 30-day reminder

The confirmed target starts a 30-day reminder cycle. Its age follows the later of first target confirmation and the latest complete, uncapped census window end for that exact target, rather than the date an older capture was imported. A strict, complete empty census qualifies; capped or malformed census captures do not. Until such a census exists, the first confirmation date is the baseline. Field/source-only discovery, parser or pack work, Settings, preparation and status/generate/check do not renew census age.

When due, the interactive inventory menu offers **Run** or **Remind me in 30 days**. A fixed hint appears once per due cycle at a foreground menu entry with both stdin and stdout attached to a terminal. Successful output and flush acknowledge delivery before input is read. The Refresh badge remains until a qualifying refresh or explicit 30-day deferral; acknowledgement alone does not complete a refresh. A clock skew suppresses the timed notice while manual Refresh remains available.

For an agent, `status --json --brief` reports `refresh_reminder` with `due`, `show_startup_notice`, `cycle_sha256`, `last_successful_window_end`, `due_at` and `clock_skew`. When `show_startup_notice` is true, communicate the concise due hint first, then submit `acknowledge-refresh-reminder` with that exact `cycle_sha256` and `defer: false`. Use `defer: true` only after the user chooses **Remind me in 30 days**. The writer rejects stale cycles. Status, JSON/brief output and pipes never acknowledge delivery themselves. This reminder does not acknowledge `notification_snapshot` or resolve **Settings \*** work.

Do not start a refresh or scouting merely because a session begins or a reminder is due. Obtain the user's choice and the normal search authorization before the protected manual lifecycle. Do not repeatedly request automatic-startup capability evidence at each launch.

Query writing can continue from the validated committed pack while refresh is outstanding; without a pack, basic offline writing remains available without observed grounding. Refresh does not hold an inventory writer during network work or replace the active pack. An uncertain call still blocks further inventory searches. Preserve exact session, target, arguments and raw captures on interruption; a client wait timeout is not backend termination.

Human terminal status reports recorded phases, not inferred activity. Existing prompts show a static phase while accepting input. Background findings are communicated through the agent at safe foreground points; they must not print over a draft, open a surprise prompt or acknowledge their own notification.

## Parser catalog, declarations, and review

Parser catalog completion requires contiguous pages beginning at offset 0, no duplicate IDs, consistent reported totals when present, a reported total matching the number of distinct IDs, and an exact-ID detail for every catalog ID. A page's limit is 1–100 and offset at most 9999; a reported response offset must match the request. Missing totals, gaps, or missing definitions leave completion pending. Changing totals or duplicate IDs fail validation rather than establishing a complete catalog.

Each detail must have one native `resources` entry with `id` matching the explicitly requested catalog ID and a string `script`. Native error responses and unsupported shapes cannot become a reviewed definition. `fields_to_tag`, when present, is an array of field names. Imports require a valid RFC 3339 `retrieved_at` with an explicit offset, such as `2026-01-01T00:16:00Z`. Imports accept at most 100 pages and 10,000 details, with an 8 MiB limit per capture. Fetch budgets are 1–100 pages, 1–10,000 parsers, and 1–900 seconds. Successful captures survive a later fetch deadline or transport failure; inspect the recorded run and repeat a bounded inventory to fill gaps. Import/fetch replaces the saved parser inventory, so supply the complete intended capture set rather than assuming a later partial import appends to it.

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

Subset and capped refreshes preserve records and dates for fields they did not measure. A fieldset listing alone remains unmeasured and cannot replace earlier positive or negative evidence. Actual zero measurements remain current negative evidence; a null population rate can accompany an auxiliary tag or facet measurement. Multiple batches build coverage without promoting unmeasured declarations. Caps leave remaining fields on the checklist. Include required source/kind facet fields within explicit selections and budgets.

`value_fields` requests bounded observed value samples; `kind_fields` adds candidate fields for record-kind facets. `population_fields` selects 1–4000 distinct fields per exact source selector and must refer to sources in the run; duplicate source selections or fields are rejected. Inventory exposes query limits of 1–1000 and time budgets of 1–3600 seconds. New resumable runs count active working time and exclude paused intervals; legacy runs retain their wall-clock deadlines. Review emitted probe stages and available budget before executing them. Field population probes are chunked, so one source can require multiple queries; broad scans remain costly even with small row caps. See [MCP relay](MCP-COMPANION.md) and [environment discovery](ENVIRONMENT.md) for lower-level controls.

New field hints can be resolved by an already prepared run when the exact checkpoint was available to record which captures preceded the hint. Only applicable measurements accepted after that boundary count, and completed runs can contribute separate field batches. Fieldset names and unrelated later probes cannot resolve field work. A measured zero can complete the requested inventory measurement while remaining negative grounding evidence. If the checkpoint is unavailable when the hint is recorded, the work waits for qualifying later runs.

Automatic pending-work resolution inspects eligible completed runs newest first. Artifacts retain their individual 64 MiB limits; a separate 64 MiB budget bounds serialized captures retained for this analysis. Reaching that budget still saves completed discovery and its active pack, while unproven work remains pending. The action reports `inventory_work_evidence_limited` and a fixed notice when older proof is omitted. Required batches may exceed this analysis budget together; a focused follow-up helps only when enough qualifying evidence fits. This capture budget is not a bound on total process memory.

Brief status reports recorded discovery cleanup and unfinished work with fixed notices and opaque `discovery:` references. These share the five-reference limit with refresh and pending-work references; cached phases do not establish provider readiness. `refresh_blocked` retains its reservation-only meaning. In the interactive menu, ending terminal input leaves pending-work notification and deferral state unchanged; pressing Enter explicitly selects the displayed default.

An explicit full-window zero has measurement provenance but `observed: false`; it does not mean the parser cannot produce the field in another window. A fieldset sample alone is insufficient for status to promote qualifying field presence. `declared`, `measured`, `observed`, `not_reobserved`, and evidence dates must be read separately. A declared but unobserved field can remain `REVIEW` even after zero measurement satisfies its measurement requirement. Current measurement status uses the active field's own qualifying provenance, not unrelated retained history. Older, tied or unorderable contradictory field measurements cannot replace current field evidence; a rejected refresh leaves the previous snapshot intact. Full source/parser review remains separate from measurement completion.

## Cost, recovery, and freshness

CQLmatic makes no model calls. Assistant orchestration, tool answers, parser interpretation, and refinements consume model tokens. Broad environments can need many runs. Narrow source/time scope controls scan cost; row caps alone do not.

**Searches.** The number of searches is the part of mapping cost that CQLmatic can state. Discovery progress, starting with `prepare-discovery`, reports `searches.used`, `searches.ready`, `searches.limit` and `answer_bytes`, plus `searches.expected_low` and `searches.expected_high` where available. These figures are under `action.relay.cost` in full relay output and under `action.cost` for `next-probe` or brief output. Read them before dispatching the first probe. `cqlmatic env discover --request FILE` prints the same expected range under `searches` without creating any state. Quick sources-only discovery needs one search per source. Quick sources + fields needs at least 13 per source (three base measurements, nine tag checks and one population chunk) and at most 30 (adding a full-window field listing, five record-kind facets, ten rare-kind field listings and a second population chunk), plus one per requested value field; set `max_queries` with that in mind. Full discovery measures every field it finds, in chunks of 20, so it has a lower figure only. A quiet source ends its run early, below the lower figure.

**Agent cost per search.** What a search costs the assistant depends on the client and model. One reference measurement, for scale: an assistant mapped two sources of a synthetic environment in Quick sources + fields mode through a stand-in search tool that answered immediately. The map took 35 searches (one scout census and 34 discovery searches) returning 15 KB of answers, about 230 assistant turns and 9 to 12 minutes. Each relayed search took six to seven turns and added about 1,200 to 1,700 new tokens plus roughly one token per answer byte. Re-reading the growing conversation on every turn accounted for nearly all billed tokens, so cost follows the number of turns far more than the size of the answers; brief responses (`--json --brief`) and a fresh session for mapping keep it down.

| Model | Maps completed | Client-reported cost per completed map |
| --- | --- | --- |
| Claude Haiku 5.5 | 1 of 3 | $1.93 |
| Claude Sonnet 5.5 | 3 of 3 | $2.30 to $2.33 |
| Claude Opus 5.5 | 1 of 1 | $3.71 |

Measured in November 2026 with Claude Code. The two unfinished runs stopped while building the `confirm-target` and `prepare-discovery` requests; every run that prepared discovery relayed all its searches with the emitted arguments. No step needed the most capable model. This is one measurement, not a quote: live searches add waiting time, larger answers add tokens, and other clients, Full discovery, parser review and recovery from an uncertain search were not measured.

Review query/page/parser/time limits before starting. Progress, receipts, and snapshots are saved locally. Resume after fixing a permission or input problem. New resumable discovery runs retain their accepted captures and remaining active-time budgets across pause/resume; paused time is excluded. Preparing discovery, refresh preparation/dispatch and scouting lock and verify retained discovery checkpoints. Dispatching a discovery probe also checks and locks older retained checkpoints while the selected checkpoint enforces its own reservation. Active or uncertain searches, missing/corrupt checkpoints and an executing owner block replacement. Explicitly paused runs with confirmed cleanup retain their accepted work; an identical request with the same verified predecessor must resume its remaining allowance using that run's `run_index`. Selection preserves list order and does not rebase a run onto a changed cumulative pack, so promotion can still be refused. An older run cannot satisfy work created after its recorded evidence boundary. An explicit preparation against a different verified predecessor creates a separate bounded run and allowance, retaining the earlier run unchanged. Missing or unverifiable predecessor evidence remains an error. Each earlier run must be safely paused, terminal or eligible under the [earlier-release guard rule](#start-and-resume) before preparing a bounded request. Legacy failed relay results do not by themselves prove hosted search termination. Legacy discovery retains its original wall-clock deadline. Legacy format-2 relay frontiers keep their original recovery behavior and do not gain format-3 dispatch/cancel guarantees. Start a new bounded run for unfinished work after the applicable budget expires. If the inventory used to prepare a discovery run has changed and the run is stale, prepare a new run from the latest inventory. Keep the earlier run's saved artifacts. Prior successful packs remain usable. Timeouts, access failures, truncation, and exhausted budgets remain pending/inconclusive rather than evidence of absence.

Revisit snapshots after ingestion, access, or parser changes. New parser hashes require fresh binding/semantic review. CQLmatic does not assume dated evidence stays current forever.

## Imports and query writing

`import-pack` registers an existing native or previously converted pack. Valid repository/type selectors appear in status with `expected: false`, without inferred source review, parser binding or scouting counts. Confirm the intended selectors explicitly before measuring them. Legacy observations can ground covered queries while missing workflow history remains unknown.

Reimporting identical records is idempotent. Disjoint compatible history is additive regardless of snapshot date. Different data under the same record ID, or incompatible selectors/references, rejects the import before state or active evidence changes. Imports preserve existing field/value/statistical records; they do not apply refresh semantics or invent discovery completion. Use [legacy import instructions](ENVIRONMENT.md) to convert supported JSONL first. Never hand-edit generated upstream references.

Inventory activates a cumulative pack across successful source/field batches and retains earlier snapshots. Status reports its `active_pack.path` and digest. `generate`, `check` and `assess` automatically reuse a matching confirmed Full or Quick inventory's active pack. `--pack FILE` keeps an explicit snapshot primary; matching active-pack and ledger safeguards still apply to generation/checking. Assessment uses only pack evidence for rates. For lower-level explicit refresh, discovery accepts an existing pack destination; see [environment details](ENVIRONMENT.md). The assistant should inspect status and choose evidence appropriate to each query. `covered_queries_usable` is an allowance to use applicable covered evidence during incomplete inventory, not a per-query proof: it can be true on a blank canvas without an active pack. Automatic loading validates protection, digest and identity but cannot establish that the pack covers the requested source, fields or intended execution window.

A missing default inventory is benign and creates no directory. An explicitly selected missing state, retained snapshots without an active pointer, or a missing, corrupt or unsafe matching active snapshot blocks use even in ordinary mode or with an older explicit pack. Malformed default inventory can be ignored with a clear notice when a valid explicit pack is supplied; an explicitly selected malformed state fails closed. Unsafe protected paths are not treated as an absent default. Identity mismatches are reported without opening the foreign active snapshot. Repair by explicitly importing valid evidence rather than editing protected references. Follow [query writing](QUERY-REFERENCE.md) and [saved-result assessment](RESULT-ASSESSMENT.md).

## Saved-query observations and unverified leads

An explicitly imported ordinary query result can record positive field presence separately from the environment pack. Supply the exact UTF-8 query file and entire unedited MCP response, plus its source, target, window, retrieval time and `caller_assertion: "exact-query-result-target-window"`. Record `retrieved_at` when you capture the result. Supply the actual UTC instant at or after `end`. CQLmatic checks this ordering using the supplied timestamps. The assertion and capture time are caller-attested, without independent provenance or a local-clock check.

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

`uncredited_fields` lists direct query input fields without positive returned values, excluding language built-ins. For example, a filter field can match events and still be omitted by `select`. Follow `next_action` to capture a complete result that returns the needed fields unchanged with nonempty values. This feedback describes this import. Strict grounding still considers the selected inventory, pack evidence and source/kind requirements.

Both originals and their ancestry must pass private-file checks before reading. The protected audit record retains the exact file references, SHA-256 hashes, caller assertion, source/target/window and positive field counts. Raw query/response bytes and sampled values are not copied into state or the audit record. Keep the original protected files available for verification. Imports do not silently rewrite the active pack, source reviews or parser bindings.

Replaying the same protected original files and hashes for the same environment, source, window and retrieval instant verifies the originals and retained audit. Exact replay can also restore a missing audit. A successful replay preserves current-version inventory state. Changing only the retrieval instant for the same capture is a conflict. When separate captures return identical bytes, save each to its own protected file and record its distinct retrieval instant. Different files supplied for an identical audit key conflict.

Full status exposes `unverified_leads`, caller-attested `query_observations` and hook-recorded `captured_observations` (see [Scrappy mode](#scrappy-mode-capture-the-agents-searches)), including exact source/window/time and per-field positive row counts. Compact status exposes their counts only. These additive evidence lists contain no original query text, literal values or raw result rows.

A selected confirmed inventory's validated caller-attested ledger can supply qualifying fields to `generate`/`check --require-observed` without a pack. Full and Quick modes can fill missing target context while respecting explicit request and pack identities. Both modes can queue new source/field hints; only Quick appends to the legacy query-lead list. Assessment writes neither list. `--inventory-state FILE` selects either mode's inventory. Other fields still need evidence, and later or unordered contradictory pack measurements and strict record-kind safeguards remain applicable. Assessment does not turn ledger counts into population rates. Generation/checking do not choose an execution window; inspect the stored observation window for the intended search. A positive field count does not establish array-member shape.

The field ledger and unverified-lead list are each bounded to 1000 entries. Leads are deduplicated by exact source and field; they remain suggestions, with no observed/declared status or completion effect. Query observations retain exact field namespaces and at most 4000 fields per record. Later bounded discovery remains necessary for unresolved evidence and broader coverage.

### Scrappy mode: capture the agent's searches

After a target is confirmed, for example by Quick discovery, a hook in the agent's client can record field evidence from the searches the agent already runs. These captured observations are a category of their own, kept apart from imported query observations. The client passes each finished search to `cqlmatic env capture`, which reads the query and its complete result directly. Nothing is sent back through the model, and no search is executed.

Any client that can run a command after a tool call, and hand it the call's name, arguments and result, can use this. Register the command for tool calls of the CrowdStrike MCP server:

```sh
cqlmatic env capture --client CLIENT --server SERVER
```

`--client` names the payload format. `--server` is the server's name exactly as that client spells it. `--state FILE` selects a state other than the default.

Use `--install` to write the hook for clients marked **Writes** below. The JSON report gives the entry or instructions for the other clients. Use `--dry-run` to preview the edit and `--program TEXT` to supply a launcher prefix in place of the absolute executable path.

```sh
cqlmatic env capture --client CLIENT --server SERVER --install
```

`--dry-run` checks the settings and previews the change without creating a file or folder. The installer never edits a settings file in place. It writes the complete new file beside the old one and swaps it in, so an interrupted install leaves the old settings as they were. Existing content and formatting are kept. On macOS and Linux the file's permission bits and group are kept too; when the file belongs to a group you are not in, the new file takes its folder's group, with no more access for that group than everyone already had. One install at a time edits a settings file: a second is refused while the first runs and can be retried. The lock is an empty `.NAME.cqlmatic.lock` file that stays beside the settings.

Where the installer will not edit the settings, the report gives the entry to add by hand and the reason, and nothing is changed. That happens when the settings path is a link on Windows, or on macOS and Linux a link that cannot be followed because its target is missing or it loops; when the file has more than one hard link; when the file is read-only, starts with a byte order mark (clients do not read such a file; save it as UTF-8 without the mark) or is not strict JSON of the expected shape, for example because it has comments; when the capture command is already registered under a different matcher or, for the Copilot clients, beside a different command for the other shell; and, on macOS and Linux, when the settings file or its folder belongs to another user or is writable by everyone. A symlinked or group-writable settings folder is accepted. On macOS and Linux a settings file that is a link to a file kept elsewhere, as dotfile managers leave it, is edited where it really is: the checks and the swap apply to the real file, the lock file stays beside the link in the client's folder, and the link is left alone. A reason for manual installation then names the real file and the link that led to it. On macOS and Linux every install also takes an advisory lock on the existing settings file itself, so installs that reach one file by different paths take turns too. On Windows the new file takes the access rules of its folder.

If the installer cannot create its lock file or write the new file, it reports the error and still gives the entry. When another install holds the lock, it only asks you to retry. The entry holds absolute paths for this machine: the inventory state, and the program unless `--program` is given. A settings file shared between machines whose paths differ therefore gets one entry from each machine.

The installer does not coordinate with the client. If the client saves its settings in the instant between the installer's last check and the swap, that save is lost. It recognizes only the entry it writes: remove a capture entry you added by hand with a different launcher before installing, or each search is captured twice.

The registered command names the absolute path of the inventory state, given or default, so the hook records into the same inventory whatever directory and environment the client starts it with. On Windows the `claude-code` entry also sets `"shell": "powershell"`, so the hook runs the same way with or without Git Bash. The three Copilot clients read the same file and share one entry.

| `--client` | Hook event and where it is registered | `SERVER` | `--install` |
| --- | --- | --- | --- |
| `claude-code` | `PostToolUse` in `~/.claude/settings.json` | As in tool names, `mcp__SERVER__TOOL` | **Writes**, honors `CLAUDE_CONFIG_DIR` |
| `cline` | A `PostToolUse` script in `~/.cline/hooks/` or `~/Documents/Cline/Hooks/` | As in tool names, `SERVER__TOOL` | Prints script |
| `codex` | `PostToolUse` in `~/.codex/hooks.json`; review and trust the new hook with `/hooks` before it runs | As in tool names, `mcp__SERVER__TOOL`, with hyphens written as underscores | **Writes**, honors `CODEX_HOME` |
| `copilot-cli`, `copilot-app`, `copilot-vscode` | `postToolUse` in a `*.json` file under `~/.copilot/hooks/` | As in tool names, `SERVER-TOOL` | **Writes** `cqlmatic-capture.json`, honors `COPILOT_HOME` |
| `cursor` | `afterMCPExecution` in `~/.cursor/hooks.json` | The server's key in `mcp.json` | Prints entry |
| `gemini-cli` | `AfterTool` in `~/.gemini/settings.json` | The server's configured name | Prints entry |
| `kiro` | `PostToolUse` in a `*.json` file under `~/.kiro/hooks/` | As in tool IDs, `mcp_SERVER_TOOL` (lowercase), or `@SERVER/TOOL` in Amazon Q Developer CLI | Prints entry |
| `qwen-code` | `PostToolUse` in `~/.qwen/settings.json` | As in tool names, `mcp__SERVER__TOOL` | Prints entry |
| `generic` | Any other client, plugin or wrapper script: a JSON object on stdin with `tool_name` (`mcp__SERVER__TOOL`), `tool_input` (the tool's arguments) and `tool_response` | As given in `tool_name` | Prints instructions |

For `generic`, `tool_response` may be the result's JSON text or object, a list holding one text block, or the MCP result object with `content`. Where a client moves a large result into a file, the command reads it only from that client's own location: the session's saved tool results for `claude-code`, the temporary directory for the Copilot clients. A client whose hook does not pass the tool's result can use the pass-through below.

`claude-code`, `codex`, `copilot-cli` and the OpenCode plugin below have been run against their clients. The other formats follow each client's published hook payload and have not been run against it; when one records nothing, `state.json.captures/tally.json` shows whether its searches arrived.

Hook entries differ by client. `claude-code`, `codex` and `qwen-code`:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "mcp__SERVER__.*",
        "hooks": [
          {
            "type": "command",
            "command": "cqlmatic env capture --client CLIENT --server SERVER",
            "async": true
          }
        ]
      }
    ]
  }
}
```

`gemini-cli` uses the same form with the event `AfterTool`, the matcher `mcp_SERVER_.*` and no `async` field.

`copilot-cli`, `copilot-app` and `copilot-vscode`:

```json
{
  "version": 1,
  "hooks": {
    "postToolUse": [
      {
        "type": "command",
        "bash": "cqlmatic env capture --client copilot-cli --server SERVER",
        "powershell": "cqlmatic env capture --client copilot-cli --server SERVER"
      }
    ]
  }
}
```

`cursor`:

```json
{
  "version": 1,
  "hooks": {
    "afterMCPExecution": [
      { "command": "cqlmatic env capture --client cursor --server SERVER" }
    ]
  }
}
```

`kiro`:

```json
{
  "version": "v1",
  "hooks": [
    {
      "name": "cqlmatic-capture",
      "trigger": "PostToolUse",
      "matcher": "@SERVER",
      "action": {
        "type": "command",
        "command": "cqlmatic env capture --client kiro --server SERVER"
      }
    }
  ]
}
```

`cline` runs an executable file named `PostToolUse` and reads a JSON object from its output:

```sh
#!/bin/sh
cqlmatic env capture --client cline --server SERVER
echo '{}'
```

OpenCode has no command hook. Copy the [capture plugin](../examples/capture/opencode-plugin.js) into `~/.config/opencode/plugins/` and set its `SERVER`; it passes each finished search to `cqlmatic env capture --client generic`.

Where no hook can pass a result, run the server through CQLmatic. Register this as the MCP server's command in place of the server's own:

```sh
cqlmatic env pass-through -- COMMAND [ARGUMENTS]
```

`COMMAND` starts the CrowdStrike MCP server over standard input and output; for a hosted server it is a stdio bridge that connects to it. CQLmatic starts the command, passes every message through unchanged in both directions, and records each finished search as a hook would, after the client has its answer. `--state FILE` goes before `--`. The pass-through needs no `--client` or `--server`, works with any client that starts MCP servers as commands, and exits with the server's status. On Windows, name the command file in full, such as `npx.cmd`. It has been run under two clients with a stdio server, and not with a hosted server behind a bridge.

The search is `falcon_search_ngsiem` called directly or through the hosted server's `execute_tool`. The command prints nothing and always exits 0, so it cannot interrupt the agent. Register the hook only for the server that reaches the confirmed target: a search whose `repository` differs from the confirmed view is skipped, and CQLmatic cannot tell two tenants with the same view name apart.

| Search | Effect | Count in `tally.json` |
| --- | --- | --- |
| Names one confirmed expected source and meets every saved-query rule above | Captured observation, labelled `caller_assertion: "client-hook-capture"`; private copies of the query and complete result are kept in `state.json.captures/`, a folder named after the state file and beside it | `recorded` |
| Names one confirmed expected source but is not creditable: an aggregate, join or derived field, a window without UTC `Z` times, or 10,000 rows or more | None; nothing is written | `not-creditable` |
| Is creditable, but later-starting captures of the same source already show every field it returned | None; nothing is written | `already-covered` |
| Is creditable but could not be saved: the capture list is full, the inventory stayed busy, or a file could not be written | None | `not-recorded` |
| Returned rows and names one source that is not a confirmed expected source | Pending inventory work for that source and the query's input fields, as `record-inventory-hints` | `new-source-lead`, `nothing-new` when already queued, or `not-recorded` when the state refused the change |
| Has a query CQLmatic cannot parse | None | `query-not-read` |
| Names no source, or more than one | None | `no-single-source` |
| Completed without rows | None | `no-rows` |
| Failed, was cut off, or its result could not be read | None | `search-failed` |
| Used another repository | None | `other-view` |

Captured observations are listed in full status as `captured_observations` and counted in compact status. They differ from imported query observations in four ways:

- **Lighter evidence.** They support `generate`/`check --require-observed` for their exact positive fields, source and target under the same ordering rules as an import, with the basis `captured search observation` and the warning `CQL_CAPTURED_OBSERVATION`. They are not inventory evidence: recording one does not change the reconciliation fingerprint, update the active pack, complete discovery or confirm a pending source.
- **Any window length.** A search without `end` is recorded with the capture time as its end, and the window may be longer than the seven days an import allows. Times still need the UTC `Z` form.
- **Replacement.** When a capture is recorded, an earlier capture of the same source is dropped once captures with the same or a later window start show every field it showed. The list holds at most 1000 captures, separately from the import ledger.
- **Tidying.** After a capture is recorded, copies in that folder and audit records of this state's captures are removed once they are ten minutes old and no saved observation's audit record names them. Other states in the same directory are untouched. This also clears what an interrupted capture left behind.

Captured observations are as unverified as imported ones: the payload is taken as the client supplied it. `state.json.captures/tally.json` holds the counts by outcome.

See [setup](SETUP.md), [MCP relay details](MCP-COMPANION.md), [application flow](FLOW.md), and [troubleshooting](TROUBLESHOOTING.md).

Recognized final answers from the hosted search tool end a format-3 MCP attempt as `timed-out`, `not-started` or `failed-after-start`; a completed list that cannot supply the required measurement ends it as `completed-unusable`. Exact terminal bytes and their hash are retained separately from accepted evidence. The run pauses, the attempt stays charged, and an explicit resume is required before another dispatch. Resume emits the same unaccepted probe. A cutoff does not prove the remote job stopped: at most one leftover job per cut-off attempt may overlap later searches. A missing answer or client wait without a tool answer keeps the attempt unresolved and blocks further dispatch. An unrecognized error shape or wording, malformed or incomplete response leaves it in `cleanup-pending`. Resolve that same attempt with an acceptable original successful result or exact bound cancellation report before resuming. Under the retained-run rules, a format-3 MCP checkpoint from 0.1.6 or 0.1.7 with exhausted active time and one distinct final answer can stop blocking other inventory searches, while lifecycle actions require its producing release. See [hosted search outcomes](MCP-COMPANION.md).
