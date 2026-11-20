# Capability profile and limits

Profile: `cqlmatic-ngsiem-core-v3`. Contracts: JSON schema version 1. Inspect function contracts with `catalog --functions --limit 250` and the current profile, knowledge revision and budgets with `catalog --profile`.

## Assurance boundaries

| Assessment | Establishes | Leaves open |
| --- | --- | --- |
| Reference lookup | Attributed language guidance and documented parameter facts | Executable forms, target availability and execution |
| Typed construction | Structured input lowered through the implemented query model | Whether the supplied intent, fields and values answer the question |
| Parsing | Full input fits the implemented syntax subset | Other valid platform syntax |
| Static acceptance | Implemented argument, effect, placement and selected value rules pass | Full compiler parity, dynamic types and runtime semantics |
| Grounding | Field evidence matches selected source namespaces and policy | Unobserved data, future ingest and field meaning |
| Bounded English | The documented grammar was fully consumed into a visible plan | Fidelity to unrestricted prose |
| Optional capability observation | A dated fixed case matched its expected result in the recorded target/authorization context | Other forms, contexts, resources, feature behavior and later availability |
| Saved-result assessment | Returned rows were checked against the modeled output, count, source and cap lineage | Query/result binding, remote completeness and analytic correctness |

An unknown function, regex feature or unsupported syntax returns `unsupported`: the checker cannot assess it within the implemented subset. That can mean a typo or valid platform syntax outside the profile. `invalid` means a recognized construct violates a checked rule, such as an unknown argument to a known function. A rejected profile rule is not a universal platform verdict. Knowledge lookup is broader than executable coverage.

Empty, whitespace-only, and `*` queries are valid match-all inputs to `check`. A `ready` assessment does not ensure restrictive source or event scope. `--require-observed` grounds referenced deployment fields; it does not certify source existence or availability. A source-only query can therefore be ready without matching source evidence, but `--require-observed` still requires a valid pack even for that query.

`grounding.policy` records the effective `require_observed`, `accept_mixed_record_kinds`, and `accept_partial_coverage` flags on every analysis outcome. Without strict grounding, a selected map adds warnings only for what it lacks, after the diagnostics about the query itself: `CQL_UNMAPPED_SOURCE` for a `#repo` or `#type` value that matches no mapped source, and `CQL_UNMAPPED_FIELD` for a referenced deployment field that no selected source lists as observed, documented or declared, or that the map measured and did not observe. Built-ins, `@` metadata fields and query-produced values are never reported, and a field the map lists only as documented or declared is quiet. Reading a field that an earlier step such as `drop` or `rename` removed warns `CQL_DROPPED_FIELD`, with or without a map. Without a map these warnings do not apply; `grounding.basis` holds the evidence for each field. Ordinary mixed or incompletely sampled kinds produce informational `CQL_RECORD_KIND_NOTICE`, including nested queries. In strict observed mode, unresolved kinds retain `CQL_RECORD_KIND_AMBIGUOUS` and require context unless explicitly accepted with a warning. `--accept-partial-coverage` permits positive current source/field observations with partial deployment provenance and retains `CQL_PARTIAL_COVERAGE` with the affected field, retrieval date and observation window. Stale, absent, nonobserved or ambiguous-source evidence still requires context in strict mode; partial acceptance does not resolve mixed kinds or establish complete coverage.

## Supported surface

The [official-page ledger](../data/function-scope.json), reviewed on 2026-11-14, contains 196 function names: 193 current Query candidates, deprecated Query use of `lowercase`, parser-only `copyEvent`, and unreleased `math:spherical2cartesian`. The executable catalog has bounded contracts for the 194 Query candidate names. `lowercase` is distinct from `lower`; deprecation is not removal or aliasing. Candidate membership does not confirm every documented argument form or deployment prerequisite.

Use the [query authoring reference](QUERY-REFERENCE.md) for typed variants, recipes, bounded-English clauses, raw syntax rules and the complete function-family index. [Runnable synthetic examples](../examples/query-reference/README.md) include expected diagnostics and rendered CQL.

