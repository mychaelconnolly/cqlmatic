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

The query must fit the implemented parser, but assessment is not a substitute for generation/checking or its full grounding policy. A parsed stage with unsupported arguments, unresolved parameters, or an incomplete output model produces scoped uncertainty. Recognized dynamic writes can instead carry an informational limitation while the known checks continue. Resolve known static defects before execution.

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
| `caps` | Known numeric limits as `{stage,cap,defaulted}`; includes nested dependencies and is not an execution trace. Limits with unknown numeric values are omitted here and reported in findings. |
| `columns` | Map from inspected output-column name to `{present,null,empty}` counts; at most the first 1,000 columns by name. |
| `findings` | Diagnostics with severity/code/message and optional location/reference. |

`present` counts rows containing the key, including null and empty string. `null` counts explicit JSON null; `empty` counts exactly `""`. Missing is `rows - present`; these categories describe returned rows only. The inspector uses known query outputs where available, otherwise the union of keys present in the returned rows. It cannot invent columns that the parser/output analysis cannot identify. Every inspected column is counted and contributes to the outcome, but the result lists at most 1,000 columns in `columns` and at most 1,000 `RESULT_NULL_FIELDS` findings, first by column name. When more columns were inspected, an informational `RESULT_COLUMN_REPORT_LIMIT` gives both totals.

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
| `RESULT_CAP_EXCEEDED` | The result has more rows, or a final `groupBy` more groups, than a limit allows: the final stage's own limit, or an earlier limit followed only by stages that cannot add rows. The saved result is inconsistent with the supplied query, so it is inconclusive; confirm the exact executed query and the complete unedited response. |
| `RESULT_UPSTREAM_CAP_UNVERIFIED` | Intermediate/nested/series/collect/join limits cannot be fully evaluated from outer rows. |
| `RESULT_QUERY_COVERAGE` | A parsed stage or dependency has unresolved/unsupported arguments, parameters or stream/output effects. The assessment is inconclusive for that scope; later source replacement does not erase the finding. |
| `RESULT_OUTPUT_COVERAGE` | A recognized operation writes dynamically named columns. Informational: known columns and other checks continue, but the result does not certify a complete output schema. |
| `RESULT_ROW_LIMIT` | At least 10,000 rows; the MCP path may not contain every row. |
| `RESULT_COLUMN_REPORT_LIMIT` | More than 1,000 columns were inspected. Informational: `columns` and `RESULT_NULL_FIELDS` list only the first 1,000 by name, and the outcome still reflects every column. |
| `RESULT_TIMEOUT`, `RESULT_ERROR`, `RESULT_INCOMPLETE`, `RESULT_SHAPE` | Tool timeout/error/completeness/shape failed normalization; inconclusive. Narrow or repair the authorized workflow and retain the capture. |
| `RESULT_JSON`, `RESULT_QUERY_PARSE`, `REQUEST_VERSION`, pack errors | Invalid saved JSON, unsupported parser input, wrong version, or incompatible snapshot; fix the specific error. |

Coverage inspection is deliberately narrow: each inspected source context must identify exactly one pack source without ambiguous `#repo`/`#type` selection. Primary queries, independent subqueries and declared-table dependencies retain their own source/field lineage; an imported column is not automatically an input read from the primary source. It uses recorded rates for relevant fields, and can suggest up to five qualified observed siblings under the same parent. No warning does not establish complete field presence, source availability, record-kind resolution, or correctness. It does not replace generation/checking's informational normal-mode kind notice or strict `CQL_RECORD_KIND_AMBIGUOUS` policy.

Output and count checks follow the final visible lineage. Projection and aggregation retire fields they do not retain; renaming a known count carries its count meaning, while overwriting it with a value or a possibly colliding dynamic write can remove that meaning. Composite aggregate pipelines use their final outputs, including later renames, drops and overwrites. `range()` is a numeric max-minus-min reducer, so a zero range is not a zero event count.

