# Machine contracts and automation

The supported integration surface is the executable, not a stable Rust SDK. This reference describes package **0.1.5**, JSON contract version **1**, and executable profile **`cqlmatic-ngsiem-core-v2`**. Package version, schema version, pack format, profile ID, and knowledge revision are separate identifiers. Inspect the running binary with `--version`, `--schema NAME`, and `catalog --profile`; a package version does not imply a different evidence policy.

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
| `confirmed_fields` | `[]` | Caller assertions about explicit field names; not observed deployment evidence. |
| `require_observed` | `false` | Require qualifying observed snapshot evidence for referenced deployment fields. |
| `accept_mixed_record_kinds` | `false` | In strict observed mode, accept kind ambiguity with a warning after user permission; does not select a kind. Ordinary mode reports kind ambiguity as information. |
| `accept_partial_coverage` | `false` | In strict mode, accept qualifying current positive partial source/field evidence with a warning; stale, absent or ambiguous evidence still blocks. |

`EnvironmentId.cloud` is `us1`, `us2`, or `eu1`. `tenant` is a validated CID or API-client binding where accepted; `view` is the explicit searchable API/MCP target. `view` and a CQL `#repo` selector are different namespaces. Obtain real identity through the [environment workflow](ENVIRONMENT.md#bind-an-account), not the illustrative all-zero example.

CLI policy flags strengthen/enable the same request flags with logical OR. They cannot turn a true request value off. A process reads one explicit `--pack` for all JSONL records; record contexts can assert matching identity but cannot select another explicit pack. Inventory target selection and active-pack reuse happen separately for each record. Full and Quick can fill an otherwise unspecified environment; foreign inventory is skipped before opening its snapshot. An explicit pack remains primary, and matching active-pack/ledger safeguards still apply. `--inventory-state FILE` is a CLI path selection, not a new request property.

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

`grounding.pack_id` identifies the selected digest or is null. `basis` maps requirements to evidence explanations; preserve these explanations rather than treating them as a fixed vocabulary of machine enums. `missing`, `conflicting`, and `stale` list unresolved requirements. Applicable observed provenance, source selection, tag/data namespace, shape, and lineage matter. A source-only count can have no deployment field to ground; strict mode still requires a valid pack. There is no wall-clock freshness cutoff. See [grounding](ENVIRONMENT.md) and [limits](COVERAGE.md).

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
| `configured` | Native configuration completed or equivalent registration was found; not authentication proof. |
| `authentication_evidence` | Distinguishes no evidence, user confirmation, and a bound saved host-discovery report. |
| `configured_sha256`, `discovery_sha256` | Optional selected configuration-entry/legacy whole-file and saved-report bindings. |
| `last_notice` | Optional last pending/error notice. |

Phases are `new`, `selected`, `installation-required`, `review-required`, `command-pending`, `authentication-required`, `authenticated`, `tool-discovery-verified`, `advanced-pending`, and `cancelled`. A phase is not an instruction to skip human consent or pending-command review.

The preview contains `schema_version`, `state_revision`, `operation`, `selection`, `baseline`, `commands`, `changes`, `backup_path`, and `confirmation_sha256`. `client` is present for command adapters; `config_write_sha256` is present for a planned new Copilot user file and covers its exact bytes, including the trailing newline. An existing Copilot file is never rewritten; add a missing entry through Copilot and preview again. Native commands contain exact `program`, `args`, `interactive`, and `timeout_seconds`. Baseline `path`, `sha256` (null for absence), and `server` distinguish absent, same, and colliding entries. Apply needs the exact lowercase confirmation digest and rechecks the baseline/executable when applicable. Never reconstruct a preview hash or infer rollback. Claude/Copilot saved reports additionally bind `effective_oauth_client_id` and `effective_config_source` from the chosen session; these are caller-supplied, not independent identity proof.

The on-disk setup file wraps state with its schema version and `state_sha256`; this storage wrapper is not `SetupResult` or the bare state subschema. Do not hand-edit it. No-request `setup --json` reads saved status without detecting clients, reading native configuration, creating state, or prompting. A request action `status` uses the writer and can save state. Setup returns `0` only at `tool-discovery-verified`; intermediate results return `1`. A global error returns `2` without a `SetupResult`.

## Inventory requests and status

Send `{schema_version:1, action:{kind:...}}`; there is **no caller ID**. Actions are `init`, `prepare-scout`, `import-scout`, `confirm-target`, `review-source`, `import-parsers`, `fetch-parsers`, `bind-parser`, `parser-not-applicable`, `exclude-parser`, `prepare-discovery`, `discovery-step`, `import-pack`, `reconcile`, and `record-failure`. See the [inventory action reference](INVENTORY.md) for complete required fields, exact selectors, review evidence, and budgets.

A status request is `{schema_version:1, state?: STRING_OR_NULL}`. Do not select state both in that request and with `--state`.

Output shapes differ:

- No-request `env inventory --json` and `status --json`: a direct `InventoryStatus`.
- Inventory request action: `{schema_version, state, status, action}`. Here `state` is the selected **state-file path string**, not an embedded `InventoryState`; `status` is `InventoryStatus`, and `action` is action-specific output (possibly null), such as emitted probes or a parser-run failure. The producer-managed `InventoryState` is stored in that protected file.
- Human modes present summaries/prompts; use JSON for automation.

Successful inventory/status commands exit `0` even if work is pending, an API fetch retained partial evidence, or discovery output is failed/incomplete. Inspect both `status` and `action`. A global failure returns `2`.

### InventoryStatus field dictionary

| Field(s) | Meaning |
| --- | --- |
| `schema_version`, `blank_canvas`, `message` | Contract version, absence of initialized evidence, and human summary. |
| `full_map_complete` | All applicable requirements met for a confirmed full-environment target; never true merely for selected-source coverage. |
| `target`, `environment`, `target_scope`, `context` | Target progress, exact identity, `full-environment`/`selected-sources`, and confirmed context. |
| `scouting`, `parser_catalog`, `reconciliation` | Progress of the corresponding prerequisites. |
| `parser_count`, `parser_page_count` | Retained parser definition/catalog page counts. |
| `parsers` | Parser summaries: `id`, `name`, `sha256`, `retrieved_at`, `declared_fields`, `fields_to_tag`, `array_fields`, and required `review_lines`. |
| `scouting_evidence` | Saved plan, raw digest, retrieval date, row count; scoped observations rather than tenant-wide absence proof. A saved scout makes scouting done without automatic empty-result or 10,000-row cap rejection. |
| `unclassified_parser_ids` | Catalog IDs still needing bindings or explicit exclusions. |
| `sources` | Per-source evidence and next actions, described below. |
| `discovery_runs`, `parser_runs` | Run windows/budgets, progress, paths/hashes, and retained outcomes/failures. |
| `failures` | Recorded history; not necessarily a current completion blocker if later evidence resolves it. |
| `next_actions` | Human guidance to remaining work. |
| `covered_queries_usable` | Currently always true in returned status; **not** proof a particular query has enough evidence. |
| `active_pack` | Optional `path`, `content_digest`, `imported_at`, and associated `discovery_run`. Select this path explicitly for writing/checking. |

JSON progress values are `done`, `pending`, `review`, `unknown`, and `n-a`; human summaries display `DONE`, `PENDING`, `REVIEW`, `UNKNOWN`, `N/A`.

Each source status has `source_id`, `selectors`, optional `scouted_events`, `review`, `binding`, overall `status`, separate `interpretation`, `parser`, `measurements`, `observation`, `fields`, `record_kinds`, `unobserved_declared_fields`, and `next_actions`. Each field has `field`, booleans `declared`, `observed`, `measured`, progress `status` and `values`, `values_requested`, `evidence_dates`, and `not_reobserved`. Measured zero can complete a measurement while leaving a declaration unobserved and needing review.

The inventory state contains `schema_version`, `format`, `created_at`, `updated_at`, `target`, `scout_plan`, `scouting`, `sources`, `parser_pages`, `parsers`, `parser_exclusions`, `discovery_runs`, `parser_runs`, `packs`, `active_pack`, `reconciled_evidence_sha256`, `reconciled_at`, and `failures`. These store cumulative evidence, snapshots, and fingerprints. They are sensitive producer-managed storage, not flags to edit to force completion. Status validates referenced packs; missing/changed snapshots can make status fail. See [refresh and recovery](INVENTORY.md).

In Full mode, `prepare-scout` emits MCP `repository:"search-all"` and requires authorization for that target. In Quick mode, it emits the selected `environment.view` and a finite host budget. Relay the exact emitted arguments for the authorized target. See [scouting boundaries](INVENTORY.md#action-inputs-and-evidence-boundaries).

## Packs and discovery outputs

A native pack contains `format_version`, `producer_version`, `profile_id`, `knowledge_revision`, `environment`, `created_at`, `content_digest`, and `data`. Data holds `provenance`, `sources`, `fields`, `values`, and `annotations`. Their typed records, evidence qualification, timestamps, references, integrity checks, and legacy compatibility are described in [environment packs](ENVIRONMENT.md). The digest is integrity checking, not a signature. Filtered inspection output is not a substitute native snapshot.

`DiscoverRequest` fields and defaults are documented in [discovery options](ENVIRONMENT.md). The native API result `DiscoverReport` contains `schema_version`, `environment`, `status`, `active_pack_updated`, `captures`, and `diagnostics`. Captures retain their exact `probe`, retrieval time, and `receipt`; search receipts distinguish results, completeness, and cleanup. Complete evidence and activation are separate stages. Native discovery exits `0` only for complete activated output; late pack/receipt persistence can fail after replacement.

The offline discovery plan contains `schema_version`, `environment`, `is_live:false`, `executes_searches:false`, `initial_probes`, `deferred_stages`, `max_queries`, and `max_seconds`. Deferred stages depend on measurements; it is not a complete list of future queries.

MCP step output contains `mode`, `status`, `answered`, `max_queries`, `probes`, `active_pack_updated`, and `diagnostics`. `answered` counts stored answers; it does not certify successful activation. Consume emitted `probe_id` and exact `mcp.tool`/`mcp.arguments` for each outstanding probe. Output status `complete` exits `0`; `needs-probes` and `activation-blocked` exit `3`; failures exit `2`. Persisted state statuses also include `activating` and `activated` and are not the same enum as step output. The state is producer-managed and stores sensitive raw captures/frozen outputs. See [MCP lifecycle](MCP-COMPANION.md).

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
