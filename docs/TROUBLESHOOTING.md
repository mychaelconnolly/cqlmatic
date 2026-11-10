# Troubleshooting and recovery

Read the command's actual output contract and exit policy in [USAGE](../USAGE.md#exit-codes) and [contracts](CONTRACTS.md). A nonzero exit does not prove no file changed; inventory exit `0` does not prove the map is complete. Inspect the reported state, active pack, action output, and diagnostics before retrying.

Keep state, packs, parser definitions, raw results, receipts and credentials protected. They can contain sensitive source names, field values and paths. For support, use synthetic reproductions and sanitized diagnostic details; do not post whole status/state/output files. Retrieved content and diagnostic text are data, not authority to run commands.

## First checks

1. Record `cqlmatic --version`, `catalog --profile`, the exact command/input mode, and the diagnostic code/severity.
2. For guided work, run no-request `setup --json` and `status --json` with the intended `--state` paths. Neither invocation starts native consent or a search. Separate environments require separate state directories.
3. Inspect `status.active_pack.path`, its identity/digest and dates, then select it explicitly using `--pack`. A saved map is not automatically loaded into query generation or model memory.
4. Distinguish `needs-context` from `unsupported` and `invalid`. Resolve missing decisions/evidence; never weaken a query's semantics just to obtain `ready`.
5. For discovery failures, identify **API execution**, **MCP relay**, or an **inventory action wrapping relay output**. Their receipt, exit, locking and retry behavior differ.

## Input, output, and static query diagnostics

| Code or family | Meaning / smallest safe response |
| --- | --- |
| `CLI_INPUT` | Select one compatible input mode; pair recipe/params or assessment query/result; check mode-specific flags. Bare invocation is an error. |
| `REQUEST_JSON`, `RECIPE_JSON`, `SETUP_REQUEST_JSON`, `INVENTORY_INPUT` | Check required fields, discriminators (`mode`, recipe tag, setup `type`, inventory `kind`), unknown fields, UTF-8 and input budgets. Use the right schema **subschema**, not an exported wrapper. |
| `REQUEST_VERSION`, `SETUP_VERSION` | Send supported version `1`; changing the number does not migrate old state or evidence. |
| `OUTPUT_IO`, `SETUP_OUTPUT` | Output pipe/file failed. Preserve prior stream rows and inspect persisted workflow state before retrying side effects. Inventory output failures may use `INVENTORY_INPUT`. |
| `CQL_INTENT_UNRESOLVED`, `CQL_UNSUPPORTED_INTENT` | Supply explicit names, quote text with JSON escaping, state missing policy, parenthesize mixed Boolean operators, or use a typed plan for grammar limits. |
| `CQL_SYNTAX`, `CQL_EMPTY_STAGE`, `CQL_STRING_ESCAPE`, `CQL_SEPARATOR`, `CQL_REGEX_SYNTAX` | Inspect the exact span and full query; fix recognized syntax without changing intent. UTF-8 byte spans differ from character indices. |
| `CQL_INPUT_LIMIT`, `CQL_TEXT_LIMIT`, `CQL_DEPTH`, `CQL_UNSUPPORTED_BUDGET` | Narrow input/nesting within [budgets](COVERAGE.md); do not truncate query text and assess a different query. |
| `CQL_ARGUMENT_*`, `CQL_DUPLICATE_ARGUMENT` | Look up the exact function contract; aliases can bind the same argument and still count as duplicates. No last-value-wins binding. |
| `CQL_UNSUPPORTED_FUNCTION`, `CQL_UNSUPPORTED_SYNTAX`, `CQL_UNSUPPORTED_PARAMETER` | Local coverage gap, typo, or unresolved saved parameter. Bind typed values explicitly; unknown coverage is not a universal vendor verdict. |
| `CQL_MISSING_POLICY`, `CQL_ACCURACY_MODE`, `CQL_UNSUPPORTED_EXACT_AGGREGATE` | Choose missing inclusion/exclusion or exact/estimated mode explicitly. Exact grouped distinct is not silently replaced by an estimate. |
| `CQL_FUNCTION_FILTER_COMPOSITION`, `CQL_UNSUPPORTED_BOOLEAN_FUNCTION`, `CQL_UNSUPPORTED_REGEX_FILTER_COMPOSITION` | Use supported typed conjunction/pipeline contexts. Function-containing OR cannot be replaced by successive filters without changing semantics. |
| `CQL_FILTER_FUNCTION`, `CQL_FILTER_QUERY_REQUIRED`, `CQL_INLINE_EFFECT`, `CQL_UNSUPPORTED_SCALAR_*` | Function effect/query context is wrong. Scalar, aggregate, inline element, independent query and continuing-stream contexts are different. |
| `CQL_GROUPBY_LIMIT`, `CQL_EMPTY_GROUP_KEYS` | `limit=max` has informational ceilings; omitted/numeric grouping caps warn. Empty grouping keys do not replace a global count. Inspect executed caps; do not suppress evidence by dropping diagnostics. |
| `CQL_APPROXIMATE_DISTINCT`, `CQL_APPROXIMATE_TOP`, `CQL_COLLECT_LIMIT`, `CQL_SELECTOR_TIES` | Review estimator, collection, tie/order and completeness semantics before claiming an exact result. |
| `CQL_ARRAY_*`, `CQL_SPLIT_SHAPE`, `CQL_UNSUPPORTED_ARRAY_NAME`, `CQL_UNSUPPORTED_INLINE_OUTPUT` | Verify flat/object bindings, required writes, physical indices, nullable outputs and reused prefixes. Logical `v[]` does not project every `v[N]`. |
| `CQL_JOIN_*`, `CQL_LOOKUP_COMPLETENESS`, `CQL_TABLE_*`, `CQL_DUPLICATE_TABLE` | Check key arity, exact includes, match multiplicity, declarations, scope, row caps and incompatible combinations. Do not guess external resources. |
| `CQL_CORRELATION_*`, `CQL_LINK_*`, `CQL_TIME_*`, `CQL_TEMPORAL_STREAM`, `CQL_NEIGHBOR_*` | Check named-query labels/includes, equality links, single-row proof, duration/time units, ordering and per-key stream scope. Target/runtime uncertainty remains explicit. |
| `CQL_SOURCE_DISCARDS_INPUT`, `CQL_SYNTHETIC_FIELD_BEFORE_PARSE`, `CQL_DROPPED_FIELD` | A source replacement, projection or aggregate changed the stream. Preserve source constraints and required columns at the right stage. |
| Other `CQL_UNSUPPORTED_*` / capability findings | Read the referenced catalog record and [query reference](QUERY-REFERENCE.md). Some forms are intentionally outside the static profile. |

