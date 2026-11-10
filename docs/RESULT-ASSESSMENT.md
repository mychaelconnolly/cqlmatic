# Assess a saved search result

`assess` inspects a saved MCP result, the exact query supplied by the caller, and an optional environment pack. It is offline: it does not execute CQL, retrieve another page, verify the execution target/window, or attest that the supplied result came from the query. The calling analyst or agent owns that binding. Follow the [MCP workflow](MCP-COMPANION.md) to retain the exact executed bytes, confirmed target, UTC window, and entire unedited response.

## Inputs and modes

Choose exactly one mode:

```sh
cqlmatic assess --request examples/assess.request.json --json
cqlmatic assess --query-file .local/query.cql --result .local/mcp-result.json --pack .local/pack.json --json
cqlmatic assess --jsonl < .local/assess.requests.jsonl
```

The first example is runnable offline and synthetic. Operational paths in the other commands are caller-supplied protected files. `--query-file` and `--result` are required together. `--request` takes `{schema_version:1,id?,query,result,context?}`, where `result` is the **entire** saved MCP JSON value. It is not just a row array. See [contracts](CONTRACTS.md#result-assessment) and `cqlmatic --schema assess` for request/result subschemas. File arguments accept `-` for bounded stdin, but one stdin stream cannot independently supply both query and result.

Assessment automatically reuses a matching confirmed Full or Quick inventory's active pack and can fill an otherwise unspecified target. `--inventory-state FILE` selects a different protected inventory; `--pack FILE` keeps an explicit validated snapshot primary. Neither option triggers refresh. A request's optional `context.environment` must agree with the selected pack. Inventory identity is checked for each request or JSONL row before opening its active snapshot; foreign inventory is skipped with a notice. A missing default inventory is benign, while an explicitly selected missing state or broken matching active snapshot fails closed, including when an explicit pack was supplied. A malformed default can be ignored with a notice when a valid explicit pack is supplied; unsafe protected paths still fail closed.

Assessment never captures Quick leads or turns saved-query ledger counts into population rates. It does not run generation/checking's complete grounding policy: `require_observed` or `confirmed_fields` in the shared context are not an assessment acceptance gate. Run `generate`/`check` separately with the intended policy. `--fail-on-warning` and mixed-kind acceptance flags are not assess options.

The query must fit the implemented parser, but assessment is not a substitute for the full static validator. Unknown/dynamic query effects can leave output or cap analysis uncertain. Resolve known static defects before execution.

## Accepted saved-response shapes

A bare result is an object with a `result` array of objects:

```json
{"result":[{"_count":"24"}]}
```

Optional bare-result keys are `trace_id` (string), `metaData`/`metadata` objects, `error`/`errors`, and recognized completeness indicators. Unknown top-level fields are rejected. A raw row array, a `rows` wrapper, or a reconstructed summary is not the contract.

The standard MCP text envelope must contain exactly one `{type:"text",text:STRING}` content item. Optional `isError` must be boolean (omission means false); optional `structuredContent` must equal the parsed text JSON. Nested envelopes and extra envelope fields are rejected. For example:

```json
{"content":[{"type":"text","text":"{\"result\":[{\"_count\":\"24\"}]}"}],"isError":false,"structuredContent":{"result":[{"_count":"24"}]}}
```

Saved-result files may contain whitespace-separated identical JSON repeats produced by a host display. Differing copies or duplicate object keys fail closed. Assessment **requests**, including JSONL records, still require one JSON document with unique object keys, including inside `result`. If the host saved a large response to a file, pass that file directly; never retype or trim it.

Timeout/deadline text, tool errors, incomplete flags and malformed metadata/rows cannot become successful empty observations. Recognized completeness indicators are evaluated both at top level and inside metadata; flags are conservative, including floating-point values that decode to zero. Any non-null `error`/`errors` value is treated as a tool error. Normalization validates shape and flags, not the remote search's completeness.

The raw result-file limit is 8 MiB. Full request/JSONL input is limited to 1 MiB, so a larger permitted saved result should use the file-pair mode. At **10,000 or more rows**, assessment is inconclusive (`RESULT_ROW_LIMIT`); discovery rejects such a response rather than activating a pack. The common normalizer does not itself impose that row-count rule.

## Result fields and outcomes

| Field | Meaning |
| --- | --- |
| `schema_version`, `id` | Version `1` and successfully decoded optional request ID. |
| `outcome` | `no-findings`, `advisory`, `inconclusive`, or `error`. |
| `query_sha256` | SHA-256 of the supplied query's exact UTF-8 bytes; whitespace/newlines matter. |
| `rows` | Number of normalized result rows, not necessarily total matches. |
| `caps` | Known limiting stages as `{stage,cap,defaulted}`; includes nested stages and is not an execution trace. |
| `columns` | Map from inspected output-column name to `{present,null,empty}` counts. |
| `findings` | Diagnostics with severity/code/message and optional location/reference. |

`present` counts rows containing the key, including null and empty string. `null` counts explicit JSON null; `empty` counts exactly `""`. Missing is `rows - present`; these categories describe returned rows only. The inspector uses known query outputs where available, otherwise the union of keys present in the returned rows. It cannot invent columns that the parser/output analysis cannot identify.

| Outcome | Exit | Meaning |
| --- | --- | --- |
| `no-findings` | 0 | No adverse finding from these checks. Informational findings can still be present. |
| `advisory` | 1 | Inspect an observed issue, such as zero count, missing/null output, or sampled key coverage. |
| `inconclusive` | 1 | The result cannot settle a check, including empty rows, timeout, incomplete payload, or an unverifiable ceiling. |
| `error` | 2 | Bad request/query/JSON, incompatible pack, or another assessment error. |

When multiple findings apply, severity of the **outcome** is `error` > `inconclusive` > `advisory` > `no-findings`; it is not derived by counting diagnostic severities. A normal assessment includes informational `RESULT_COMPLETENESS_UNATTESTED`, even for `no-findings`. Adapter-level parse failures can return an error result without that informational finding. Global I/O/configuration errors can instead return only a diagnostic on stderr; see [stream framing](CONTRACTS.md#streams-and-errors).

`query_sha256` does not prove the caller executed that query. The saved-response normalizer hashes the bytes it receives. In the assess core, a request's JSON `result` is reserialized before normalization: that internal raw digest describes the reserialization, **not original MCP wire whitespace/order**. `AssessResult` exposes the query digest, not a wire-response digest. Preserve the original protected capture separately when exact response bytes matter.

## Checks and their limits

| Finding | Interpretation and next action |
| --- | --- |
| `RESULT_EMPTY` | Zero returned rows are inconclusive; inspect source, window, field semantics, and failures. It is not evidence of no activity. |
| `RESULT_ZERO_COUNT` | A recognized aggregate count alias contains integer zero or string `"0"`; advisory, not proof of underlying absence. Counts are tracked through known aggregate outputs. |
| `RESULT_NULL_FIELDS` | Inspected output column has missing, null, or empty-string values; review intended evidence and nullable transforms. |
| `RESULT_KEY_COVERAGE` | A relevant key has a stored coverage fraction below one in one unambiguously selected source. Suggested same-parent fields are candidate leads, not automatic substitutions. |
| `RESULT_CAP_REACHED` | A visible result or collected list reached a recognized cap; severity depends on requested size versus completeness ceiling. |
| `RESULT_UPSTREAM_CAP_UNVERIFIED` | Intermediate/nested/series/collect/join limits cannot be fully evaluated from outer rows. |
| `RESULT_ROW_LIMIT` | At least 10,000 rows; the MCP path may not contain every row. |
| `RESULT_TIMEOUT`, `RESULT_ERROR`, `RESULT_INCOMPLETE`, `RESULT_SHAPE` | Tool timeout/error/completeness/shape failed normalization; inconclusive. Narrow or repair the authorized workflow and retain the capture. |
| `RESULT_JSON`, `RESULT_QUERY_PARSE`, `REQUEST_VERSION`, pack errors | Invalid saved JSON, unsupported parser input, wrong version, or incompatible snapshot; fix the specific error. |

Coverage inspection is deliberately narrow: source filters must identify exactly one pack source without ambiguous `#repo`/`#type` selection. It uses recorded rates for relevant fields, and can suggest up to five qualified observed siblings under the same parent. No warning does not establish complete field presence, source availability, record-kind resolution, or correctness. It does not replace generation/checking's informational normal-mode kind notice or strict `CQL_RECORD_KIND_AMBIGUOUS` policy.

### Cap defaults recognized by this version

These are the assessor's bundled assumptions, not measurements of a particular server's current configuration. `defaulted` distinguishes omitted from explicit limits. A `max` sentinel can still depend on target configuration and memory.

| Stage | Default cap | Recognized `max` | Main limitation |
| --- | --- | --- | --- |
| `groupBy` | 20,000 groups | 1,000,000 | Group/memory ceiling; outer row count may hide earlier loss. |
| `top` | 10 | Not in this profile | Returned ranking/optional rest row, not all values. |
| `sort` | 200 | Not in this profile | Requested ordering/size can follow an earlier lossy stage. |
| `head`, `tail`, `table` | 200 | 200,000 | Requested output size or configured row ceiling. |
| `timeChart`, `bucket` | 10 series | `bucket`: 500; no verified `timeChart` maximum | Row count does not count series. |
| `collect` | 2,000 values per group | Not in this profile | Collected values, not outer-row count. |
| `join` | 100,000 subquery rows | No verified maximum in this cap table | Subquery rows cannot be counted from the outer result. |

The function profile and query validator still determine which argument forms can be generated/checked. The assessor cannot infer a verified numeric maximum merely because the parser accepts a sentinel. See [function contracts](QUERY-REFERENCE.md#function-contracts).

- Reaching an **explicit requested** `sort`/`head`/`tail`/`table`/`top` integer size is informational. Reaching a default size or a `groupBy`/row-ceiling `max` at the final visible stage is inconclusive.
- A numeric/default `groupBy` cap before a later stage or inside a nested query is inconclusive even if final rows are below it. Intermediate rows are unavailable. Upstream `groupBy(limit=max)` is informational, while its group/memory ceiling remains unverified.
- Series-limiting stages cannot be proved complete by counting result rows; numeric/default series caps remain inconclusive. Requested recognized ceilings still carry informational uncertainty.
- `collect` inspects visible arrays or newline-delimited strings for its values. A hit on an explicit requested size is informational; a default hit is inconclusive. Missing/unobservable collected outputs or a below-default list can also leave completeness inconclusive.
- Numeric/default join subquery limits remain inconclusive. A parsed `limit=max` yields informational uncertainty, not a verified numeric cap or complete join.

Adding a final row limit does not repair upstream loss. Conversely, an informational ceiling finding is not a new adverse outcome by itself. Keep static grouping diagnostics, assessment findings, and remote execution metadata separate.

## Offline outcome examples

These requests illustrate assessment behavior without any tenant contact:

```json
{"schema_version":1,"query":"count()","result":{"result":[{"_count":"24"}]}}
```

Expected `no-findings`, exit `0`, with informational completeness finding.

```json
{"schema_version":1,"query":"count()","result":{"result":[{"_count":"0"}]}}
```

Expected `advisory`, exit `1`, with `RESULT_ZERO_COUNT`.

```json
{"schema_version":1,"query":"count()","result":{"result":[]}}
```

Expected `inconclusive`, exit `1`, with `RESULT_EMPTY`.

```json
{"schema_version":2,"query":"count()","result":{"result":[]}}
```

Expected `error`, exit `2`, with `REQUEST_VERSION`. Runtime checks the supported version even when a shape validator accepts the integer.

Save each as a request file for `assess --request FILE --json`, or put one request per line through `--jsonl`. Assess JSONL preserves decoded IDs, continues after record errors, and returns the highest exit. For a live answer, report the intended query/target/window, actual returned evidence, findings and unresolved limits; `no-findings` never attests search completion or analytic correctness.