Typed plans expose the bundled function contracts through expressions, function calls and pipeline steps. Use `catalog --functions --limit 250` to inspect their arguments. [Parameter facts](../data/function-parameters.json) retain qualified choices, bounds and defaults; the Rust contracts remain the argument-type authority. Reference lookup can describe valid CQL outside the implemented checker profile. The vendor's published grammar is a generation subset, not a complete parser specification.

| Family | Construction and static checks | Remaining limits |
| --- | --- | --- |
| Endpoint | Matching modes, missing policy, extraction/decoding, ordered classification and projection | Deployment field presence, arbitrary prose and runtime value semantics require caller review |
| Aggregation | Conditions, aggregates/selectors/top, rates, exactness and formatting | Cardinality, estimator error, ties, caps and production order depend on the target |
| Arrays | Flat/object bindings, inline effects, physical indices, split and nullable lineage | Indexed shape, gaps, output reuse and unknown member ownership remain explicit |
| Mixed | Source namespaces, joins, table declarations/exports and enrichment contracts | External resources, enrichment availability and production cardinality depend on the deployment |
| Temporal | Timestamp fields, buckets, series/session outputs, neighbor order and correlation links | Actual timestamps, sequence dimensions, gaps and target capabilities require runtime confirmation |
| Scalar and parsing | Math/text/time components, encodings, hashes, structured parsers, literal field-setting targets and formatting | Runtime domains, nullable/dynamic output and external data remain conditional |
| Preamble and resources | Query time settings, local/remote table declarations, persisted aggregations and result-phase DNS | Effective time bounds, connection language, feature flags, resource access and data freshness require review |

Catalog entries explain syntax, supported forms and caveats. Static acceptance does not establish target availability, runtime values, ordering or execution completeness.

## Important rules