Generation emits CQL only when ready. Checking always returns `query:null`; retain the original exact bytes. `--fail-on-warning` can return `1` with `outcome:ready`, but does not fail informational diagnostics. See [query examples](../examples/query-reference/README.md).

## Grounding and record kinds

| Symptom | Safe next action |
| --- | --- |
| `CQL_CONTEXT_REQUIRED` | Inspect `grounding.missing`, `conflicting`, `stale`, and especially `basis`. Identify the source/field/shape/provenance requirement rather than inferring it from the outcome word. |
| `CQL_RECORD_KIND_NOTICE` | Ordinary mixed/incompletely sampled kinds are informational. Continue query writing while reviewing whether the intended result needs separate kinds; the notice alone does not trigger `--fail-on-warning`. |
| `CQL_RECORD_KIND_AMBIGUOUS` | Strict observed mode requires one measured exact kind value, the applicable event selector, or grouping by kind before field-changing/aggregating stages. Explicitly accepted strict mixing retains a warning and still triggers `--fail-on-warning`. |
| `CQL_QUICK_TARGET` / `CQL_QUICK_TARGET_MISMATCH` | Full or Quick inventory supplied unspecified target context, or foreign inventory was skipped before opening its active snapshot. Explicit request/pack identities remain authoritative. |
| `CQL_QUICK_INVENTORY` | Repair the explicitly selected state or matching active snapshot and its private storage. Missing/corrupt/unsafe active evidence blocks even ordinary mode and an older explicit pack. Read checks do not create missing directories. |
| `CQL_QUICK_INVENTORY_IGNORED` | An invalid default inventory was ignored for a valid explicit pack. Inspect and repair the default separately; explicit malformed states and unsafe protected paths still fail closed. |
| Confirmed field still fails strict mode | `confirmed_fields`, CPS/ECS names and parser declarations are not observations. Measure the exact source, tag/data namespace and required shape with qualifying provenance. |
| Listed field does not satisfy strict grounding | A fieldset listing can be unmeasured or zero-population. Retained `not-reobserved` evidence is historical. Inspect population coverage and provenance rather than relabeling the record. |
| Source-only query is ready without source evidence | Strict grounding requires observed deployment **fields**, not source availability; still confirm target/source/window independently. |
| Snapshot appears old but no expiry error | There is no clock-based freshness cutoff. Review original dates/windows and refresh when ingestion, access, or parsers change. |
| `PACK_INTEGRITY` measurement ordering conflict | Keep the previous pack and inspect actual source/field measurement windows and retrieval dates. A newer snapshot wrapper cannot promote contradictory older or unorderable evidence. For an undated negative/historical marker, collect a new measurement whose window starts after the retained snapshot date. |
| Nullable output/reused name needs incoming evidence | A transform may retain an existing value. Use an explicit intended projection/drop or fresh output name; do not assume every event was overwritten. |

