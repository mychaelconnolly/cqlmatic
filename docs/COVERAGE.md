# Capability profile and limits

Profile: `cqlmatic-ngsiem-core-v2`. Contracts: JSON schema version 1. Inspect function contracts with `catalog --functions --limit 100` and budgets with `catalog --profile`.

## Assurance boundaries

| Assessment | Establishes | Leaves open |
| --- | --- | --- |
| Parsing | Full input fits the implemented syntax subset | Other valid platform syntax |
| Static acceptance | Implemented argument, effect, placement and selected value rules pass | Full compiler parity, dynamic types and runtime semantics |
| Grounding | Field evidence matches selected source namespaces and policy | Unobserved data, future ingest and field meaning |
| Bounded English | The documented grammar was fully consumed into a visible plan | Fidelity to unrestricted prose |

An unknown function, regex feature or unsupported syntax returns `unsupported`: the checker cannot assess it within the implemented subset. That can mean a typo or valid platform syntax outside the profile. `invalid` means a recognized construct violates a checked rule, such as an unknown argument to a known function. A rejected profile rule is not a universal platform verdict. Knowledge lookup is broader than executable coverage.

Empty, whitespace-only, and `*` queries are valid match-all inputs to `check`. A `ready` assessment does not ensure restrictive source or event scope. `--require-observed` grounds referenced deployment fields; it does not certify source existence or availability. A source-only query can therefore be ready without matching source evidence, but `--require-observed` still requires a valid pack even for that query.

`grounding.policy` records the effective `require_observed`, `accept_mixed_record_kinds`, and `accept_partial_coverage` flags on every analysis outcome. Lenient ready assessments warn with `CQL_GROUNDING_LENIENT` when a referenced deployment field relies on weak evidence; built-ins, query-produced values and qualifying observations do not trigger that warning. `--accept-partial-coverage` permits positive current source/field observations with partial deployment provenance and retains `CQL_PARTIAL_COVERAGE` with the affected field, retrieval date and observation window. Stale, absent, nonobserved, ambiguous-source and unresolved mixed-kind evidence still require context. Partial acceptance does not establish complete coverage.

## Supported surface

Use the [query authoring reference](QUERY-REFERENCE.md) for every typed variant, recipe, bounded-English clause, raw syntax rule and all 67 executable function contracts. [Runnable synthetic examples](../examples/query-reference/README.md) include expected diagnostics and rendered CQL.

Typed plans expose the bundled function contracts through expressions, function calls and pipeline steps. Use `catalog --functions --limit 100` to inspect their arguments. Reference lookup can describe valid CQL outside the implemented checker profile.

| Family | Construction and static checks | Remaining limits |
| --- | --- | --- |
| Endpoint | Matching modes, missing policy, extraction/decoding, ordered classification and projection | Deployment field presence, arbitrary prose and runtime value semantics require caller review |
| Aggregation | Conditions, aggregates/selectors/top, rates, exactness and formatting | Cardinality, estimator error, ties, caps and production order depend on the target |
| Arrays | Flat/object bindings, inline effects, physical indices, split and nullable lineage | Indexed shape, gaps, output reuse and unknown member ownership remain explicit |
| Mixed | Source namespaces, joins, table declarations/exports and enrichment contracts | External resources, enrichment availability and production cardinality depend on the deployment |
| Temporal | Timestamp fields, buckets, series/session outputs, neighbor order and correlation links | Actual timestamps, sequence dimensions, gaps and target capabilities require runtime confirmation |

Catalog entries explain syntax, supported forms and caveats. Static acceptance does not establish target availability, runtime values, ordering or execution completeness.

## Important rules

- Keep literal/glob/regex matching, missing inclusion/exclusion, numeric/lexical sorting and exact/estimated accuracy explicit. Function-containing disjunctions are unsupported; typed conjunctions lower to successive filters.
- Exact distinct lowering uses grouping and can still hit group/memory ceilings. `groupBy(limit=max)` emits informational `CQL_GROUPBY_LIMIT`; numeric/omitted limits warn. A later sort or row cap does not recover lost groups. `head`, `tail` and `table` support integer/`max` row limits; `sort`, `top` and `collect` remain integer-only.
- Preserve function contexts, effects and source namespaces. Source replacement may discard earlier filters; logical array markers do not project physical indices; nullable writes, reused names and unknown exports do not prove value presence. Join/table declarations, correlation links and named-query attributes have bounded placement contracts.
- Correlation aggregation requires a statically proven single-row bound. `collect(multival=false)` needs a later row cap such as `head(1)` or `tail(1)`. Timestamp ordering, target capability and actual returned rows require runtime review.
- `CQL_RAW_SCAN_COST` advises when regex/wildcard matching scans `@rawstring` or all original event fields without a guaranteed preceding source/tag or field filter. Narrowing must hold across every OR/case branch; a later filter does not suppress the warning, independent subqueries start afresh, and source replacement discards prior narrowing. This is a conservative cost advisory, not a runtime estimate. It does not rewrite the query or prevent `ready`; `--fail-on-warning` returns exit 1 for warnings.

Generated numeric relational filters can contain quoted numeric values, such as `score > "10"`. They depend on the target's numeric string conversion described in the [CQL operator documentation](https://library.humio.com/crowdstrike-query-language/syntax-operators.html); offline acceptance does not establish that behavior for every target version or field value.

`dedup` is outside the profile. For the narrower unique-key-row request, `groupBy([key], limit=max) | select([key])` discards other columns and does not select a representative event. Grouping ceilings and missing-key behavior still require review. This is not an equivalent replacement for every dedup request, and no rewrite is performed.

Discovery does not inventory event-scoped sources; very large multi-event repositories may exceed practical discovery budgets (see [ENVIRONMENT.md](ENVIRONMENT.md#mcp-discovery)).

## Regex subset

Static syntax includes literals, escaped metacharacters, character classes/ranges, alternation, capturing/noncapturing/named groups, repetition, anchors, common shorthand classes and word boundaries. Distinct i/m/d suffix flags and bounded inline i/m/s modifiers are checked. Lookarounds, backreferences, non-ASCII case folding, Unicode property/quoted escapes and engine-switch flags are outside the profile.

The checker parses regex syntax; it does not execute Rust regex matching as a substitute for CQL behavior.

## Runtime and input budgets

- 1 MiB per CLI request or JSONL line; oversized lines are drained before the next record.
- 66,000 UTF-16 code units per query, depth 64 and 16,384 AST nodes.
- English input up to 16 KiB with Boolean nesting up to 32.
- Environment snapshots up to 64 MiB and 200,000 records.
- HTTP responses up to 8 MiB; results at the 10,000-row cap cannot establish completeness.
- Search defaults: 180 seconds, 360 polls, 500 ms poll interval and a separate five-second cleanup attempt.

Diagnostic spans use zero-based UTF-8 byte offsets, end exclusive, with one-based Unicode line/column. Generation errors can supply JSON pointers. Generation withholds a query unless ready.

## Compatibility and exclusions

Legacy core-v1 packs at the original knowledge pin remain readable without changing their digest or relabelling their data. Refresh retains parent metadata and untargeted records. Strict grounding requires applicable observations; unknown or stale coverage cannot be upgraded by a version change.

No unrestricted language model, automatic deployment mapping, full vendor compiler, pack-supplied executable rules, automatic time sharding, production execution service, ingestion, alert/rule lifecycle or autonomous query repair is included.