Dynamic parsing and enrichment remain conditional. For example, `parseJson()` can write unknown field names; a disjoint explicit prefix narrows which existing fields it can overwrite. `kvParse` preserves or overwrites existing fields according to its parameters. `RESULT_OUTPUT_COVERAGE` records that limitation without turning every missing dynamic key into a required-column failure.

Caps belong to the stream or nested dependency that produced them. A cap in an independent query or a table subquery cannot be measured by counting unrelated outer rows. Source replacement starts a new result stream; reading a declared table brings that table's known outputs and dependencies into the new stream. A final row limit does not prove an earlier grouping, collection, series or join stage was complete.

### Cap defaults recognized by this version

These are the assessor's bundled assumptions, not measurements of a particular server's current configuration. `defaulted` distinguishes omitted from explicit limits. A `max` sentinel can still depend on target configuration and memory.

| Stage | Default cap | `limit=max` | Main limitation |
| --- | --- | --- | --- |
| `groupBy` | 20,000 groups | Target `GroupMaxLimit`; effective numeric value unknown | Group/memory ceiling; outer row count may hide earlier loss. |
| `top` | 10 | Not in this profile | Returned ranking/optional rest row, not all values. |
| `sort`, `head`, `tail`, `table` | 200 | Target `StateRowLimit`; effective numeric value unknown | Requested output size or configured row ceiling; earlier lossy stages remain unverified. |
| `timeChart`, `bucket` | 10 series | Not in this profile | Row count does not count series. |
| `collect` | 2,000 values per group | Not in this profile | Collected values, not outer-row count. |
| `join` | 100,000 subquery rows | Not in this profile | Subquery rows cannot be counted from the outer result. |