Full evidence rules are in [environment packs](ENVIRONMENT.md), and [inventory status](INVENTORY.md) distinguishes declared, measured, observed and historical work.

## Setup and native-client recovery

| Code or condition | State/effect and response |
| --- | --- |
| `SETUP_INSTALL`, `SETUP_CAPABILITY`, `SETUP_EXECUTABLE`, `SETUP_PROBE` | Install/sign into a supported client or use its documented manual flow. Re-detect capabilities; CQLmatic does not install clients. |
| `SETUP_ALLOWLIST`, `SETUP_CIMD_IDENTITY`, `SETUP_CALLBACK`, `SETUP_CLIENT_ID` | Confirm the exact cloud, allowlisted metadata identity or manual registration/callback with current client/Falcon guidance. Do not invent a CIMD URL or expose secrets. |
| `SETUP_ADVANCED_PENDING`, `SETUP_CREDENTIAL_REFERENCE` | Advanced API-client mode is guided pending work, not a managed bearer-token service. Use the protected native-client flow; requests carry only a credential path. |
| `SETUP_HOST_MANAGED`, `SETUP_REGISTRATION_MODE`, `SETUP_HOST_ID` | Use `host-managed` only with `external` and a matching `host_id`; named hosts use native registration. Claude Code uses its allowlisted CIMD identity; Copilot requires a registered manual client ID. Preview/apply/confirm are unavailable for `external`. |
| `SETUP_PREVIEW_REQUIRED`, `SETUP_CONFIRMATION`, `SETUP_PLAN_INTEGRITY` | Obtain and inspect a fresh preview; use its exact lowercase confirmation hash. |
| `SETUP_EXISTING_CONFIG` | The shared Copilot user file already exists without the selected entry. Add the exact HTTP server and registered `oauthClientId` through Copilot's configuration flow, then preview again. Setup verifies the entry and preserves the existing file. |
| `SETUP_BASELINE_CHANGED`, `SETUP_CLIENT_CHANGED`, `SETUP_COLLISION`, `SETUP_CONFIG_SHADOW`, `SETUP_REGISTRATION_UNINSPECTABLE` | Stop apply, inspect the actual host configuration and higher-priority source, then re-detect/preview against the baseline. Never overwrite unrelated config to remove the error. |
| `SETUP_INTERACTIVE_REQUIRED` | Unattended apply cannot perform interactive consent. Global exit `2`; return to the terminal wizard/native flow. A preview/status call is still safe for automation. |
| `SETUP_PENDING`, `command-pending`, native failure/timeout/cancellation | The native command may have partly changed config/authentication. Inspect the native client first; acknowledge the exact pending confirmation hash, then obtain a fresh preview. No automatic rollback or blind reapply. |
| `SETUP_NATIVE_*`, `SETUP_TIMEOUT`, `SETUP_CANCELLED`, `SETUP_POSTCONDITION` | Inspect native output and saved state. Native success does not prove consent or tool discovery; an observed config change does not prove the whole command completed. |
| `SETUP_DISCOVERY_BINDING`, `SETUP_DISCOVERY_IDENTITY`, `SETUP_DISCOVERY_TOOLS`, `SETUP_DISCOVERY_JSON`, `SETUP_NOT_CONFIGURED` | Obtain the complete bound host report for the selected client/optional external host ID/server/endpoint. Claude/Copilot reports also need the selected session's effective OAuth client ID and configuration source. A valid authenticated report without exact available `falcon_search_ngsiem` remains incomplete. Native setup requires the selected entry to remain unchanged; all reports are caller-supplied. |
| `SETUP_STATE_*`, `SETUP_PROTECTION`, `SETUP_STORAGE`, `SETUP_BACKUP`, `SETUP_DURABILITY` | Check dedicated private storage and every parent directory for another user's replacement rights, plus exact saved files and backup/native state. Preserve evidence; do not hand-edit hashes or assume a durability error means nothing changed. |