- Keep literal/glob/regex matching, missing inclusion/exclusion, numeric/lexical sorting and exact/estimated accuracy explicit. Function-containing disjunctions are unsupported; typed conjunctions lower to successive filters.
- Exact distinct lowering uses grouping and can still hit group/memory ceilings. `groupBy(limit=max)` emits informational `CQL_GROUPBY_LIMIT`; numeric/omitted limits warn. A later sort or row cap does not recover lost groups. `head`, `tail`, `sort` and `table` support integer/`max` row limits; `top` and `collect` remain integer-only.
- Preserve function contexts, effects and source namespaces. Source replacement may discard earlier filters; logical array markers do not project physical indices; nullable writes, reused names and unknown exports do not prove value presence. Join/table declarations, correlation links and named-query attributes have bounded placement contracts.
- `CQL_BRANCH_FUNCTION` rejects aggregate and join calls in `case` alternatives and `match` arms, including nested conditionals and assigned calls. These branches may filter, assign and transform; aggregate or join outside the conditional. Function argument subqueries retain their own scope rules.
- `CQL_JOIN_PLACEMENT` rejects `join`, `selfJoin` and `selfJoinFilter` inside a function argument: a sub-aggregate, a filter subquery or an inline array function. They are top-level functions; the subquery of `join` is its own query, so a join at its top level is accepted.
- `math:mod` accepts only `field` unnamed and requires a named `divisor`. String-array arguments accept numeric literals alongside strings and bare words; choice restrictions still apply.
- `CQL_FREE_TEXT_AFTER_AGGREGATE` rejects bare text, standalone regex filters and fieldless `wildcard()` after the first documented aggregate, including in continuing filters and conditional branches. Quoted `"*"` is free text. Explicit field filters and the bare match-all term `*` remain supported; independent subqueries start fresh.
- `setTimeInterval` requires named `start` and is first, top-level and unique; review its override of external API/MCP time bounds. Remote CQL subquery and remote query-string forms have different requirements. Stored-event inspection precedes aggregation/source replacement, and reverse DNS follows a top-level aggregate.
- Composite aggregate pipelines must actually aggregate; later transformations determine their final outputs. `range()` is a numeric maximum-minus-minimum reducer. Sequence functions require prior aggregation and ordering review; `window` is restricted to the aggregate argument of `bucket`/`timeChart`.
- Dashboard/user parameters without defaults, saved-query helpers and parser macros have no binding/substitution interface in this profile and remain unsupported. A recognized `beta:param()` call does not implement those interfaces.
- Correlation aggregation requires a statically proven single-row bound. `collect(multival=false)` needs a later row cap such as `head(1)` or `tail(1)`. Timestamp ordering, target capability and actual returned rows require runtime review.
- `CQL_RAW_SCAN_COST` advises when regex/wildcard matching scans `@rawstring` or all original event fields without a guaranteed preceding source/tag or field filter. Narrowing must hold across every OR/case branch; a later filter does not suppress the warning, independent subqueries start afresh, and source replacement discards prior narrowing. This is a conservative cost advisory, not a runtime estimate. It does not rewrite the query or prevent `ready`; `--fail-on-warning` returns exit 1 for warnings.
- Performance notes are informational: they never change `ready`, and `--fail-on-warning` ignores them. They apply to the stages of the query and of its independent subqueries, such as a `join` or table query, before the first aggregate or source replacement. Function arguments and conditional branches carry none. `CQL_TAG_FILTER_ORDER` marks a filter on `#` fields only that follows another stage. `CQL_FILTER_AFTER_TRANSFORM` marks a filter of field comparisons that follows a transformation. Both are reported once per query, and only when the whole filter gives the same result earlier: every earlier stage is a per-event filter, assignment or transformation with listed outputs, the filter reads no field those stages wrote or were given, and it extracts no field itself. A parser, a lookup, a join, a function filter or a conditional ends them. `CQL_SORT_COST` and `CQL_SPLIT_COST` mark `sort` and `split` when no earlier stage can have removed events: only assignments, or nothing, came before. `CQL_WILDCARD_IGNORE_CASE` marks a `wildcard(ignoreCase=true)` stage. `CQL_REGEX_ALL_FIELDS` marks a regex with no field in a narrowed query whose earlier stages are plain filters that write nothing; without narrowing `CQL_RAW_SCAN_COST` warns instead. The advice follows the vendor's pages on [query performance](https://library.humio.com/crowdstrike-query-language/writing-queries-write-best-practice.html), [tag fields](https://library.humio.com/crowdstrike-query-language/syntax-operators.html), [`sort`](https://library.humio.com/crowdstrike-query-language/functions-sort.html), [`split`](https://library.humio.com/crowdstrike-query-language/functions-split.html), [`wildcard`](https://library.humio.com/crowdstrike-query-language/functions-wildcard.html) and [regex filters](https://library.humio.com/crowdstrike-query-language/syntax-filters.html). The notes are static: they do not measure cost, and a query with none is not thereby cheap.

Generated numeric relational filters can contain quoted numeric values, such as `score > "10"`. They depend on the target's numeric string conversion described in the [CQL operator documentation](https://library.humio.com/crowdstrike-query-language/syntax-operators.html); offline acceptance does not establish that behavior for every target version or field value.

`dedup` is outside the profile. For the narrower unique-key-row request, `groupBy([key], limit=max) | select([key])` discards other columns and does not select a representative event. Grouping ceilings and missing-key behavior still require review. This is not an equivalent replacement for every dedup request, and no rewrite is performed.

