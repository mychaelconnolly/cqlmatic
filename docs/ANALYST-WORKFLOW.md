# Analyst workflow

CQLmatic is an offline construction, reference and review tool. The calling analyst or agent interprets the question and uses an existing authorized search client to execute it. CQLmatic makes no model calls. No model, installed skill or particular agent host is required. See the [documentation index](README.md) for setup, [query reference](QUERY-REFERENCE.md) for construction, and [contracts](CONTRACTS.md) for JSON interfaces. `ready` means the supported static rules and chosen grounding policy passed; it does not mean the query ran or answered the question.

## Establish the question and context

Record the intended result before constructing CQL: events or aggregates, keys, evidence columns, thresholds, exclusions, matching mode, missing/null policy, exactness, ordering and ties. Obtain the searchable target, source selectors, field meanings, array shapes and an explicit time window/timezone. A searchable view and a `#repo` tag are different namespaces. Preserve the confirmed `environment.view` as the execution target (MCP `repository`); keep source selectors in CQL and never silently broaden the target to `search-all`. Use absolute UTC bounds for the API/MCP handoff after resolving the requested timezone. Check every independent source separately.

Use measured environment evidence when available:

```sh
cqlmatic env inspect .local/pack.json
cqlmatic env inspect .local/pack.json --field user --source example_repo
```

A documented CPS/ECS field name, parser declaration or hand-confirmed mapping is not an observed tenant field. Fieldset listing also does not establish populated-field coverage: inspect `coverage.observed`, the rate when measured, namespace and provenance window. Use source-bound [population batches](ENVIRONMENT.md#explicit-population-batches-and-targeted-refresh) to measure specific fields, and `kind_fields`/`value_fields` to sample missing discriminator/value evidence. A pack observation also does not guarantee presence on every event. Ask a precise question when the required source, meaning, matching behavior or environment evidence is missing. Do not repair uncertainty by inventing names or dropping constraints.

## Consult the reference and construct the query

```sh
cqlmatic catalog cql:function:join --full --limit 1
cqlmatic catalog --functions array:filter
cqlmatic --schema generate
```

Catalog entries explain syntax and caveats. Function lookup exposes executable argument contracts; a reference example alone does not establish local checker coverage or runtime correctness.

Use a recipe when its semantics match, otherwise compose a typed plan with function calls, expressions, queries, branches and links. Inspect the returned interpretation. The English adapter supports its documented small grammar; unrestricted analyst prose is interpreted by the caller, not silently delegated to that adapter.

```sh
cqlmatic generate --request .local/request.json --json
cqlmatic check --file .local/hunt.cql --json
cqlmatic generate --request .local/request.json --pack .local/pack.json --require-observed --json
```

`needs-context` identifies missing/conflicting/stale evidence. `unsupported` marks a coverage limit, and `invalid` identifies a checked rule violation. Generation withholds the query unless ready. Never hand-edit emitted CQL to bypass a diagnostic; revise the request or typed plan and regenerate. Preserve the assessment, interpretation, diagnostics and original input rather than reporting only the outcome word.

When typed construction or local checking lacks coverage, reference-reviewed raw CQL may proceed through the agent's existing authorized execution boundary. Disclose that it was not locally validated and retain the coverage diagnostic. Resolve demonstrated semantic defects before execution; a coverage fallback does not excuse an incorrect interpretation or missing required context.

## Preserve the workflow's semantics

| Work | Decisions that need to remain explicit |
| --- | --- |
| Endpoint hunting | Source/event scope, literal/glob/regex modes, exclusions and missing policy; capture guarantees, decoder charset and ordered classification; evidence projection. |
| Aggregation | Key scope, exact versus estimated cardinality, aggregate outputs, thresholds/rates, sort/ties and group/memory ceilings. `limit=max` is still bounded. |
| Arrays | Flat versus structured element bindings, physical indices, gaps, missing members and output reuse. Fresh temporary namespaces can avoid stale indexed values; cleanup must be deliberate. |
| Mixed sources | Independent source/target contexts, corresponding multikeys, unmatched/duplicate policy, lookup includes and row caps. External files and IOC/geo/ASN availability need evidence. |
| Temporal work | Actual timestamp meaning, window/timezone, sequence and equality links, anchor offsets, per-key stream scope and order. Gap and correlation behavior depend on the target data and query constraints. |

Do not turn OR into successive filters, change literal matching to a glob, replace an exact calculation with an estimate, add a catch-all branch, or discard source constraints to obtain a ready result. An aggregate or projection can remove fields needed later. Nullable outputs and unknown all-field exports remain uncertain.

## Execute and inspect the exact query

Use the existing authorized API, MCP or console client for the selected target and window. Pass the exact emitted/reviewed query bytes and record the target, window and query hash beside the execution receipt. Generation/checking does not execute arbitrary queries; do not infer an execution attestation from their result.

Inspect start/compilation errors, completion and cancellation status, warnings, truncated/skipped/partial results, caps, returned columns, missing/null values, duplicate multiplicity and required order. An accepted start or `done` flag is insufficient. Compare returned data with the analytic intent; an empty result may reflect filtering, missing fields, limits or the absence of matching activity. Retain relevant diagnostics and cleanup status.

When executing through the read-only `falcon_search_ngsiem` MCP tool, save its **entire response exactly as received** to an access-restricted file before assessing the exact query bytes; do not edit the saved result:

```sh
cqlmatic assess --query-file .local/query.cql --result .local/mcp-result.json --pack .local/pack.json
```

Review `no-findings`, `advisory`, `inconclusive` or `error` and any findings, including empty results, caps, incomplete results, null fields, missing key coverage and unattested completeness. Assessment never certifies query correctness. A timeout or `inconclusive` result is not “no results”; narrow, re-run or clarify before reporting. See [result assessment](RESULT-ASSESSMENT.md) for cap and completeness findings, the [MCP companion workflow](MCP-COMPANION.md) for the probe loop and exact-result handoff, and [troubleshooting](TROUBLESHOOTING.md) for recovery.

If execution exposes an error or an incorrect interpretation, revise the plan/query against the actual evidence, rerun the static checks, and execute the new exact bytes. Keep prior attempts and explain the change. Ask for missing context instead of guessing.

## Report the answer and its limits

Return the query, interpretation, source/window, relevant rows or aggregate answer, execution completeness and unresolved assumptions. Distinguish static acceptance, field grounding, native acceptance and observed row correctness. State when execution was not run or remained inconclusive. `ready`, successful API execution and a correct analyst answer are separate claims.

Use the capability profile in `docs/COVERAGE.md` for supported syntax and limits. Keep credentials, tenant snapshots and execution receipts in protected local storage.