See [setup](SETUP.md) and its [synthetic requests](../examples/setup/README.md). Setup exit `1` normally means remaining work, not invalid JSON.

## Inventory and parser recovery

Inventory actions can persist partial evidence and still exit `0`. Read the action's `failure`/status and the returned checklist. A `reconcile` command does not waive unmet prerequisites.

| Symptom / code | Response |
| --- | --- |
| `full_map_complete:false` | Inspect target scope, scouting, catalog completeness/classification, source review/parser review/measurement and current reconciliation. Selected-source coverage does not produce full-map completion. |
| `covered_queries_usable:true` | Not a query-specific attestation; currently returned even on a blank canvas. Select the active pack and assess the actual query. |
| Scout target differs from the saved view | Built-in scouting is fixed to `search-all`. Relay only when explicitly authorized for that target/window; otherwise leave scouting pending and use lower-level selected-source discovery on the authorized view. Do not rewrite emitted arguments. |
| Empty or cap-sized scout imported as `done` | Scout import rejects reported errors/incomplete flags but has no empty-result or 10,000-row guard. Inspect/narrow the authorized census independently; `done` means saved capture, not completeness. |
| Partial parser fetch, `PARSER_BUDGET`, `HTTP_*`, `AUTH_*` | Successful pages/details can be retained. Review `parser_runs`, complete contiguous pages from offset zero with stable totals and exact-ID definitions, then reclassify/review. Credentials require NGSIEM Parsers: READ. |
| Parser binding remains `REVIEW` | Review every required one-based line for the exact saved parser hash. Merely storing a binding or reviewing some lines is insufficient. |
| Parser refresh reopens work | Catalog import/fetch replaces the snapshot. Changed/missing hashes invalidate bindings/exclusions; a partial replacement can reopen earlier work. Retain authorized captures and import the intended complete catalog. |
| Declared zero field has `measured:true`, `observed:false` | The zero measurement can be done while interpretation/observation remains reviewable. Do not mark the declaration observed. |
| `INVENTORY_EVIDENCE`, changed/missing active snapshot | Restore the exact retained snapshot if available or explicitly import new evidence. Do not update stored digests by hand. |
| `INVENTORY_INPUT`, wrong source ID/window/binding | Use source IDs returned by status and the complete action schema. An environment switch needs separate workflow state. |
| Historical failure still listed after recovery | Failures are retained history. Use current evidence/status, not the presence of any old failure alone, to judge remaining work. |

## MCP relay recovery

| Code / status | State/effect and response |
| --- | --- |
| `needs-probes` (exit `3`) | Execute only the currently emitted read-only probes with exact arguments, preserve each complete result, then supply the matching ID/file. |
| `MCP_UNKNOWN_PROBE`, `MCP_ALREADY_ANSWERED`, `MCP_PROBE_REQUIRED` | Inspect the currently emitted batch. Earlier answers in a multi-answer invocation may already be persisted; do not replay the whole batch blindly. |
| `MCP_SHAPE`, `MCP_TOO_LARGE`, `MCP_TOOL_ERROR`, `MCP_INCOMPLETE`, `MCP_DEADLINE`, terminal `failed` | Failed evidence is retained in state and cannot be replaced/reopened as an empty success. Start a newly authorized bounded run with new state/receipt paths; preserve the failed state for private diagnosis. |
| `MCP_STATE_RESTART` | Stored algorithm/state format is incompatible. Begin a new run; do not relabel its format/version. |
| `MCP_STATE_TAMPERED` | Hashes or request/answer relationships do not match. Preserve and investigate; checks are unkeyed integrity checks, not authenticity signatures. |
| `activation-blocked` (exit `3`) | Evidence may be complete and the pack may already have changed. Inspect `active_pack_updated` and diagnostics; resolve storage/lock issues, then resume the same state **without answers**. |
| `MCP_ACTIVATION_CONFLICT` | Pack/receipt differs from the frozen predecessor/target bytes. Investigate the replacement; do not overwrite unrelated data to force activation. |
| `complete` reopens with integrity failure | Activated state checks the exact expected pack and receipt again. Use a new run for a new refresh rather than editing the previous activated outputs. |

