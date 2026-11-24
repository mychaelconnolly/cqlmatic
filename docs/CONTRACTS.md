# Machine contracts and automation

The supported integration surface is the executable, not a stable Rust SDK. This reference describes package **0.1.13**, JSON contract version **1**, and executable profile **`cqlmatic-ngsiem-core-v3`**. Earlier packages retain their shipped behavior and installation links. Package version, request/status schema version, inventory state version, pack format, profile ID, and knowledge revision are separate identifiers. Inspect the running binary with `--version`, `--schema NAME`, and `catalog --profile`; a package version does not imply a different evidence policy.

See [hosted search outcomes](MCP-COMPANION.md#how-to-tell-whether-a-hosted-search-finished) for completion and error shapes, retry guidance and the positive control required for zero results. For refresh and capability settlement, use `not-started` for a recognized start rejection and `timed-out` for a hosted cutoff. Keep lost responses, client waits without a tool answer, transport errors and unrecognized errors `unresolved`. A recognized poll failure from the hosted search tool shows the search had started by the time the answer arrived. Use that arrival time for `search_started_at` and submit the exact bound `lifetime-expired` settlement with `recorded_at` at least the retained provider lifetime later. Record the saved answer's SHA-256 and trace ID in `evidence`. For capability, also supply the saved answer as `result_file`. The refresh imports no rows and the capability case stays `unknown`.

Use [USAGE](../USAGE.md) for flags, [setup](SETUP.md) and [inventory](INVENTORY.md) for actions, and [query construction](QUERY-REFERENCE.md) for nested plans. All examples are synthetic. Runtime operations require the caller's explicit authorization and environment.

## Schema exports

Every export is JSON Schema draft 2020-12, available offline and identical to its checked-in file:

| Export | Schema root describes | What to send or consume |
| --- | --- | --- |
| [`generate`](../schemas/generate.json) | `GenerateRequest` | Send the whole request object. |
| [`check`](../schemas/check.json) | `CheckRequest` | Send the whole request object. |
| [`result`](../schemas/result.json) | `Analysis` | Consume generate/check results. |
| [`pack`](../schemas/pack.json) | Native `Pack` | Read/import a complete unfiltered snapshot. |
| [`discovery`](../schemas/discovery.json) | `DiscoverRequest` | Send a discovery request; this is not its report schema. |
| [`setup`](../schemas/setup.json) | Wrapper with `request`, `result`, `state` | Select the matching subschema. |
| [`inventory`](../schemas/inventory.json) | Wrapper with `request`, `state` | Send its request type; stored state is not an action request. |
| [`status`](../schemas/status.json) | Wrapper with `request`, `result` | Send a status request or consume `InventoryStatus`. |
| [`assess`](../schemas/assess.json) | Wrapper with `request`, `result` | Send `AssessRequest` or consume `AssessResult`. |

For example, `cqlmatic --schema setup` has a `properties.request` reference into the root `$defs`. Configure your validator to resolve that subschema with the original root and `$defs` available. Do not send `{request: ..., result: ..., state: ...}` to the CLI or extract a `$ref` without its definitions. Inventory action output is an envelope described below; there is no separate bundled schema for that envelope or the MCP step output.

Schemas describe JSON shape. Runtime also checks exact supported versions, finite ranges, placement, evidence, paths, hashes, and cross-field relationships. A schema can permit an integer that runtime refuses: requests accept exactly `schema_version: 1`. Named typed request objects reject unknown fields. Omitted defaulted fields and explicit `null` are not interchangeable for nonoptional types. JSON scalars are strings, numbers, and booleans; there is no raw-CQL-fragment scalar.

## Generation and check requests

| Field | Generation | Checking |
| --- | --- | --- |
| `schema_version` | Required integer `1`. | Required integer `1`. |
| `id` | Optional caller string; absent/null becomes null in the result. | Same. |
| `input` | Required tagged generation input below. | Not accepted. |
| `query` | Not accepted. | Required CQL string, including empty or `*` match-all. |
| `context` | Optional context, default `{}`. | Same. |

Generation input is exactly one of:

| `input.mode` | Other fields |
| --- | --- |
| `plan` | Required `plan` object with `source`, optional/defaulted filters and steps. |
| `recipe` | Required `request` containing a tagged recipe and its parameters; see [recipes](QUERY-REFERENCE.md#all-nine-recipes). |
| `english` | Required `text` and required `source` object, including `{}` if unscoped. |

```json
{"schema_version":1,"id":"synthetic-count","input":{"mode":"english","text":"count events","source":{}},"context":{}}
```

```json
{"schema_version":1,"id":"synthetic-check","query":"count()","context":{}}
```

The CLI shortcut `--plan` takes only a Plan, while `--request` takes the outer request. `--recipe ID --params FILE` supplies the recipe tag separately. Do not wrap a plan twice.

### Context

| Field | Default | Meaning |
| --- | --- | --- |
| `environment` | Null | Optional `{cloud, tenant, view}` binding; when provided, it must agree with the supplied pack. |
| `execution_context_id` | Null | Optional caller-attested current authorization-context ID for exact capability observations; never inferred from CID. |
| `confirmed_fields` | `[]` | Caller assertions about explicit field names; not observed deployment evidence. |
| `require_observed` | `false` | Require qualifying observed snapshot evidence for referenced deployment fields. |
| `accept_mixed_record_kinds` | `false` | In strict observed mode, accept kind ambiguity with a warning after user permission; does not select a kind. Ordinary mode reports kind ambiguity as information. |
| `accept_partial_coverage` | `false` | In strict mode, accept qualifying current positive partial source/field evidence with a warning; stale, absent or ambiguous evidence still blocks. |

`EnvironmentId.cloud` is `us1`, `us2`, or `eu1`. `tenant` is a validated CID or API-client binding where accepted; `view` is the explicit searchable API/MCP target. `view` and a CQL `#repo` selector are different namespaces. Obtain real identity through the [environment workflow](ENVIRONMENT.md#bind-an-account), not the illustrative all-zero example.

CLI policy flags strengthen/enable the same request flags with logical OR. They cannot turn a true request value off. A process reads one explicit `--pack` for all JSONL records; record contexts can assert matching identity but cannot select another explicit pack. Inventory target selection and active-pack reuse happen separately for each record. Full and Quick can fill an otherwise unspecified environment; foreign inventory is skipped before opening its snapshot. An explicit pack remains primary, and matching active-pack/ledger safeguards still apply. `--inventory-state FILE` is a CLI path selection, not a new request property.

`--execution-context-id` supplies the current generation/checking context and, when present, overrides the request's `context.execution_context_id`. A qualifying capability observation also requires the exact environment/CID/view, current case revision, argument shape and resource binding, and expires exactly 30 days after result retrieval. This is separate from field grounding. Unresolved dashboard/user parameters, saved-query helpers and parser macros have no binding/substitution interface and remain unsupported.

### Analysis results

| Field | Meaning |
| --- | --- |
| `schema_version` | Result contract version, currently `1`. |
| `id` | Successfully decoded request's caller ID, otherwise null. |
| `outcome` | `ready`, `needs-context`, `unsupported`, `invalid`, or `error`. |
| `query` | Generated CQL only for ready generation; null on other outcomes and **all check results**. |
| `validation` | Local language assessment; see below. |
| `grounding` | Evidence and unresolved requirements; see below. |
| `interpretation` | Selected input path, visible semantic plan, and assumptions. |
| `diagnostics` | Ordered diagnostic objects; inspect severity and code, not just count. |

`validation.status` is `accepted`, `rejected`, or `unknown`. `validation.profile_id` identifies the implemented profile; `validation.execution` is `not-run`. Accepted syntax plus unresolved grounding can yield `needs-context`: do not infer outcome from validation alone. `unsupported` is a coverage verdict, not a vendor-invalid verdict.

`grounding.pack_id` identifies the selected digest or is null. `basis` maps requirements to evidence explanations; preserve these explanations rather than treating them as a fixed vocabulary of machine enums. `missing`, `conflicting`, and `stale` list unresolved requirements. Applicable observed provenance, source selection, tag/data namespace, shape, and lineage matter. A source-only count can have no deployment field to ground; strict mode still requires a valid pack. Without strict grounding, `CQL_UNMAPPED_SOURCE` and `CQL_UNMAPPED_FIELD` warnings report a source selection that the selected pack does not hold, or a referenced field that it does not list as observed, documented or declared; `basis` keeps the per-field evidence. Field-pack grounding has no wall-clock freshness cutoff; optional capability observations have their separate 30-day expiry. See [grounding](ENVIRONMENT.md) and [limits](COVERAGE.md).

`interpretation.path` identifies `plan`, `recipe`, `english`, `check`, or an input-failure path. `plan` is the normalized semantic plan when available; `assumptions` is a string list. Its presence is not a substitute for caller review of analytic intent.

### Diagnostics

| Field | Meaning |
| --- | --- |
| `code` | Source-defined diagnostic identifier; no forward-stability guarantee is promised. |
| `severity` | `error`, `warning`, or `info`. Severity alone does not classify unsupported syntax versus a checked rule. |
| `message` | Human explanation; do not parse it as a contract. |
| `span` | Optional `{start,end,line,column}` in the assessed CQL. |
| `pointer` | Optional request location or synthetic `/line/N` for a stream failure. |
| `reference` | Optional bundled catalog record ID for explanation. |

Span `start`/`end` are zero-based **UTF-8 byte** offsets, end exclusive. `line`/`column` are one-based Unicode-character coordinates, not UTF-16 offsets. Omitted span/pointer/reference fields are not errors. Generated CQL diagnostics can refer to rendered text; request-lowering diagnostics can use a pointer instead. Obtain full catalog records using `catalog RECORD_ID --full --limit 1`.

## Setup request, result, and state

Send `{schema_version:1, id?, action:{type:...}}`. The [setup action table](SETUP.md) describes each payload and transition. Actions are `status`, `detect`, `select`, `preview`, `apply`, `confirm-authentication`, `import-discovery`, `acknowledge-pending`, and `cancel`. The optional setup ID is at most 256 bytes without control characters.

`SetupResult` contains `schema_version`, `id`, complete `state`, `next_actions` strings, optional `handoff` text, dated `sources`, and `diagnostics`. Each source supplies `reviewed_on`, `title`, `url`, and `uncertainty`. Guidance describes the adapter and dated references, not discovered tenant permissions. Interpret any example target in handoff guidance against the actual confirmed environment.

| State field | Meaning |
| --- | --- |
| `schema_version`, `revision` | Version and changing setup revision; previews bind the revision. |
| `phase` | State machine phase below. |
| `selection` | Selected `client`, `cloud`, `server_name`, tagged registration choice, and `host_id` only for `external`, or null. |
| `clients` | Observed client executables, versions, capability flags and fingerprints. |
| `plan` | Optional reviewed native-command or Copilot file-write preview, baseline, changes, backup path, and confirmation hash. |
| `recovery_origin` | Optional original acknowledged, uncertain Codex CIMD fresh-add plan retained for a same-entry recovery login. Omitted in legacy/no-recovery state; it is not proof of dispatch or success. |
| `configured` | Native configuration completed or equivalent registration was found; not authentication proof. |
| `authentication_evidence` | Distinguishes no evidence, user confirmation, and a bound saved host-discovery report. |
| `configured_sha256`, `discovery_sha256` | Optional selected configuration-entry/legacy whole-file and saved-report bindings. |
| `last_notice` | Optional last pending/error notice. |

Phases are `new`, `selected`, `installation-required`, `review-required`, `command-pending`, `authentication-required`, `authenticated`, `tool-discovery-verified`, `advanced-pending`, and `cancelled`. A phase is not an instruction to skip human consent or pending-command review.

Preview operations are `configure`, `authenticate`, and eligible same-entry `recover`. Recovery preserves uncertainty until a freshly reviewed explicit native CIMD login succeeds against unchanged configuration; it does not adopt arbitrary preexisting or manual entries. A valid protected receipt can instead restore configuration provenance after an interrupted state save or authentication attempt. Login provenance is distinct from completed-add provenance; the saved host report remains required. See [setup recovery](SETUP.md#recover-a-partially-completed-codex-cimd-setup).

The preview contains `schema_version`, `state_revision`, `operation`, `selection`, `baseline`, `commands`, `changes`, `backup_path`, and `confirmation_sha256`. `client` is present for command adapters; `config_write_sha256` is present for a planned new Copilot user file and covers its exact bytes, including the trailing newline. An existing Copilot file is never rewritten; add a missing entry through Copilot and preview again. Native commands contain exact `program`, `args`, `interactive`, and `timeout_seconds`. Baseline `path`, `sha256` (null for absence), and `server` distinguish absent, same, and colliding entries. Apply needs the exact lowercase confirmation digest and rechecks the baseline/executable when applicable. Never reconstruct a preview hash or infer rollback. Claude/Copilot saved reports additionally bind `effective_oauth_client_id` and `effective_config_source` from the chosen session; these are caller-supplied, not independent identity proof.

The on-disk setup file wraps state with its schema version and `state_sha256`; this storage wrapper is not `SetupResult` or the bare state subschema. Do not hand-edit it. No-request `setup --json` reads saved status without detecting clients, reading native configuration, creating state, or prompting. A request action `status` uses the writer and can save state. Setup returns `0` only at `tool-discovery-verified`; intermediate results return `1`. A global error returns `2` without a `SetupResult`.

## Inventory requests and status

Send `{schema_version:1, action:{kind:...}}`; there is **no caller ID**. Core actions are `init`, `set-discovery-mode`, `prepare-scout`, `import-scout`, `confirm-target`, `review-source`, `import-parsers`, `fetch-parsers`, `bind-parser`, `parser-not-applicable`, `exclude-parser`, `prepare-discovery`, `next-probe`, `dispatch-discovery`, `discovery-step`, `pause-discovery`, `resume-discovery`, `import-pack`, `import-query-observations`, `reconcile`, and `record-failure`. See the [inventory action reference](INVENTORY.md) for complete required fields, exact selectors, review evidence, and budgets. `next-probe`, discovery resume, dispatch, step and pause accept optional `run_index`; omission selects the latest retained run. Carry the returned index into each follow-up action without reordering saved runs. A discovery error can carry `pointer: "/discovery_runs/0"` to identify a retained run in the saved inventory state. A retained-run guard points to the blocking run; errors specific to a selected checkpoint use that run. Global inventory, refresh and capability errors do not acquire a discovery-run pointer merely because the action selected a run. Brief failures preserve only a canonical numeric index and fixed guidance; detailed diagnostics remain available in full and summary modes.

The `import-pack` action accepts `pack_file`, optional `mode` (`append` or `update`), and optional request-only `supersede_runs` (defaults to `false`). Successful full replies include `action.mode`, `pack_updated`, `counts`, and `not_applied`. Summary replies include `not_applied_count` and at most 20 entries. A changed import that would displace discovery requires explicit acknowledgement with `supersede_runs: true`. When an import displaces runs, full and summary action replies include their sorted indices in `superseded_runs`. Brief action replies use counts, including `superseded_run_count` when applicable. Imported pack references may include `input_content_digest`, a lowercase SHA-256 of the submitted pack when it differs from the activated cumulative digest. Discovery references omit it, and it does not affect reconciliation.

`prepare-discovery` returns exit 2 with `INVENTORY_INPUT` before allocating a run when the predecessor is future-dated, a selected source has incompatible retained selectors, or a selected source's window starts too early to replace stale or undated negative evidence. Retained-run guards take precedence over these pack checks. Brief selector refusals use fixed text.

Resume and dispatch reject a resumable run whose verified predecessor differs from the current cumulative inventory, before reserving another search or changing its allowance. Valid `activating` and `activated` relays retain their existing recovery path. A verified completion against an earlier predecessor records inventory run status `superseded`, retains its artifacts and accounting, and neither promotes its pack nor supplies pending-work proof or a new capture boundary. It is terminal for unfinished-work counts. The action returns `status: "incomplete"` and `DISCOVERY_SUPERSEDED`, with the relay's verified `complete` status preserved; brief exposes only its fixed `incomplete` outcome. This completed action exits 0. Repeated polls/resume and duplicate accepted answers preserve the saved terminal outcome, accounting, failures and timestamps; duplicate answers still report `MCP_ALREADY_ANSWERED`. Artifact/path/hash corruption remains an error.

Supported producer/format inspection is independent of lifecycle permission. Selecting a foreign-producer discovery checkpoint returns `MCP_STATE_PRODUCER` with exit 2 before any inventory commit or retained-checkpoint association; it is not an incomplete action or a tampering claim. An unfinished unowned foreign resumable run also blocks guarded work with this code unless it qualifies under the earlier-release guard rule. Brief output retains this code, a canonical numeric run pointer and fixed producer-recovery guidance, without copying the detailed diagnostic. Bound foreign history follows the retained-run rules. An uncertain attempt blocks guarded work unless the last attempt qualifies under the earlier-release guard rule. A successful action recording relief returns a `DISCOVERY_ALLOWANCE_EXPIRED` info notice with a numeric `run_index`. See [upgrade and recovery boundaries](INVENTORY.md#start-and-resume).

Continuing-inventory actions are `set-refresh-settings`, `prepare-refresh`, `dispatch-refresh`, `settle-refresh`, `import-refresh`, `acknowledge-refresh-reminder`, `record-inventory-hints`, `acknowledge-inventory-work`, `acknowledge-inventory-notice`, and `exclude-inventory-work`. Refresh is manual; `automatic_refresh_available` is false and new preferences default to `automatic: false` with `lookback: "24h"` (alternatives `7d`, `14d`, `30d`). Saved automatic preferences remain readable but cannot enable new automatic preparation or dispatch. Retained attempts still require safe settlement and may import completed evidence. Manual refresh requires protected, exact-session evidence of a verified provider-enforced lifetime of 1–600 seconds. An advertised default or client timeout is insufficient. Dispatch is reserved before one call, unresolved work blocks another attempt, and only exact completed settlement permits result import. Strict capability/settlement shapes and separate reminder/work-notice handling are in [continuing inventory](INVENTORY.md#continuing-inventory-and-settings).

`batch` carries `actions`, 1 to 32 of `review-source`, `bind-parser`, `exclude-parser`, `parser-not-applicable`, `import-parsers` and `reconcile` with their usual fields, `reconcile` only last. The items are applied in order under one writer lock and saved together or not at all. The schema lists exactly these six kinds for a batch item. Before sending a batch to an executable you have not checked, confirm that `cqlmatic --schema inventory` offers the `batch` kind: 0.1.13 refuses it with exit 2 and changes nothing. See [several local actions at once](INVENTORY.md#send-several-local-actions-at-once).

Optional function actions are `prepare-capabilities`, `dispatch-capability`, `submit-capability`, `pause-capabilities`, and `resume-capabilities`. They require an activated field pack, use a separate finite allowance and retain fixed-case observations without changing field completion. They are off by default. Preparation/reservation do not execute a search; the authorized host relays the exact reservation. Unresolved termination blocks later inventory dispatch. See [the action fields and qualification limits](INVENTORY.md#optional-function-capability-checks).

A status request is `{schema_version:1, state?: STRING_OR_NULL, session_id?: STRING_OR_NULL}`. The optional stable host session ID scopes brief refresh reporting; it does not attest capability or authentication. Do not select state both in that request and with `--state`.

Output shapes differ:

- No-request `env inventory --json` and `status --json`: a direct `InventoryStatus`.
- Most inventory request actions return `{schema_version, state, status, action}`. `state` is the selected **state-file path string** where the producer stores `InventoryState`, `status` is `InventoryStatus`, and `action` is the action-specific result (possibly null).
- `next-probe` returns `{schema_version, action}` in every output mode.
- `batch` returns the same envelope. Its `action` is `{kind: "batch", commit, results}`, with one `{index, outcome}` per item in order. `commit` is `committed`, `unchanged`, `rolled-back` or `visible-durability-unconfirmed`, and an outcome is `applied`, `rolled-back`, `failed` (with `diagnostic`) or `skipped`. A failure that belongs to no single item, or an unconfirmed write, adds `action.diagnostic`. `status` describes the saved inventory.
- Human modes present summaries/prompts; use JSON for automation.

Successful inventory/status commands exit `0` even if work is pending, an API fetch retained partial evidence, or discovery output is failed/incomplete. Inspect `status` and `action` where present. A global failure returns `2`. A `batch` that saves nothing, or whose write is unconfirmed, also returns `2`, with its reply on stdout; a malformed batch request is a global failure.

### Single-probe output

Use `cqlmatic env inventory --state STATE --json --next-probe` with optional `--run-index N`. Supply `--probe-id ID` and `--result-file FILE` together to settle an answer. Use `--request` separately for the equivalent JSON action, whose optional `answer` has the same fields as an element of `discovery-step`'s `answers`.

The response has `schema_version: 1` and an `action` with `kind: "next-probe"`, the resolved `run_index`, `state`, `settled`, `dispatched`, the discovery `cost` object and fixed `next_action` text. `settled: true` confirms this call accepted the submitted result as discovery evidence. An accepted result can still end the run, for example by using the last search the run allows. The response then has `settled: true`, the resulting state and a diagnostic. `dispatched: true` accompanies a newly reserved `probe`. Rereading a reservation sets `dispatched: false` and leaves the charge intact. Once the run's time limit has passed, the reserved probe is not returned again. The response reports `MCP_DEADLINE` and keeps `outstanding_probe_id`. When a capped answer arrives after the deadline, the reply and later polls report both `SEARCH_ROW_LIMIT` and `MCP_DEADLINE`. `outstanding_probe_id` names the attempt that still needs settlement, including in `cancel-requested` and `cleanup-pending`. CQLmatic omits the field when no attempt is outstanding. A registered capture hook or pass-through settles the outstanding probe without a `next-probe` call when a completed search used exactly its arguments. It reserves nothing: the following `next-probe` without an answer reserves the next probe. An answer submitted for a probe that is already answered changes nothing: the response has `settled: false` and continues from the run's current state. When a new probe is reserved after an answer this call did not settle, `next_action` says that a hook or pass-through may be delivering results and to ask again without an answer after the search.

`probe` appears only in state `probe` and has `probe_id`, `tool` and exact `arguments`. CQLmatic builds those arguments from the run, including source names and values measured earlier when needed. `outstanding_probe_id` matches `probe.probe_id` whenever the response includes a probe, and `cost.searches.ready` counts the probe in this response. `state` is `complete` after successful completion or the run's recorded phase when no probe is available.

If the answer is refused, the response reports the resulting state and uses fixed diagnostic messages. Protected storage retains the same details and state changes as `discovery-step`. If choosing or reserving the next probe fails, CQLmatic attempts the same advancement as `discovery-step` with `answers: []`. With a verified checkpoint readback, it returns exit 0 with the observed state, `dispatched: false` and no probe. A reservation error after a visible write can still leave an outstanding ID. Diagnostics contain `code`, `severity` and program text, including a run index where applicable. Diagnostics that could carry input or system text use a fixed message. Command errors keep their existing diagnostics and brief error framing.

After exit 2 while submitting an answer for an outstanding reservation, call `next-probe` again without an answer on the same state and `run_index`. With no intervening lifecycle action, a matching `outstanding_probe_id` means that attempt still needs settlement. Resolve the error and submit the saved result again. A missing or different ID means the original attempt no longer needs settlement. Keep the original result and continue from the returned state. The run can retain the result as terminal evidence without accepting it as a measurement. If readback exits 2, resolve that error and use the same readback before resubmitting.

### Bounded brief output

`status --json --brief` and `env inventory --request FILE --json --brief` select a separate versioned agent projection. `--brief` requires `--json` and conflicts with `--summary`. Summary size can grow with inventory size. Full and summary source rows include optional `retained_restrictions` when the retained source carries `sourcetype` or `event_family`. The object includes those labels when present and the boolean `discovery_compatible`. Brief omits retained restriction labels and values.

A complete brief projection, including an action envelope and final newline, is at most 4,096 UTF-8 bytes. `next-probe` uses the same single-probe response with or without `--brief`, including exact executable arguments even when they exceed the byte limit. Brief omits the conflicting inventory source ID from selector-conflict replies. Brief runtime errors use bounded fixed diagnostics on stderr with exit 2. Known causes retain their diagnostic code. Use the exit code and diagnostic object to handle failures, including `INVENTORY_BRIEF_FAILED` for unclassified causes or brief output failures. If output fails, read status before repeating a write. Argument-parser errors/help retain their ordinary CLI behavior. The brief projection uses fixed codes, messages and next-action text, aggregate counts and at most five total opaque work/attempt references. Brief action replies include fixed diagnostics and a fixed next action when available. It omits freeform target/session text, paths, selectors and all executable/result payloads. Omission counts make the limited view explicit; safety blockers take precedence over routine references.

`notification_snapshot` is null when no unresolved, unacknowledged inventory work exists. Otherwise it contains `snapshot_sha256` and `count`, committing to the exact environment and complete sorted `(id, revision)` set. Submit `acknowledge-inventory-notice` with the unchanged `snapshot_sha256` only after the host has presented that aggregate notice; optional `defer` defaults false. The existing writer reloads and verifies the snapshot before reusing per-work acknowledgement. Stale or empty snapshots fail atomically. This digest identifies content, not authorization or a unique state instance; keep the explicit state selection unchanged. The bounded action response identifies the acknowledged digest/count separately from the remaining notification snapshot.

`refresh_reminder` is separate from `notification_snapshot`. It carries only fixed flags, a cycle digest and dates. When `show_startup_notice` is true, the foreground host presents the due hint before `acknowledge-refresh-reminder` with exact `cycle_sha256` and optional `defer` (default false). Explicit `defer: true` postpones the reminder for 30 days. The writer rejects stale cycles atomically. Delivery acknowledgement does not complete a census or resolve pending work. Clock skew suppresses the timed hint. Status/JSON/brief/piped output does not acknowledge delivery; the interactive menu acknowledges only after successful foreground output and flush when both stdin and stdout are terminals.

Background imports, status reads and generation/checking never acknowledge notification delivery. Acknowledgement/defer does not resolve inventory, promote grounding, authorize a search or suppress non-work safety flags. Retries can require a fresh brief after an acknowledgement or lost response; exactly-once user delivery is not guaranteed.

Use the single execution descriptor returned by `next-probe` directly. For other actions, a native owner needing exact MCP arguments consumes a protected full action capture locally. Keep raw results in protected storage and use bounded output for routine responses such as acknowledgement.

### InventoryStatus field dictionary

| Field(s) | Meaning |
| --- | --- |
| `schema_version`, `blank_canvas`, `message` | Contract version, absence of initialized evidence, and human summary. |
| `full_map_complete`, `selected_scope_complete`, `quick_scope_complete` | Distinct full-environment, Full selected-source and Quick selected-depth completion flags. Selected work never establishes a full map. |
| `discovery_mode`, `quick_depth` | Persistent Full/Quick preference and sources-only/sources-and-fields depth. |
| `refresh_settings`, `automatic_refresh_available`, `refresh_attempts`, `refresh_blocked` | Lookback and retained automatic preference; automatic availability is false. Exact session/target/argument hashes, automatic flag, dispatch time, retained lifetime and known termination remain visible; reserved is not backend-running. Unresolved refresh blocks another attempt. |
| `capabilities`, `capability_blocked` | Separate fixed-case run summaries and an unresolved capability-search termination flag. Full/summary output retains case outcomes; brief output exposes `capability_run_count` and `capability_blocked` without case payloads. |
| `refresh_reminder` | Optional bounded `{due, show_startup_notice, cycle_sha256, last_successful_window_end, due_at, clock_skew}`. Timed status follows the later of first confirmation and the exact target's latest qualifying uncapped census window end. No raw target or result content. |
| `pending_inventory`, `inventory_notification_count`, `settings_attention` | Bounded source/field/type work revisions, pending notices, and persistent Settings marker. Acknowledgement/defer is not completion. |
| `target`, `environment`, `target_scope`, `context` | Target progress, exact identity, `full-environment`/`selected-sources`, and confirmed context. |
| `scouting`, `parser_catalog`, `reconciliation` | Progress of the corresponding prerequisites. |
| `completion_detail` | What those progress words cover and what is still open: the review time of the current evidence fingerprint, a note that a recorded review is not a completion claim, open counts for expected sources, catalog capture, a prepared census awaiting capture, the latest census size, and `next_action`, the first listed step. See [read the checklist](INVENTORY.md#read-the-checklist). |
| `parser_count`, `parser_page_count` | Retained parser definition/catalog page counts. |
| `parsers` | Parser summaries: `id`, `name`, `sha256`, `retrieved_at`, `declared_fields`, `fields_to_tag`, `array_fields`, and required `review_lines`. |
| `scouting_evidence` | Saved plan, raw digest, retrieval date, row count; scoped observations rather than tenant-wide absence proof. A saved scout makes scouting done without automatic empty-result or 10,000-row cap rejection. |
| `unclassified_parser_ids` | Catalog IDs still needing bindings or explicit exclusions. |
| `sources` | Per-source evidence and next actions, described below. |
| `discovery_runs`, `parser_runs` | Run windows/budgets, progress, paths/hashes, and retained outcomes/failures. |
| `failures` | Recorded history; not necessarily a current completion blocker if later evidence resolves it. |
| `next_actions` | Human guidance to remaining work. Steps come first, in the order `completion_detail.next_action` and the reconcile `next_action` follow; notes with recorded detail follow them. |
| `covered_queries_usable` | Currently always true in returned status; **not** proof a particular query has enough evidence. |
| `active_pack` | Optional `path`, `content_digest`, `imported_at`, and associated `discovery_run`. Matching confirmed inventory is reused automatically; explicit `--pack` selects a primary snapshot. |

JSON progress values are `done`, `pending`, `review`, `unknown`, and `n-a`; human summaries display `DONE`, `PENDING`, `REVIEW`, `UNKNOWN`, `N/A`. Text status names the milestone instead of `DONE` for scouting (`census saved`), the parser catalog (`captured`) and reconciliation (`evidence review current`), and shows a completed discovery run as `run complete, pack activated`.

Each source status has `source_id`, `selectors`, optional `scouted_events`, `review`, `binding`, overall `status`, separate `interpretation`, `parser`, `measurements`, `observation`, `fields`, `record_kinds`, `unobserved_declared_fields`, and `next_actions`. Each field has `field`, booleans `declared`, `observed`, `measured`, progress `status` and `values`, `values_requested`, `values_saved`, `evidence_dates`, and `not_reobserved`. `values_saved` is true when the retained pack holds at least one observed value for the field. Measured zero can complete a measurement while leaving a declaration unobserved and needing review.

The inventory state contains `schema_version`, `format`, optional `field_evidence_schema_version`, `created_at`, `updated_at`, `discovery_mode`, `quick_depth`, `unverified_leads`, `query_observations`, optional `captured_observations`, `refresh_settings`, optional `refresh_reminder`, `refresh_attempts`, optional `capability_runs`, `source_metadata`, `pending_inventory`, `target`, `scout_plan`, optional `scout_plan_pending`, `scouting`, `sources`, `parser_pages`, `parsers`, `parser_exclusions`, `discovery_runs`, `parser_runs`, `packs`, `active_pack`, `reconciled_evidence_sha256`, `reconciled_at`, and `failures`. New writes use inventory state version **4**; request and status contracts remain **1**. Readable version-1–3 states retain their field evidence and original reconciliation basis through migration; they cannot attest capability runs. The producer-managed `field_evidence_schema_version` preserves that earlier basis. Capability history is separate from the field-evidence reconciliation fingerprint. These are sensitive snapshots and fingerprints, not flags to edit to force completion. Status validates referenced packs; missing/changed snapshots can make status fail. See [state compatibility](INVENTORY.md#bounded-agent-handoff).

Each `capabilities` entry reports `run_id`, `execution_context_id`, `session_id`, `max_queries`, `attempts`, `max_seconds`, `paused`, `termination_pending`, and `cases`. Each case reports `case_id`, `case_revision`, optional `last_outcome` (`verified` or `unknown`), `last_attempt_id`, `retrieved_at`, and `result_sha256`. An unattempted case has no outcome. A retained `verified` outcome describes its historical fixed result, not current eligibility: generation/checking reevaluate the exact binding and expiry, and a newer unknown attempt supersedes an older positive.

Both Full and Quick generation/checking can queue bounded unverified source/input-field hints. Existing Quick leads remain; assessment writes neither hints nor leads. Pending work retains exact selector namespaces and revision identity. Refresh only adds scoped metadata/pending work; it does not ground fields, infer parser assignments or complete inventory.

In Full mode, `prepare-scout` emits MCP `repository:"search-all"` and requires authorization for that target. In Quick mode, it emits the selected `environment.view` and a finite host budget. Relay the exact emitted arguments for the authorized target. See [scouting boundaries](INVENTORY.md#action-inputs-and-evidence-boundaries).

## Packs and discovery outputs

A native pack contains `format_version`, `producer_version`, `profile_id`, `knowledge_revision`, `environment`, `created_at`, `content_digest`, and `data`. Data holds `provenance`, `sources`, `fields`, `values`, and `annotations`. Their typed records, evidence qualification, timestamps, references, integrity checks, and legacy compatibility are described in [environment packs](ENVIRONMENT.md). The digest is integrity checking, not a signature. Filtered inspection output is not a substitute native snapshot.

`DiscoverRequest` fields and defaults are documented in [discovery options](ENVIRONMENT.md). The native API result `DiscoverReport` contains `schema_version`, `environment`, `status`, `active_pack_updated`, `captures`, and `diagnostics`. Captures retain their exact `probe`, retrieval time, and `receipt`; search receipts distinguish results, completeness, and cleanup. Complete evidence and activation are separate stages. Native discovery exits `0` only for complete activated output; late pack/receipt persistence can fail after replacement.

The offline discovery plan contains `schema_version`, `environment`, `is_live:false`, `executes_searches:false`, `initial_probes`, `deferred_stages`, `max_queries`, and `max_seconds`. Deferred stages depend on measurements; it is not a complete list of future queries. When the request names its sources, `searches` adds `expected_low`, `limit` and, for sources-only and capped field runs, `expected_high`: the expected number of searches for sources that hold events, counted from the same stages and never above `max_queries`. A census request has no figure, and an uncapped field run has no upper figure.

Durable discovery step output contains `mode` (`mcp` or `native`), `status`, `answered`, `max_queries`, `cost`, `probes`, `active_pack_updated`, and `diagnostics`. `answered` counts accepted captures, not remote dispatch attempts or completed activation. `cost.searches` reports `used` (searches charged to the run, whatever their outcome), `ready` (searches this response asks for), `limit`, and the plan's `expected_low` and `expected_high` where they exist; `cost.answer_bytes` is the size of every answer handed back so far, accepted or not; a cleanup report is not an answer, and a native run, which fetches its own answers, omits the field. Brief action output keeps `cost` and, while probes are ready, adds a `next_action` that says how to list their arguments. The figures are counts, not an estimate of time, tokens or price. In a new format-3 MCP run, reserve one exact `probe_id` with `--dispatch` before its host call; consume its exact `mcp.tool`/`mcp.arguments`, then submit the entire matching original result before another dispatch. Accepted captures replay locally without remote re-execution; cancelled retries consume another attempt. Unrecognized rejected format-3 results retain their exact bytes/hash and diagnostic separately from accepted evidence, leaving the same attempt in `cleanup-pending`. An error or client timeout is not remote terminal proof. An acceptable original successful result or exact bound host cleanup settlement can resolve that attempt; rejected history and charges remain retained. Format 3 retains immutable request/target/window/path/starting-pack bindings, attempt charges, native known-job IDs and original active-time allowance; safe pauses exclude idle time without renewing budgets. Compatible format-2 checkpoints retain their legacy multi-answer, absolute-deadline and terminal-failure rules.

Output status `complete` exits `0`; `needs-probes`, `paused`, `cancel-requested`, `cleanup-pending` and `activation-blocked` exit `3`; failures exit `2`. Persisted states also include `activating` and `activated`. Cancellation is separate from provider termination: use exact bound host settlement evidence for MCP, and real saved-job cleanup for native execution. The checkpoint is producer-managed sensitive storage, not an editable request. See [cancel/resume contracts](ENVIRONMENT.md#cancel-and-resume-discovery) and [MCP lifecycle](MCP-COMPANION.md) for flags, strict settlement fields and frozen activation recovery.

## Result assessment

Send `{schema_version:1, id?, query:STRING, result:ENTIRE_SAVED_MCP_JSON, context?:{}}`. A valid request includes the entire payload, not only rows. `--query-file`/`--result` is an alternative adapter, not this wrapper. Assessment requests reject duplicate object keys, including inside `result`, and require one JSON document. Saved-result files separately support identical host-rendered repeats.

All assessment modes reuse a matching inventory's active pack unless an explicit `--pack` is selected. `--inventory-state FILE` selects a different protected inventory without changing the JSON contract. Assessment never captures leads or interprets saved-query ledger counts as population rates. See [inventory selection and failures](INVENTORY.md#imports-and-query-writing).

The result is `{schema_version,id,outcome,query_sha256,rows,caps,columns,findings}`. It is **not** `Analysis`. Outcomes, cap defaults, null statistics, normalization, hashes, and recovery are detailed in [result assessment](RESULT-ASSESSMENT.md). Runtime accepts only version `1`.

## Streams and errors

`generate --json`, `check --json`, and `assess --json` emit compact newline-terminated objects on stdout. JSONL processes sequential nonblank records and emits one result per processed record; blank means ASCII whitespace only. Record line numbers include skipped blanks. An empty stream succeeds with no rows.

Malformed or oversized records produce an error result with null ID and a `/line/N` diagnostic pointer. A successfully decoded but unsupported-version request retains its ID. Oversized lines are drained before the next line. Generation/checking rows are `Analysis`; assessment rows are `AssessResult`. Each has its own diagnostics/findings and outcome vocabulary. JSONL returns the highest record exit code, and continues after record errors.

Configuration/pack/storage/stream-read failures can terminate the process outside record handling. Global application failures emit one standalone `Diagnostic` on stderr and return `2`, even if JSON was requested. Argument parsing happens before that handling and can emit plain-text help/errors. Result-output failures such as closed pipes report an error and exit `2`. Consumers must preserve already emitted rows and still detect a failing process; stdout alone is insufficient.

Human generation prints only an available query; unsuccessful generation can print no stdout. Human checking prints its assessment, not the original query. Human generate/check diagnostics go to stderr. Human assessment findings and summary go to stdout. Catalog and low-level environment commands output their own JSON objects/arrays; no universal `Analysis` wrapper exists.

## Budgets and compatibility

| Input | Limit / rule |
| --- | --- |
| Generate/check/assess request or JSONL record; raw CQL file | 1 MiB adapter limit; query parser adds 66,000 UTF-16 units, depth 64, 16,384 nodes. |
| English text | 16 KiB; Boolean nesting 32. |
| Setup request/state | 256 KiB; saved host-discovery report 1 MiB. |
| Inventory request/state; pack; MCP relay state | 64 MiB for each bounded storage/input; see lifecycle guides for additional record counts. |
| Raw MCP result file | 8 MiB. Discovery rejects 10,000 or more rows; assessment marks that row count inconclusive. Assess inline requests still have the outer 1 MiB limit. |
| Credentials | 16 KiB, protected regular non-symlink file. |

Deadlines, source counts, page limits, field batches, and search budgets add independent constraints; see [coverage](COVERAGE.md) and [environment details](ENVIRONMENT.md). An individually permitted answer does not guarantee the accumulated relay state fits its budget.

Use only the versions currently documented by the binary. Compatible legacy packs preserve their original identity/digest; compatibility does not upgrade historical or declared evidence. MCP state format changes can require a new run. No promise of future diagnostic-code, internal-state, or Rust SDK compatibility is made. Use exported contracts and explicit error handling rather than assuming every release accepts the same stored state.

Recognized final answers from the hosted search tool end a format-3 MCP attempt as `timed-out`, `not-started` or `failed-after-start`; a completed list that cannot supply the required measurement ends it as `completed-unusable`. Exact terminal bytes and their hash are retained separately from accepted evidence. The run pauses, the attempt stays charged, and an explicit resume is required before another dispatch. Resume emits the same unaccepted probe. A cutoff does not prove the remote job stopped: at most one leftover job per cut-off attempt may overlap later searches. A missing answer or client wait without a tool answer keeps the attempt unresolved and blocks further dispatch. An unrecognized error shape or wording, malformed or incomplete response leaves it in `cleanup-pending`. Resolve that same attempt with an acceptable original successful result or exact bound cancellation report before resuming. Under the retained-run rules, a format-3 MCP checkpoint from 0.1.6 or 0.1.7 with exhausted active time and one distinct final answer can stop blocking other inventory searches, while lifecycle actions require its producing release. See [hosted search outcomes](MCP-COMPANION.md).