The function profile and query validator still determine which argument forms can be generated/checked. The assessor cannot infer a verified numeric maximum merely because the parser accepts a sentinel. See [function contracts](QUERY-REFERENCE.md#function-contracts).

- Reaching an **explicit requested** `sort`/`head`/`tail`/`table`/`top` integer size is informational. Reaching a default size or a numeric `groupBy` cap at the final visible stage is inconclusive.
- A final stage that returns more rows than its limit allows (plus the optional `top` rest row) contradicts the supplied query and is inconclusive with `RESULT_CAP_EXCEEDED`, including `head(0)` with returned rows. The same check applies to an earlier top-level `head`, `tail`, `sort`, `table`, `top` or one-row-per-group `groupBy` limit, written directly or assigned (such as `a := table([a], limit=1)`), when every later stage is one of: a filter without function calls (including an `OR` of such alternatives), `select`, `drop`, `rename` (also assigned, as in `c := rename(a)`), `head`, `tail` or `sort` without nested calls, or an assignment of an expression without calls. That limit keeps its intermediate finding. After any other later stage, more rows than an explicit earlier limit allows is an inconclusive `RESULT_UPSTREAM_CAP_UNVERIFIED`: the stage is not proven to keep rows within the limit, and some stages (an appended aggregate row, a split) can legitimately exceed it. Rows within the limit are unaffected, and a stage that replaces the source ends the earlier limit.
- A `groupBy` limit counts groups. When its sub-aggregation outputs at most one event per group, each row is a group. When a group can return several rows (for example `function=tail(3)` or `function={tail(3)}`), the final cap counts distinct grouping-key tuples, but only when every key is a plain field name and every sub-aggregation is on a fixed list. An array, indexed or wildcard key, or a row that lacks the key but holds another spelling of the same data (such as `a.x` or `a[0]` for key `a`), leaves the group count unverified; a key absent from a row with no such spelling counts as its own value. The list is `head`, `tail` and `sort`, which pass events through, and `top`, `collect`, `table`, `selectLast`, `selectFromMax`, `selectFromMin`, `count`, `sum`, `avg`, `min` and `max`. None of their outputs (including `as` aliases and default names) may refer to the same data as a grouping key: array elements, structured members and wildcards of a key count, so `count(as="a[0]")`, `count(as="a.x")` and `count(as="a*")` all conflict with key `a`. An inline pipeline also qualifies when every step is a literal or wildcard comparison filter or such a call. No argument anywhere may contain a nested call or pipeline. Any other sub-aggregation, including nested pipelines, regular-expression or parameter filters, assignments, branches, source replacement and unlisted functions, leaves rows unable to identify groups, and the final cap is an inconclusive `RESULT_UPSTREAM_CAP_UNVERIFIED`.
- `sort`, `head`, `tail` and `table` with `limit=max` use the target's `StateRowLimit`. The [sort](https://library.humio.com/crowdstrike-query-language/functions-sort.html), [head](https://library.humio.com/crowdstrike-query-language/functions-head.html), [tail](https://library.humio.com/crowdstrike-query-language/functions-tail.html) and [table](https://library.humio.com/crowdstrike-query-language/functions-table.html) documentation gives a default of 200,000 for that setting; assessment does not know the target's effective value. It reports an inconclusive `RESULT_UPSTREAM_CAP_UNVERIFIED` for final, intermediate and nested uses and omits the unknown value from the numeric `caps` list. It does not treat `max` as unlimited or assume 200,000.
- `groupBy(limit=max)` uses the target's `GroupMaxLimit`. The [groupBy](https://library.humio.com/crowdstrike-query-language/functions-groupby.html) documentation gives a default of 1,000,000 for that setting, and memory limits can truncate earlier. Assessment treats it like `StateRowLimit`: inconclusive in final, intermediate and nested positions, with no number in `caps`.
- A `limit=max` on a stage without a `max` sentinel in this profile, or a limit that is not a literal non-negative integer (for example negative or parameterized), is an inconclusive `RESULT_UPSTREAM_CAP_UNVERIFIED` and is omitted from `caps`.
- Aggregates outside this table whose bundled parameter data documents a default row or group limit, such as `selfJoin`, `fieldstats` and `transpose`, are inconclusive with `RESULT_UPSTREAM_CAP_UNVERIFIED` whether that limit is written or omitted.
- A numeric/default `groupBy` cap before a later stage or inside a nested query is inconclusive even if final rows are below it. Intermediate rows are unavailable.
- Series-limiting stages cannot be proved complete by counting result rows; numeric/default series caps remain inconclusive.
- `collect` inspects visible arrays or newline-delimited strings for its values. A hit on an explicit requested size is informational; a default hit is inconclusive. Missing/unobservable collected outputs or a below-default list can also leave completeness inconclusive. Values are unobservable with `multival=false` or a non-newline separator, and after an assignment to the collected name. A later aggregate keeps them countable only through an output that copies the value: a key of `groupBy`, `bucket` or `top`, a `timeChart` series, a field of `table` or `selectLast`, or an `include` field of `selectFromMax`/`selectFromMin`. Under `stats`, `groupBy`, `bucket` or `timeChart`, a sub-aggregate qualifies only if it is one of those selectors naming the field, alone or in an inline pipeline of comparison filters, `head`, `tail` and `sort`. Every other aggregate output counts as a new value even when its name equals the collected field, including names an aggregate can compute (`_count`, `_bucket`, `_sum`, `_max`, `percent`), `as` aliases, reducers such as `count(_count)`, another `collect`, and nested `stats`. Names are compared by overlap, not spelling. An output, alias, assignment or rename that can refer to the same data as the collected field rewrites it unless it is that exact field copied unchanged. Overlapping names include array forms (`b[]`, `b[0]`, `b[1]`), structured members (`b` and `b.x`) and wildcards. For example, `array:union(..., as=b)` producing `b[]` rewrites a collected `b[0]`. These stages mark the list uncountable for good when they remove or rename away a collected field, so a name recreated later is not counted: a projection or `include` that does not keep the field exactly, `drop`, `array:drop`, `array:rename` to a new name, `lowercase` field renaming, a rename other than an exact rename of the field itself, and an unassigned aggregate that does not output it. An aggregate assigned to a new name (such as `c := table([b])`) is not covered: the collected name is not marked as gone there, so a later stage that recreates it is not detected. After `case`, `match` or `OR` branches, a list stays countable only if every surviving branch still holds it; a branch that replaced its source does not.
- Numeric/default join subquery limits remain inconclusive.

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