The deadline includes agent/tool latency from initialization through answer ingestion. Accepted answers do not extend it. Activation-only recovery can proceed after expiry if all evidence was accepted in time. Frozen recovery never remerges. MCP state can contain original response bytes and exact frozen outputs; it is sensitive. A failed evidence run generally has failed **state**, not a newly written activation receipt. See [MCP details](MCP-COMPANION.md).

## Native API and protected-file recovery

| Code or family | Response |
| --- | --- |
| `AUTH_MISSING`, `AUTH_FILE`, `AUTH_PERMISSIONS` | Use a protected regular non-symlink credential file or explicit supported environment variables. Check Unix `0600` restrictions; Windows ACLs are operator-managed. Do not paste secrets into commands/logs. |
| `AUTH_CLOUD`, `AUTH_ACCOUNT`, `AUTH_TENANT`, `ENV_TENANT`, `ENV_VIEW`, pack environment mismatch | Reconfirm exact cloud/account/view and member context. Fingerprints are account-configuration bindings, not provider-verified CIDs. Never reuse another environment's snapshot. |
| `AUTH_REJECTED`, `AUTH_RESPONSE`, `HTTP_STATUS` | Distinguish renewal/401 from permission/403; check cloud, roles/scopes, member binding and endpoint. Parser reads and API job lifecycle have distinct scope requirements. |
| `HTTP_START_AMBIGUOUS` | Start may have succeeded without a usable job ID. Do not automatically repeat the POST; investigate through the authorized client. Cleanup cannot target an unknown ID. |
| `HTTP_DEADLINE`, `HTTP_RETRY_AFTER`, `SEARCH_BUDGET`, `SEARCH_DEADLINE`, `SEARCH_CANCELLED` | Narrow the explicit scope/window or choose a new authorized finite budget. A timeout/cancellation is not zero events. Known jobs get a separate bounded cleanup attempt. |
| `HTTP_RESPONSE_LIMIT`, `HTTP_RESPONSE`, `SEARCH_RESPONSE`, `SEARCH_ERRORS`, `SEARCH_INCOMPLETE`, `SEARCH_ROW_LIMIT`, `DISCOVERY_EVIDENCE` | Incomplete/malformed/capped evidence cannot activate a replacement. Preserve report/cleanup details and start narrowed work; never reconstruct missing rows. |
| `SEARCH_TIME_PRECISION`, `SEARCH_TIME_RANGE`, `SEARCH_WINDOW` | Use ordered absolute UTC `Z` bounds with whole-millisecond precision and permitted window length. No silent rounding/sharding. |
| `LOCAL_FILE_TYPE`, `LOCAL_PATH`, `LOCAL_SIZE`, `LOCAL_DIRECTORY_PERMISSIONS`, `LOCAL_IO` | Use bounded regular non-symlink files and a dedicated private directory. Fix access/path problems without relaxing protections or deleting evidence. |
| `PACK_BUSY` | Another OS writer lock is held. Wait/inspect the writer. The stable lock file normally persists; deleting it is not lock recovery. |
| `PACK_DURABILITY` | Replacement may already be visible after rename. Inspect reported `active_pack_updated`, the active snapshot and receipt before retrying; lack of durability confirmation is not rollback. |
| `PACK_VERSION`, `PACK_PROFILE`, `PACK_DIGEST`, `PACK_INTEGRITY`, `PACK_RECORD`, `PACK_TIME`, `PACK_JSON`, `PACK_UTF8`, `PACK_SIZE` | Use a complete supported original snapshot; validate provenance/identity/references. Do not rehash a filtered inspection view or relabel history as observed. |
| Import `--out` aliases an input | Choose a separate destination from the native input, environment file and all legacy components. `--replace` can overwrite an aliased source; the adapter does not enforce input/output separation. |

API discovery holds the pack writer through execution/activation; MCP does not hold it between agent calls. API preflight can remove its empty reserved receipt before any search. Measured API failures persist reports where possible; receipt persistence/output can fail after activation. Pack and MCP receipt replacement are recoverable phases, not a single atomic transaction. Forced process termination or power loss can prevent cleanup. See [environment mechanics](ENVIRONMENT.md).

## Assessment findings

Use the [result-assessment reference](RESULT-ASSESSMENT.md) for `RESULT_*` findings, cap defaults, normalization and outcome precedence. `no-findings` can include informational diagnostics and never means verified complete/correct search. An empty result is inconclusive; a zero aggregate count is advisory. Resolve the precise evidence problem before reporting absence.