Discovery does not inventory event-scoped sources; very large multi-event repositories may exceed practical discovery budgets (see [ENVIRONMENT.md](ENVIRONMENT.md#mcp-discovery)).

## Regex subset

Static syntax includes literals, escaped metacharacters, character classes/ranges, alternation, capturing/noncapturing/named groups, repetition, anchors, common shorthand classes and word boundaries. Literal `g/m/i/d/F` flags (including repeats), distinct function `i/m/d/F` flags and bounded inline `i/m/s` modifiers are checked. `F` records the requested vendor engine selection while validation remains a conservative shared syntax subset. The `regex()` flags argument excludes `g`; use `regex(repeat=true)` for repeated capture. Lookarounds, backreferences, non-ASCII case folding and Unicode property/quoted escapes remain outside the profile.

The checker parses regex syntax; it does not execute Rust regex matching as a substitute for CQL behavior.

## Optional target capability checks

After activating an ordinary field pack, `env inventory` offers **capabilities (optional)**. Checks are off by default: an empty selection or **later** returns without preparing a run. Deferring this phase preserves completed field discovery. Preparation and reservation make no MCP call; the authorized host relays each reserved fixed query and supplies its full capture and exact termination settlement.

The shipped cases are `array-union`, `array-intersection`, `array-reduce-row`, `array-reduce-column` and `math-sqrt`. Evidence covers only their qualified argument shapes: the reduction cases use `sum(var)`, and additional or altered arguments do not automatically inherit the observation. It does not confirm all forms of those functions or all 194 candidate names. `beta:repeating` and historical no-op behavior are not established by these cases.

Positive observations require the same environment/CID and view, caller-attested authorization context, case revision and resource binding. They expire exactly 30 days after result retrieval. A newer unknown attempt within the same environment, authorization context, case and resource binding supersedes an older positive result. Attempts in another authorization context neither inherit nor revoke that result; timeouts, errors, incomplete/mismatched results and unresolved termination remain unknown. Generation/checking matches the current `--execution-context-id` or request context; the same CID alone is insufficient. Qualifying evidence can make a matching target requirement informational (`CQL_TARGET_OBSERVED`), while lifecycle warnings and all other checks remain applicable.

Attempts and active time use a separate finite allowance that survives pause/resume. Unresolved termination blocks later inventory dispatch. Field observations, capability observations and saved-result assessment remain separate. See [the exact CLI actions](../USAGE.md#optional-function-capability-checks) and [the query reference](QUERY-REFERENCE.md#availability-and-optional-function-observations).

## Runtime and input budgets

- 1 MiB per CLI request or JSONL line; oversized lines are drained before the next record.
- 66,000 UTF-16 code units per query, depth 64 and 16,384 AST nodes.
- English input up to 16 KiB with Boolean nesting up to 32.
- Environment snapshots up to 64 MiB and 200,000 records.
- HTTP responses up to 8 MiB; results at the 10,000-row cap cannot establish completeness.
- Search defaults: 180 seconds, 360 polls, 500 ms poll interval and a separate five-second cleanup attempt.

Diagnostic spans use zero-based UTF-8 byte offsets, end exclusive, with one-based Unicode line/column. Generation errors can supply JSON pointers. Generation withholds a query unless ready.

## Compatibility and exclusions

Pack acceptance checks the schema/profile/knowledge-revision binding advertised by the runtime. Accepted legacy core-v1/core-v2 packs and core-v3 packs from 0.1.7 retain their original pins and digests when read. Refresh creates a current snapshot while retaining the original parent bindings, evidence and untargeted records; it does not upgrade historical evidence. Inspect `catalog --profile` and the pack metadata when moving between builds, and preserve originals instead of editing a revision to bypass validation. Strict grounding still requires applicable observations; unknown or stale coverage cannot be upgraded by a version change.

No unrestricted language model, automatic deployment mapping, full vendor compiler, pack-supplied executable rules, automatic time sharding, production execution service, ingestion, alert/rule lifecycle or autonomous query repair is included.

Syntax checking accepts one empty stage at either pipeline boundary with a warning, parameter defaults, literal argument conversions and negated transforms (with a no-effect warning). It rejects interior empty stages, reserved free-text words, unsupported quotation delimiters and invalid literal regex flags. Row/group bounds and array-name suffixes are checked; `tail` warns when a known projection lacks `@timestamp`. See [query syntax](QUERY-REFERENCE.md) for the exact forms.
