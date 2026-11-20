# Query authoring reference

This is the executable authoring surface of profile `cqlmatic-ngsiem-core-v3`, schema version 1. It describes construction and offline checks, not the entire CrowdStrike Query Language (CQL). Start with [the workflow](FLOW.md) and [assurance boundaries](COVERAGE.md). Use [contracts](CONTRACTS.md) for command framing and [result assessment](RESULT-ASSESSMENT.md) after execution. A `ready` result means the implemented checks passed; execution is still `not-run`. It does not establish field meaning, vendor compiler parity, target capability, execution completeness, or the accuracy of unrestricted English interpretation.

The owning definitions are [contracts.rs](../src/contracts.rs), [plan lowering](../src/language/plan.rs), [bounded English](../src/language/english.rs), [raw parser](../src/language/parser.rs), [static checks](../src/language/validate.rs), and [function resolution](../src/catalog.rs). [GenerateRequest JSON schema](../schemas/generate.json) is the machine-readable contract. Unknown fields are rejected in request/plan objects; JSON `null` is not a scalar literal.

## Choose an input

```sh
cqlmatic generate --request examples/generate.request.json --json
cqlmatic generate --plan examples/process-count.plan.json --json
cqlmatic generate --recipe threshold --params examples/threshold.params.json --json
cqlmatic generate --intent 'count events by ComputerName from "example_endpoint" event ProcessRollup2' --json
cqlmatic check '#repo=example_endpoint | count()' --json
```

A full request has `schema_version: 1`, `input`, optional `id` (default absent), and optional `context` (default `{}`). Input is exactly one of:

- `{"mode":"plan","plan": PLAN}`.
- `{"mode":"recipe","request":{"recipe": ID,"params": PARAMS}}`.
- `{"mode":"english","text": TEXT,"source": SCOPE}`.

Context supports optional `environment`, `execution_context_id`, `confirmed_fields: []`, `require_observed: false`, `accept_mixed_record_kinds: false`, and `accept_partial_coverage: false`. The CLI merges `--require-observed`, `--accept-mixed-record-kinds`, and `--accept-partial-coverage` with request context; `grounding.policy` reports the effective values on every analysis outcome. Confirmed fields record caller-supplied context, not observations. Use the environment/grounding workflow for strict evidence; do not add confirmations merely to clear a diagnostic. CLI target selection for API or MCP execution is separate from query source scope: `#repo` filters rows inside the selected API/MCP target; it does not choose that target, switch tenants, authorize access, or establish repository availability.

Strict grounding normally requires qualifying source and field provenance with `complete`, `observed-only`, or `reconciled` coverage. `--accept-partial-coverage` also accepts positive, current source/field observations whose deployment provenance is marked `partial`; `CQL_PARTIAL_COVERAGE` retains the affected field, retrieval date and observation window (or identifies an unrecorded window). It does not accept stale, absent, nonobserved, ambiguous-source or mixed-record-kind evidence. With strict grounding disabled, a selected map warns only about what it lacks, after the query's own diagnostics. `CQL_UNMAPPED_SOURCE` names a `#repo` or `#type` selection that matches no mapped source, names mapped selectors, and says when an unmatched value is the target view's name. `CQL_UNMAPPED_FIELD` names each referenced deployment field that no selected source lists as observed, documented or declared, with those sources, and gives the measurement window for a field the map measured and did not observe; a query without a source selector is compared with every mapped source, and `in()` on `#repo` or `#type` is not compared. Without a map these warnings do not apply, and `grounding.basis` records the evidence for each field. These advisories retain `ready`; `--fail-on-warning` returns exit 1.

Generation and checking reuse matching active inventory in Full or Quick mode; `--inventory-state FILE` selects another protected state, and an explicit `--pack FILE` stays primary with matching active/ledger safeguards. An inventory can fill otherwise unspecified target context but cannot replace explicit identities. Ordinary mixed or incompletely sampled kinds produce informational `CQL_RECORD_KIND_NOTICE`, including nested queries. Strict observed mode instead requires kind context or explicit mixed-kind acceptance, retaining `CQL_RECORD_KIND_AMBIGUOUS` as an error or accepted warning. An informational notice alone does not trigger `--fail-on-warning`.

## Typed plans

A plan requires `source: SCOPE`; `filters` and `steps` default to empty arrays. Scope has optional `repo`, `type`, and `event` strings. They lower, in that order, to literal equality filters on `#repo`, `#type`, and `#event_simpleName`. No scope value is interpolated as CQL. An empty plan lowers to `true`; it is match-all, not restrictive scope. A nested plan uses the same shape; its context depends on the receiving function argument.

The renderer preserves ordered steps. Filters are applied after scope and before steps. Names must be nonempty and text must pass control-character/budget checks; expression fields have the narrower bare-identifier requirement. Predicate/plan nesting is bounded (see [budgets](COVERAGE.md#runtime-and-input-budgets)).

### Predicates

Every predicate uses `op`. The table lists all other required fields and defaults. `SCALAR` means string, JSON number, or boolean; `CMP` is `eq`, `ne`, `lt`, `le`, `gt`, or `ge`.

| `op` | Required fields | Optional fields / behavior |
| --- | --- | --- |
| `eq` | `field`, `value: SCALAR` | Literal equality; stars are preserved literally |
| `not-eq` | `field`, `value: SCALAR`, `missing: include\|exclude` | Explicit absent-field branch or existence guard plus negation |
| `glob` | `field`, `pattern` | `negate: false`, `missing: null`; negation requires a missing policy, positive matching forbids it |
| `regex` | `field`, `pattern` | `flags: ""`, `negate: false`, `missing: null`; same missing-policy rule; lowers to `regex` function |
| `like` | `field`, `pattern` | CQL `like` matching; distinct from literal equality, glob, and regex |
| `exists` | `field` | `present: true`; lowers to `field="*"` or `field!="*"` |
| `compare` | `field`, `operator: CMP`, `value: NUMBER` | Field/value comparison, including `eq`/`ne`; no implicit missing policy |
| `fields` | `left`, `operator: CMP`, `right` | Bare expression-compatible field names; lowers to `test(left OP right)` |
| `call` | `function` | `args: []`, `negate: false`; resolved argument form must have a filtering effect |
| `all` | nonempty `items: PREDICATE[]` | Conjunction becomes successive filter stages |
| `any` | nonempty `items: PREDICATE[]` | Disjunction with any function predicate is unsupported |

Typed conjunction decomposition allows standalone function filters. Function-containing OR is rejected, including OR introduced by a negated regex with `missing: include`; generation emits no partial query. Raw functions nested under Boolean AND/OR or parenthesized as Boolean filters are rejected. Use separate pipeline stages for conjunction, and review whether a structured alternative really preserves the intended OR semantics.

Literal text equality containing `*` lowers to an absolute anchored, capture-free regex (for example `A*B.exe` becomes `/\AA\*B\.exe\z/`). Merely quoting raw CQL `field="A*B.exe"` does not make its star literal. `glob` deliberately retains wildcard meaning. `contains` in bounded English escapes regex metacharacters for a substring search. Numeric/boolean predicate literals render as quoted filter values; generated `score > "10"` depends on the target's numeric string conversion. Compare this with a numeric expression inside `test` or an assignment. Offline acceptance does not prove conversions for every field/value.

Positive regex extraction can produce named capture fields; optional captures or captures not present on every successful alternative remain conditional. Negation does not establish captured outputs. `strict=false`, repeated extraction, reused names and nullable outputs need separate effect/lineage review.

### Expressions and arguments

An argument is `{"value": EXPRESSION}` for a positional argument or `{"name":"ARG","value":EXPRESSION}` for a named argument. `name` defaults to absent. Argument order is retained; duplicate resolved names, excess positional arguments, unknown arguments, and missing required arguments are diagnosed. Prefer named arguments when multiple roles could be confused. `readFile` combines multiple unnamed resources in order. `math:mod` accepts only `field` unnamed; give `divisor` by name, as in `math:mod(y, divisor=3)`. An array-valued parameter does not imply arbitrary variadic support for other functions.

Every expression uses `kind`:

| `kind` | Required fields | Optional fields / behavior |
| --- | --- | --- |
| `field` | `name` | Unquoted expression-compatible identifier; reads a field |
| `literal` | `value: SCALAR` | String remains a value, not an expression field |
| `compare` | `left`, `operator: CMP`, `right` | Expression equality renders `==` |
| `unary` | `operator`, `value` | Operators `negative`, `positive`, `not` render `-`, `+`, `!` |
| `binary` | `left`, `operator`, `right` | `add`, `subtract`, `multiply`, `divide`, `remainder` |
| `call` | `function` | `args: []`; scalar use still requires a qualified scalar function and argument form |
| `array` | `items: EXPRESSION[]` | For typed arguments; not a general scalar value |
| `query` | `plan` | Unlabeled subquery, no named-query attributes |
| `named-query` | `name`, `plan` | `include`, `after`, `before` default absent; meaningful in qualified correlation contexts |

A named query label starts with an ASCII letter and contains only ASCII letters/digits/underscores. `include` is `{"mode":"all"}` or `{"mode":"fields","fields":[...]}`. A literal `"*"` entry is not the all-fields mode. An anchor is `{"query":"LABEL","offset":"+5s"}`; `offset` is optional and, when present, must be an explicitly signed fixed duration. See correlation below for allowed contexts.

### Stages

Every stage uses `op`. Defaults below apply only where fields are omitted, not as permission to omit a required field.

| `op` | Required fields | Defaults and lowering |
| --- | --- | --- |
| `filter` | `predicate` | Same predicate lowering as top-level filters |
| `assign` | `field`, `expression` | `field := expression`; cannot create/overwrite `#` tags |
| `call` | `function` | `args: []`; generic typed call, still subject to context/effect checks |
| `count` | — | `field: null`, `distinct: false`, `accuracy: exact`, `as: _count` |
| `group` | nonempty `keys`, nonempty `aggregates` | `groupBy(keys,function=[...],limit=max)`; keys and aggregate aliases must be distinct |
| `select` | nonempty `fields` | `select(fields)`; no implicit retained columns |
| `sort` | nonempty `fields` | `order: desc`, `type: number`, `limit: 200`; positive integer limit |
| `head`, `tail` | positive integer `limit` | Preserve first/last bounded rows; no sorting added |
| `case` | nonempty `branches: PLAN[]` | Ordered branches; no catch-all inserted |
| `match` | `field`, nonempty `arms` | Each arm has `pattern` and `plan`; ordered field-match branches |
| `join` | `query: PLAN`, `field`, `key`, `include`, `max_matches` | `left: false`, positive `limit: 200`; lowers to inner/left join with explicit `max` |
| `link` | `field`, `to`, `key` | `field <=> to.key`; correlation named-query placement only |
| `correlate` | nonempty `queries`, `links`, `within` | `sequence: false`; must be sole top-level semantic stage, with empty outer scope/filters |

For `case`, use an explicit unfiltered empty-source branch when a catch-all is intended. Match patterns use `kind`: `literal` requires `value: SCALAR`; `glob` requires `pattern`; `regex` requires `pattern` and defaults `flags: ""`; `any` has no other fields. Regex flags in field-match patterns are distinct `i`, `m`, `d`. Literal-star arms get the same anchored escaping as literal equality.

A `group` aggregate uses `function`: `count`, `sum`, `avg`, `min`, or `max`. Count defaults to `field: null`, `distinct: false`, `accuracy: exact`, `as: _count`. Other aggregates require `field` and `as`. `accuracy: estimate` is valid only for distinct count; distinct count requires a field. Exact distinct **inside a typed group aggregate** is unsupported; it is never silently estimated. Use an explicit reviewed two-stage grouping for an exact per-key distinct calculation, accounting for both grouping ceilings and missing values.

A standalone exact distinct count first excludes missing values, then groups the field with `limit=max`, then counts groups. This avoids an estimator but is exact only if execution preserves all relevant groups; memory/group ceilings still apply. Estimated mode emits `count(field=...,distinct=true)` and `CQL_APPROXIMATE_DISTINCT`. `groupBy(limit=max)` emits informational `CQL_GROUPBY_LIMIT`; omitted/numeric limits warn because groups may be discarded before sorting. These are separate from actual execution cap evidence.

Dedicated typed `sort`, `head` and `tail` stages require a JSON integer limit. Generic calls and raw CQL additionally accept the literal `max` for `sort`, `head`, `tail` and `table`; it uses the target's configurable `StateRowLimit`, whose documented default is 200,000. `top` and `collect` remain integer-only. A row cap does not prove complete input processing.

When typed `head` stages immediately follow a typed `sort` stage, `generate` emits one `sort` call with the smallest of their limits, counting the sort's default of 200. For handwritten top-N CQL, use `sort(score, order=desc, limit=3)`. `check` reports `CQL_SORT_HEAD` when `head` immediately follows `sort`, unless the sort sets a `limit` no larger than the `head` limit.

`join` stage exposes one field/key pair; generic `call` supports equal-length nonempty field/key arrays for multi-key joins. The generic form also exposes target/window arguments. A typed correlation query is `{"name":"LABEL","plan":PLAN}`; each link is `{"from":"LABEL","field":"FIELD","to":"OTHER","key":"FIELD"}`. Unknown/self links are rejected. For named-query include/anchors and correlation aggregation, use the generic typed `correlate` call.

## All nine recipes

Recipe parameters are strict objects. In the first eight recipes, `source` is required and `filters` defaults to `[]`. Source/filters lower before the steps described here; no query fragments are accepted.

| Recipe | Other parameters | Exact lowering |
| --- | --- | --- |
| `events` | `fields: []`, positive `limit: 200` | Optional `select(fields)`, then `head(limit)`; no timestamp sort |
| `count-events` | none | `count(as="_count")` |
| `count-by` | required nonempty `keys` | `groupBy(keys,function=[count(as="_count")],limit=max)`; no sort |
| `distinct` | required `field`, `accuracy: exact\|estimate` | Standalone distinct count lowering described above; accuracy has no recipe default |
| `top-values` | required `field`, positive integer `limit` | Group field with count and `limit=max`, then numeric descending sort on `_count` with requested limit |
| `threshold` | required nonempty `keys`, `minimum: u64` | Group count with `limit=max`, then `_count >= "minimum"`; zero minimum is allowed; no sort |
| `pivot` | required `field`, `value: SCALAR`; `fields: []`, positive `limit: 200` | Append literal equality to filters, optional select, then head |
| `join-pivot` | required `query: PLAN`, `field`, `key`, `include`, positive `max_matches`; `left: false`, positive `limit: 200` | One typed join stage with explicit inner/left, cap and match maximum |
| `sequence` | required nonempty `queries`, `links`, positive duration `within` | Empty outer scope/filters; one typed correlate stage with `sequence: true` |

`top-values` is groupBy plus sort, not native `top()`. Native `top` uses an approximate sketch and warns. Grouping plus sort has grouping/memory ceilings of its own; do not promote it to an unconditional complete ranking guarantee. Ties and missing-key behavior require review on the execution target. `events` and `pivot` never imply “latest”; use an explicit timestamp sort when that is the requirement.

## Bounded English

Keywords are ASCII case-insensitive. Field/source/event names can be unquoted words or double-quoted JSON strings; text values must be double-quoted JSON strings. Numeric values are JSON numbers; boolean literals are lowercase `true`/`false`. Whitespace separates words; commas separate name lists; parentheses group conditions. Operators must be separate tokens (`score >= 10`, not `score>=10`). English is limited to 16 KiB and 32 Boolean nesting levels; the full input must be consumed.

Accepted starts, with aliases and lowering:

| Form | Behavior |
| --- | --- |
| `show [latest\|earliest] events` | Optional projection then head(200); `latest` uses tail(200) instead |
| `find [latest\|earliest] events` | Alias of show |
| `list [latest\|earliest] events` | Alias of show |
| `count events [by FIELD[, FIELD...]]` | Global count or grouped count |
| `count distinct FIELD exactly` | Explicit exact standalone distinct count |
| `count distinct FIELD approximately` | Estimated count and warning |
| `top N FIELD[, FIELD...]` | Grouped count followed by descending numeric sort with limit N |

`latest`/`earliest` choose tail/head; they do not add sorting or prove timestamp ordering. `N` and `limit` must parse as positive `u32`. After the start, clauses are read in their written order, with the constraints below; they are not a natural-language reorderer:

- `from REPO`: at most once; must equal a supplied source-context repo if one exists.
- `event EVENT`: at most once; must equal a supplied source-context event if one exists. There is no English `type` clause; supply `source.type` or CLI `--type`.
- `where CONDITION`: at most once; explicit Boolean grouping is required when mixing AND and OR at the same nesting level.
- `by FIELD[, FIELD...]`: once for event counts whose keys have not already been set; unavailable for distinct and event output. `top` already sets its keys.
- `select FIELD[, FIELD...]`: once, event output only.
- `limit N`: once for event output or already-grouped event counts. Not allowed for global count, distinct, or top. For grouped count, `by` must precede `limit`; limit adds descending `_count` sorting. Without a grouped limit, no sort is added.

Conditions are `SIMPLE`, parenthesized conditions, homogeneous `CONDITION and CONDITION...`, or homogeneous `CONDITION or CONDITION...`. Accepted simple forms are:

```text
FIELD exists
FIELD is missing
FIELD is present
FIELD is VALUE
FIELD equals VALUE
FIELD = VALUE
FIELD is not VALUE including missing
FIELD is not VALUE excluding missing
FIELD differs from VALUE including missing
FIELD differs from VALUE excluding missing
FIELD >= NUMBER | FIELD <= NUMBER | FIELD > NUMBER | FIELD < NUMBER
FIELD matches glob "PATTERN"
FIELD matches regex "PATTERN" [case-insensitive]
FIELD contains "LITERAL SUBSTRING"
```

`case-insensitive` supplies regex flag `i`; no other English regex-flag syntax exists. There is no general `not`, negative glob/regex phrase, function call, arbitrary stage, field-to-field comparison, temporal phrase, join, or exception prose in this grammar. `count events where a equals 1 and b equals 2 or c equals 3` is unresolved; `count events where (a equals 1 and b equals 2) or c equals 3` is explicit. Unsupported/unconsumed clauses produce no query. Use structured input when this grammar cannot express the intent.

## Raw CQL checking

Raw checking parses an existing query; it does not accept the typed JSON variants as CQL. Conversely, a typed generic call takes typed expressions, not raw embedded expressions. The parser consumes the whole query, then checks recognized syntax/functions.

- Empty input, whitespace-only input and `*` are match-all inputs. Pipes separate stages and cannot trail the query.
- Whitespace, `//` line comments and closed `/* ... */` comments are consumed between tokens. Block comments are not nested. Quote slash-heavy text when it would be ambiguous with comments/regex.
- Bare identifiers admit alphanumeric characters, `_`, `@`, `#`, `.`, qualified `:` segments, and physical numeric indices such as `items[0].name`. Quote other filter/output names with double quotes. Double-quote assignment field names that start with a NOT, AND or OR segment in any ASCII letter case, such as `"not.user" := "alice"`. Expression fields must be bare-compatible. Backtick/single-quote syntax is outside this subset.
- Only straight double quotes delimit strings; single, curly and backtick delimiters are errors. Double-quoted strings cannot span raw lines. They decode `\t`, `\n`, `\r`, `\b`, `\f`, `\\`, `\"`, `\uXXXX` and `\UXXXXXXXX`; other backslash sequences retain the backslash. `\u{...}` is invalid. A control escape immediately followed by an ASCII letter or digit warns with `CQL_STRING_ESCAPE_CONTROL`; double the backslash to match a literal path. Filter patterns can be quoted/unquoted text, `/REGEX/flags`, or `?parameter`. Parameters with defaults, such as `?{name="value"}`, use their default for checking. Parameters without defaults remain unsupported. Saved-query/parser macro calls and saved-query argument substitution remain outside this profile. A recognized `beta:param()` call does not add parameter substitution to the parser.
- Filters support `=`, `!=`, `<`, `<=`, `>`, `>=`, `like`, standalone text/regex, `true`, `false`, `NOT`/`!`, parentheses, explicit AND and implicit whitespace AND. CQL filter **OR binds before AND**: `a=1 AND b=2 OR c=3` means `a=1 AND (b=2 OR c=3)`. Parenthesize intentionally. This differs from bounded English, which rejects mixed operators without grouping.
- Function calls use comma-separated positional/named arguments. Function lookup is case-insensitive; argument names/accepted value spellings must match their contracts. Assignments use `:=`. Expressions accept unary `!`, `-`, `+`; `* / %`, then `+ -`, then comparisons `== != <= >= < >`; binary operations associate left. Boolean `AND`/`OR` or `&&` are not scalar-expression operators.
- Arrays use `[VALUE,...]`; an aggregate list as a pipeline stage is parsed as `stats([...])`. Unlabeled subqueries use `{ PIPELINE }`; named queries use `Name: { PIPELINE }`. Named attributes are `include: ...`, `after: ...`, `before: ...`, with the context checks below.
- Ordered branches use `case { PIPELINE; PIPELINE }` and `FIELD match { PATTERN => PIPELINE; * => PIPELINE }`. A final semicolon is accepted. Links use `FIELD <=> Label.FIELD` in qualified correlation branches.

Regex syntax checks include literals, escaped metacharacters, character classes/ranges, alternation, capturing/noncapturing/named groups, repetition, anchors, common shorthand classes and word boundaries. Literal flags are `g`, `m`, `i`, `d`, `F` in any order, with repeats allowed; other flags report `CQL_REGEX_FLAGS`. Function flags remain distinct `i`, `m`, `d`, `F`; bounded inline `i/m/s` modifiers are checked. Lookarounds, backreferences, non-ASCII case folding, Unicode property/quoted escapes are outside the profile. Accepting `F` records the requested engine selection; it does not implement or verify that engine on the target. Function regex arguments are strings and have their own flags argument; omission uses the checked function default (`m` in regex validation). The checker parses syntax without executing a substitute regex engine.

## Function contracts

The executable catalog has bounded contracts for the 194 Query candidate names in [the function scope ledger](../data/function-scope.json). The official page enumeration reviewed on 2026-11-14 contains 196 names: 193 current Query candidates, deprecated Query use of `lowercase`, parser-only `copyEvent`, and unreleased `math:spherical2cartesian`. Deprecation is separate from availability: `lowercase` retains its own contract and is not an alias for `lower`.

```sh
cqlmatic catalog --functions --limit 250
cqlmatic catalog 'array:reduceRow' --functions
cqlmatic catalog 'cql:function:array:reduceRow' --full --limit 1
cqlmatic catalog --profile
```

`--functions` reports the current argument names, positional roles, required values, base effect, scalar support and requirements from [the Rust catalog](../src/catalog.rs). [Parameter facts](../data/function-parameters.json) supply qualified documented choices, bounds and defaults; Rust contracts remain the argument-type authority. An empty choice list is not permission for arbitrary semantics, and a deployment-controlled maximum is not made universal by its documented default.

Documentation membership, lookup guidance and executable forms are separate. The scope ledger records additional documented forms and their conditions; it does not certify each form as implemented. Typed calls must lower through the structured plan and pass the same contextual checks as parsed CQL. Bounded English covers only its grammar above. Field grounding, optional target capability observations and result assessment add different evidence; none establishes full vendor-compiler parity or validates unrestricted English interpretation.

String-array (`s[]`) arguments accept quoted strings, bare words and numeric literals, including decimals and negative numbers. For example, [in()](https://library.humio.com/crowdstrike-query-language/functions-in.html) accepts `in(a, values=[3, 6])`. Parameter choice restrictions still apply.

Argument type codes:

| Code | Checked shape |
| --- | --- |
| `f`, `F`, `A` | One field name; one or array of field names; array of field names |
| `s`, `S` | String/bare word; string or array of strings/bare words |
| `s[]` | Array of strings, bare words or numeric literals |
| `b`, `n`, `i`, `j` | Boolean; number; unsigned integer literal; signed integer within the checked integer contract |
| `l` | Unsigned integer or literal `max` |
| `v` | Simple scalar/word, including a negative numeric literal; not an arbitrary expression |
| `e`, `p`, `e[]` | Scalar expression; predicate expression (same shape check); array of scalar expressions |
| `r` | Quoted regex-pattern string, checked separately |
| `d` | Positive whole-number fixed duration |
| `y`, `z` | Structured array name ending `[]`; documented prefix-style array arguments where that contract permits them; no arbitrary field-pattern expansion |
| `u` | Nonaggregate inline function or unlabeled pipeline |
| `q`, `q[]` | Unlabeled query with no attributes; array of filter-only queries |
| `Q`, `Q[]` | Named query; array of named correlation aggregation queries |
| `g` | Aggregate call, qualified aggregate pipeline, or array of these; the nested context must actually aggregate |
| `P` | Scalar rename source or array of explicit `[old,new]` pairs |
| `a`, `N`, `B` | General array where qualified; numeric array; explicit bit-index/name pairs |
| `R`, `t` | Connection-dependent remote query; qualified time literal |

Fixed durations are whole numbers with `ms/millisecond(s)`, `s/sec/second(s)`, `m/min/minute(s)`, `h/hour(s)`, `d/day(s)`, or `w/week(s)`; spaces before the unit are accepted in quoted values. Zero, fractions, calendar units and anchored duration forms are unsupported. Signed anchor offsets additionally require `+` or `-`; a signed zero offset is syntactically supported but remains subject to interval/dimension checks.

### Function families

This family index covers every candidate name. Use the executable catalog output for the current per-argument contract; a family label does not permit every member in every scalar, aggregate or subquery position.

| Family | Canonical functions |
| --- | --- |
| Numerical aggregation | `avg`, `count`, `sum`, `min`, `max`, `range`, `stdDev`, `percentile`, `percentage`, `counterAsRate`, `callFunction`, `linReg`, `groupBy`, `stats`, `collect`, `selectLast`, `selectFromMax`, `selectFromMin`, `top` |
| Ordered sequences and sessions | `accumulate`, `appendAggregation`, `neighbor`, `partition`, `slidingTimeWindow`, `slidingWindow`, `series`, `session`, `window` |
| Time buckets and query time | `bucket`, `timeChart`, `duration`, `now`, `start`, `end`, `setTimeInterval`, `formatTime`, `findTimestamp` |
| Rows, fields and expressions | `head`, `tail`, `sort`, `table`, `select`, `drop`, `rename`, `eval`, `default`, `if`, `coalesce`, `getField`, `setField`, `split` |
| Event metadata and profiling | `eventFieldCount`, `eventInternals`, `eventSize`, `fieldset`, `fieldstats`, `explain:asTable` |
| Filters and regular expressions | `cidr`, `in`, `test`, `wildcard`, `regex`, `replace`, `sample`, `dropEvent` |
| Parsing | `parseCEF`, `parseCsv`, `parseFixedWidth`, `parseHexString`, `parseInt`, `parseJson`, `parseLEEF`, `parseTimestamp`, `parseUri`, `parseUrl`, `parseXml`, `kvParse` |
| Text and formatting | `lower`, `upper`, `lowercase`, `length`, `concat`, `concatArray`, `format`, `formatDuration`, `round`, `unit:convert`, `stripAnsiCodes`, `json:prettyPrint`, `xml:prettyPrint`, `writeJson`, `splitString` |
| Encoding and hashes | `base64Decode`, `base64Encode`, `urlDecode`, `urlEncode`, `hash`, `hashMatch`, `hashRewrite`, `tokenHash`, `shannonEntropy`, `crypto:md5`, `crypto:sha1`, `crypto:sha256` |
| Network and geography | `communityId`, `subnet`, `asn`, `ipLocation`, `geography:distance`, `geohash`, `reverseDns`, `bitfield:extractFlags`, `bitfield:extractFlagsAsArray`, `bitfield:extractFlagsAsString` |
| Independent sources, joins and tables | `createEvents`, `readFile`, `defineTable`, `remoteTable`, `readPersistedAggregation`, `join`, `selfJoin`, `selfJoinFilter`, `correlate`, `match`, `matchAsArray`, `ioc:lookup` |
| Visual output | `sankey`, `worldMap`, `transpose` |
| Query parameters and repeating mode | `beta:param`, `beta:repeating` |
| Flat arrays | `array:append`, `array:contains`, `array:dedup`, `array:drop`, `array:eval`, `array:exists`, `array:filter`, `array:intersection`, `array:length`, `array:reduceAll`, `array:reduceColumn`, `array:reduceRow`, `array:regex`, `array:rename`, `array:sort`, `array:union` |
| Nested arrays | `objectArray:eval`, `objectArray:exists` |
| Mathematics | `math:abs`, `math:arccos`, `math:arcsin`, `math:arctan`, `math:arctan2`, `math:ceil`, `math:cos`, `math:cosh`, `math:deg2rad`, `math:exp`, `math:expm1`, `math:floor`, `math:log`, `math:log10`, `math:log1p`, `math:log2`, `math:mod`, `math:pow`, `math:rad2deg`, `math:sin`, `math:sinh`, `math:sqrt`, `math:tan`, `math:tanh` |
| Unicode text and expression predicates | `text:contains`, `text:editDistance`, `text:editDistanceAsArray`, `text:endsWith`, `text:length`, `text:positionOf`, `text:startsWith`, `text:substring`, `text:trim` |
| Timestamp components | `time:dayOfMonth`, `time:dayOfWeek`, `time:dayOfWeekName`, `time:dayOfYear`, `time:hour`, `time:millisecond`, `time:minute`, `time:month`, `time:monthName`, `time:second`, `time:weekOfYear`, `time:year` |

### Rules not encoded by a table cell

Argument aliases are `replace.replacement -> with`, `kvParse.prefix -> as`, `match.table/readFile.table -> file`, and `join.repo -> view`. Supplying alias and canonical name together is a duplicate, not an override. A table alias also triggers declaration validation; a file name does not by itself establish that an external file exists.

Effects vary with arguments: `regex`/`match` with `strict=false` retain rows and resolve to transform; `ioc:lookup` filters only with `strict=true`, otherwise transforms; join with `include` or `mode=left` resolves to transform. Predicate calls must resolve to filtering forms. Negating a catalog transform other than `eval` or `ioc:lookup` runs the transform with `CQL_NEGATION_NO_EFFECT`; its argument, placement and output checks still apply. Scalar calls additionally reject retaining filter forms, regex with named captures or `repeat=true`, and `splitString` without `index` (array output). `test` is a pipeline filter, not a scalar computation.

Argument roles matter as much as shape: `join.key/include`, `defineTable.include`, `match.column/include`, `readFile.include`, and `cidr.column` refer to foreign/exported fields. `parseJson.include/exclude` are selector values, not input-field reads. `as`, `asArray`, `var`, and `timezoneAs` describe outputs/bindings. `drop.fields` removes fields; `rename.field` records renames. Scalar `field` expressions read fields; a literal string passed to a field-name argument names a field rather than supplying that field's content.

Query arguments normally start independent source/lineage contexts. `series.startmatch/endmatch` and `selfJoinFilter.where` continue the incoming context and must contain filters only. Non-aggregate array `function/condition` arguments establish scoped element bindings and prohibit aggregate/source/definition/correlation effects, including inside branches. The array reduction family instead permits qualified element aggregation. Aggregate `function` arguments accept aggregate calls, qualified composite pipelines, and arrays of them. A composite may filter, aggregate, then transform/project its result, but it must aggregate; output names and count lineage follow the final nested pipeline rather than every intermediate field. A scalar function by itself is not an aggregate. Context checks are recursive; listing a function in this index does not authorize using it in every nested position.

Default reads, outputs, conditional writes, projection and source replacement are tracked:

- Omitted input reads: regex/replace/parseJson/kvParse/splitString use `@rawstring`; formatTime uses `@timestamp`; asn/ipLocation use `ip`; split uses `_events[]`; temporal aggregates use `@timestamp`. `findTimestamp` also reads `@timezone` unless timezone/timezoneField supplies it. Wildcard with omitted/empty fields reads original event fields, not a caller-created placeholder.
- Common default scalar output names: `_lower`, `_upper`, `_length` (length/array:length), `_concat`, `_format`, `_if`, `_base64Decode`, `_urldecode`, `_shannonentropy`, `_coalesce`, `_getField`, `_splitstring`. `replace` defaults to rewriting its field or `@rawstring`; `round` defaults to its input field. Explicit `as` overrides the relevant default. `formatTime` requires `as` unless a direct `name := formatTime(...)` assignment supplies the output field. The `eval(name=formatTime(...))` form requires `as`. `getField` with a dynamic field name cannot prove which deployment field was read.
- Projection/aggregates retire input columns unless the specific aggregate retains them. Count defaults to `_count`; `range(field)` is a numeric reducer writing `_range` (maximum minus minimum), not a row-selection operation; group/stats/bucket/timeChart/session combine declared keys/functions (default count); bucket/timeChart add `_bucket`; series exports collected fields, `@timestamp`, `_duration`; fieldset exports `field`. Collect/selectLast export their fields; selectors export `include`; head/tail/sort retain inputs. Top exports its key(s), ranking output (`_count`, `_sum`, or `_max`, unless `as`), plus `percent` when requested. Review target runtime values and ties.
- `createEvents` emits raw strings; parse before comparing extracted fields. `createEvents`/`readFile` replace the stream, so earlier filters do not constrain replacement rows. `readFile(table=...)` can preserve a known declared export schema, while unknown all-field exports remain unknown. Creating a local field with the same name does not prove ownership of unknown imported members.
- `parseJson(handleNull="discard")` removes a null key/value pair, `empty` emits an empty string, and `keep` retains the text `null`; `excludeEmpty` is a separate rule. Nullable parsing, retaining regex, left joins and enrichments may leave incoming values or no value. A query-produced name is not proof of value presence or meaning. Conditional branch writes merge conservatively. Tag namespace mutation, including via aliases or imports, is rejected except when `default` fills a missing tag with `replaceEmpty` omitted or false.

### Endpoint and aggregation caveats

`base64Decode.charset` accepts UTF-8, UTF-16LE, UTF-16BE, ISO-8859-1. `kvParse.onDuplicate` accepts keepFirst/keepLast; `separatorPadding` yes/no/unknown; separator must be one character. Scalar rename requires `as`; pair renames cannot also use `as`, must have unique targets, and overlapping input/output pairs are unsupported. `round.how` is round/ceil/floor. `lower.type` is accepted with a warning because its distinction from locale is unresolved; verify target behavior.

`sort`/`table` order accepts asc/desc/ascending/descending in any letter case. Comparison types are lower-case hex/number/string; `table(type=any)` is invalid. Defaults apply when arguments are omitted: sort uses `_count`, descending numeric order and 200 rows; named limits and target ceilings are distinct. `top` cannot combine sum and max ranking; fractional error has a target-support warning. Collect has distinct-value/memory caps and undefined value order. SelectFromMax/Min ties choose an unspecified event; selectLast depends on event time/ties. Standalone formatTime with explicit seconds/milliseconds unit requires an exact signed-long integer timestamp literal, or warns for an unverified runtime expression; auto is also accepted. Formatting is presentation, not proof that its input was correctly interpreted.

Integer and limit parameters accept quoted integers and decimals with a zero fraction; bounds apply after conversion. String parameters also accept numbers; Boolean parameters accept bare or quoted true/false in any letter case. String arrays also accept singleton string, word or number values, Boolean elements and empty lists; nested arrays and executable values remain invalid. Numeric parameters require finite literals. Literal bounds are checked against both the exact decimal spelling and the double-precision reading; either reading outside the bounds is rejected. This includes the exclusive zero/one bounds of `percentile.accuracy`. `sample.percentage` accepts values from 0.01 to 100. Each value in `percentile.percentiles` must be between 0 and 100, inclusive. Both arguments accept bare and quoted numbers with the same bounds. `top.rest` accepts `true` or `false` in bare or quoted form. It also accepts a custom label for the remainder row. Other string parameters retain their own type and choice checks.

Assigned calls and expressions share dropped-field warnings with bare calls. Composite aggregate outputs and array element variables remain local; continuing filter queries retain the incoming event projection for these checks.

`setField` supports a literal mutable data-field target; computed target names are unsupported because immutable tag/read-only fields cannot be ruled out statically. Deprecated `lowercase` can transform a finite field-name set with `include=fields` or `both`; the reference examples leave old-name retention unqualified, so neither presence nor removal of an old name is guaranteed. Wildcard mutation is unsupported. `eventFieldCount`, `eventInternals` and `eventSize` require stored input before aggregation or source replacement. Internal event size is distinct from billed ingestion size. `reverseDns` requires a top-level aggregate result phase and cannot run in a subquery.

### Arrays

Use physical paths (`items[0].name`) for projection; a logical `items[]` marker is not whole-array projection or evidence that all indices/members exist. `array:eval` binds a scalar; objectArray functions bind members (`x.name`). Explicit `var` is a simple data identifier. Bindings and temporary inline fields do not escape into the outer event. Object member grounding remains tied to the source array, including under binding-name shadowing.

Eval functions must explicitly write the output base on every surviving inline branch; a later drop or projection can invalidate that proof. `objectArray:eval` requires a different output array. Eval/filter output reuse warns: old physical indices after a gap may survive, and no cleanup is inserted. A fresh prefix reduces collision risk but is not runtime shape proof. Nullable writes and unknown imported array members cannot establish complete ownership. `splitString` without index produces indexed output; scalar assignment requires index. `split` expands physical indices, adds `_index`, can rewrite `@id`, defaults to `_events[]`, and warns about shape/memory; verify strip mappings.

Element pipelines cannot aggregate, replace or define an event stream. These restrictions also apply to assigned calls and calls inside case/match branches.

`array:append`, `dedup`, `drop`, `rename` and `sort` operate on indexed scalar arrays; traversal gaps and destination reuse can change what survives. `array:reduceAll` reduces elements across events, `array:reduceRow` reduces each event's array, and `array:reduceColumn` groups corresponding indices across events. Union/intersection have aggregate set semantics and no promised output ordering. Their feature prerequisites do not apply to every function in the array namespace.

### Mixed sources, joins and tables

Join/match keys must be nonempty and have equal arity; repeated slots still count. Join mode is inner/left. Join limit is 1..200,000; `max` must be positive. Joining with omitted include imports no extra fields in the profile. Check cardinality and subquery caps; post-join row order is not established. External windows/views remain independent from source selectors and require deployment review.

`setTimeInterval` requires named `start`, as in `setTimeInterval(start=1d) | count()`, and can appear once, first at the top level before filters or other declarations. It overrides external query time settings and produces `CQL_QUERY_TIME_OVERRIDE`; review the effective interval against the intended API/MCP bounds. `defineTable` and `remoteTable` declarations occupy the following top-level preamble; table count is bounded to ten. A remote query is required in both connection forms, and the CQL subquery form also requires `include`. The declaration rules exclude combination with join/selfJoin/selfJoinFilter. For a declaration preamble, leave plan source/filters empty and place source filters after declarations. Includes are explicit exported fields or scalar all-fields `*`; wildcard list entries are unsupported. `match(table=...)`/`readFile(table=...)` require a preceding declaration. All-fields exports preserve unknown shape when the defining query lacks an exact projection; unknown does not prove a column present or absent.

The profile checks interval endpoints on `setTimeInterval`, `join`, `defineTable`, `remoteTable` and `readPersistedAggregation`: `now`, positive whole-number fixed lookbacks, nonzero signed offsets, signed-long epoch milliseconds and RFC3339 timestamps with an explicit offset. Existing string-typed endpoints require quoted epoch values. Unqualified spellings, including zero durations and strings such as `garbage`, produce `unsupported`, matching the other bounded duration checks. Numeric epoch zero remains supported. `remoteTable` requires named arguments. Catalog name-omission facts retain the parameter-table annotations; finite example-only positional forms for `getField` and `math:cosh` are explicit exceptions. `parseTimestamp` binds its unnamed argument to `format` and requires a named `field`.

All five interval functions reject provably reversed endpoints when both endpoints are explicit and within the same time domain: unsigned durations are lookbacks, signed durations are offsets, and absolute timestamps can be compared with epoch values. `setTimeInterval` also checks its omitted `end` as `now`. Equal endpoints and mixed relative/absolute endpoints are retained; their usefulness and effective runtime interval still require review.

Match mode is string/glob/cidr; deprecated `glob` cannot be combined with mode, and glob matching takes one scalar field/column rather than arrays. `nrows` is 1..500 or its supported max sentinel. Omitted include imports all non-key columns (tracked only when table columns are known); explicit include gives a bounded contract. `strict=false` retains unmatched rows. Enrichment capability/data are unverified: asn produces `.asn/.org`, ipLocation `.country/.city/.lon/.lat`, and ioc:lookup produces `.detected` plus nullable indexed members. IOC type is domain/ip_address/url; confidence is unverified/low/medium/high; include is limited to indicator, labels, last_updated, malicious_confidence, published_date, type. Do not interpret stale/incoming retained values as a fresh enrichment hit.

### Temporal and correlation

Bucket/timeChart span accepts auto or positive fixed duration; minSpan supports positive fixed duration, not numeric/calendar/anchored forms. Buckets are 1..1500, series limit 1..500; unit arrays have one element or one per aggregate. Boundaries depend on window/timezone; trailing data and series/memory caps affect completeness. Timestamp detection is nullable and can use current date/year; verify fully dated inputs and epoch values across timezone/DST. Session/series depend on timestamp order, key scope, gaps and memory; series memlimit is 1 byte..1 KiB with B/KB/KiB spelling. Neighbor requires input; direction is preceding/succeeding and distance 1..10,000. Establish order directly before neighbor when required; aggregation can invalidate prior order.

`accumulate`, `neighbor`, `partition`, `slidingWindow` and `slidingTimeWindow` require a preceding aggregate; uncertain ordering carries a sequence-order warning. `window` is a different form: use it only as the aggregate argument of `bucket` or `timeChart`, with exactly one of `span` or `buckets`. It excludes the current bucket, while event/time sliding-window forms include the current event unless configured otherwise.

`slidingTimeWindow.span` supports positive fixed durations and integer millisecond spans. Other spellings, including negative or zero spans, produce `unsupported`.

The bundled `series.memlimit` default is `1KiB`. Saved-result assessment treats both the omitted default and an explicit memory cap as unverified limits; returned rows do not establish whether the cap affected completeness.

Correlation must be top-level in an independent query, cannot follow aggregate/source replacement, and cannot combine with join. Generic correlation can inherit an ordinary prefix context; the specialized typed stage has stricter sole-stage placement. Mandatory queries cannot aggregate/replace/define streams or nest correlation. Aggregation queries can aggregate but cannot replace/define/nest correlation. Labels are unique across both lists. Links target a different mandatory label and bare field; place them directly in named pipelines. Mandatory links are last (only further links may follow); aggregation links precede aggregates. No links/globalConstraints across multiple mandatory queries warns about a Cartesian constellation.

Assigned aggregates obey the same placement rules as bare calls. Correlation input constraints are grounded before the first aggregate, including assigned reducers. `case` alternatives and `match` arms are filter queries: they may filter, assign or transform, but cannot call aggregates, `join`, `selfJoin` or `selfJoinFilter` (`CQL_BRANCH_FUNCTION`). Aggregate and join before or after the conditional. Function arguments with their own subqueries or sub-aggregates keep their argument-specific rules, and none of them may hold `join`, `selfJoin` or `selfJoinFilter` (`CQL_JOIN_PLACEMENT`); the subquery of `join` is its own query and may join at its top level.

Mandatory named queries may specify include fields or explicit all-fields. Aggregation queries may specify after/before anchors referring to mandatory names; other attribute/context combinations are unsupported. `sequenceBy` and `jitterTolerance` require sequence=true; jitter must not exceed within. Root names a mandatory query; iterationLimit is 1..10. Temporal offsets cannot use custom non-timestamp sequenceBy dimensions. A single open interval offset must be smaller than within; same-anchor zero/reversed intervals and provably reversed timestamp order are rejected. Different anchors retain a runtime-window warning: actual timestamps determine the interval, and within does not bound a closed interval.

Aggregation queries require a static at-most-one-row proof. Count/scalar reducers, qualified stats and head(1)/tail(1) can establish it; multiplicity-expanding operations can invalidate it. `collect(multival=false)` alone does not prove one row: add a later head(1)/tail(1). A reducer after repeated extraction can restore the bound. Correlation exports fields under `Label.field`, built-in event IDs/timestamps, and enabled constraint values; includeConstraintValues=false removes that implicit export route. Unknown include-all shapes remain unknown, not universal evidence for names below that prefix. Acceptance always retains a target capability/timestamp/runtime warning.

`CQL_UNEXPORTED_FIELD` warns when the query reads a field that a correlate does not export: after the correlate, a name outside its outputs; inside it, a field that correlate reads from a named query, such as either field of a link, or `@timestamp` when no `sequenceBy` is given, and that the query does not output at that point. In an aggregation query that point is its first aggregate. The warning names the fields and the named query they were expected from. A `QUERY.FIELD` read after the correlate is reported when that named query removed the field, for example with `drop`, even when the query has `include: *`. A name after the correlate that carries no query label is reported against correlate itself, which names the fields of each query `QUERY.FIELD`. A field removed after the correlate is not reported against it, and a later `select`, `table` or aggregate ends this tracing. `grounding.basis` keeps "not exported by preceding query" for each such field; with `--require-observed` the field is listed in `grounding.missing` instead of a warning. A read that `CQL_DROPPED_FIELD` also reports gets both warnings.

`CQL_JOIN_MAX` warns when a `join` without `max` includes a field, other than a join key, that a later filter or `test()` in the same query compares with `<`, `<=`, `>` or `>=`, as a time-window test does. `join` keeps one matching subquery row per key unless `max` is set, so the comparison may test only one of several matches. The check follows the included field and its copies made by a `:=` or `eval` expression through later filters and `test()`. Any other stage ends the check, including `rename`, an aggregate, `case`, `match` and a function written with `:=`. A `:=` or `eval` that sets the field from anything else stops following it, as does a filter that must match a regex with a named capture of the field. A subquery that returns one row, such as one ending in `head(1)`, is not reported. Neither is a subquery that ends in a `groupBy` on the join keys whose function returns one row per group, followed by nothing but `select`. The `groupBy` can leave out join keys that a `selectFromMax` or `selectFromMin` in its function includes. A `groupBy` `limit` in the subquery is not taken into account.

## Availability and optional function observations

The function ledger records minimum versions, feature flags, lifecycle and resource conditions separately from Query applicability.

`CQL_TARGET_REQUIREMENT` warns when a prerequisite has not been confirmed for the selected target; it does not reject supported syntax. Only the exact forms covered by the optional cases below can qualify through automated capability evidence. For other functions and forms, verify the documented prerequisite on the target. `CQL_TARGET_OBSERVED` is informational evidence for a matching form; `CQL_FUNCTION_LIFECYCLE` continues to warn about experimental, beta or deprecated usage independently. Examples of prerequisites include:

- `array:intersection`, `array:union`, `array:reduceRow` and `array:reduceColumn` require `ArrayFunctions`; the latter three carry experimental-use warnings in the reference.
- `beta:repeating` requires `RepeatingQueries`, is beta, and has no effect on an ordinary historical query. Its presence or a successful no-op does not confirm repeating behavior.
- `remoteTable` requires Falcon NG-SIEM and an accessible configured federation connection. CQL and other connection languages use different query forms. `readPersistedAggregation` requires an existing tagged aggregation; stored-result support is documented from 1.252 and rollup from 1.263.
- Lookup files/tables, IOC data, unexpired MaxMind data and DNS resolution are external prerequisites. Syntactic acceptance does not verify availability, permission, data freshness, lookup matches or enrichment output.
- `explain:asTable` is experimental ad hoc profiling that replaces ordinary results; its reference excludes scheduled searches, triggers and combination with correlate. Two-pass joins, live table snapshots and ordered sequence functions have distinct context restrictions.

After an ordinary field pack is active, inventory offers a separate optional function-check phase. It can be deferred without losing field-discovery progress. The shipped cases cover the four `ArrayFunctions` operations above and `math:sqrt`; they use fixed temporary-event queries and defined expected results. They do not cover all function names, argument forms, resource types, parser contexts or feature flags.

The inventory actions are `prepare-capabilities`, `dispatch-capability`, `submit-capability`, `pause-capabilities` and `resume-capabilities`. Preparation requires the confirmed environment, a caller-attested authorization-context identifier, the verified host session, finite attempt/time allowances and protected evidence of a provider-enforced search lifetime. Dispatch reserves one exact case and returns its tool arguments; it does not call MCP. Relay it only through the authorized host, retain the complete result and exact termination settlement, then submit them. Failed/retried attempts consume the same allowance. Unresolved termination blocks later inventory dispatch; pause/resume retains the original remaining budget.

An observation qualifies only for the exact authorization context, environment/CID and view, case revision and resource binding. The current fixed cases use no external resource binding. A completed result must satisfy its expected-output check; an error, timeout, missing result or different output remains `unknown`. A newer unknown attempt does not inherit an earlier success. Dated positive evidence expires after 30 days and must be refreshed for the same scope. Generation/checking uses `context.execution_context_id` to match that authorization context; the same CID alone is insufficient. This evidence does not change the ordinary field pack or attest general tenant functionality.

## Runnable examples and expected results

[Query-reference examples](../examples/query-reference/README.md) provide complete synthetic requests, canonical rendered CQL, expected diagnostics and offline commands for rate aggregation, arrays, independent-source joins, table lookup, buckets/order and named correlation anchors. Existing [endpoint classification](../examples/endpoint-classify.request.json) and [process grouping](../examples/process-count.plan.json) cover endpoint filters/ordered branches and grouping; reuse them instead of treating narrative examples as executable contracts.

These examples run with no pack and therefore exercise query construction/static acceptance, not observed grounding. No target query is executed. Review actual result warnings, source bindings, missing policies, caps, event ordering and field meaning before using a query against real data.

## Knowledge beyond executable coverage

Use `cqlmatic catalog 'TOPIC' --limit 10` for attributed knowledge, `catalog --functions --limit 250` for executable function contracts, `catalog --recipes` for recipes, and `catalog --profile` for limits. See [bundled knowledge provenance](../data/provenance.json) and the [official CQL documentation](https://library.humio.com/crowdstrike-query-language/) for broader language details. Lookup support does not expand checker coverage. For example, `dedup` remains unsupported; grouping plus projection only answers the narrower unique-key-row request, discards other columns, and still has grouping ceilings.

One leading or trailing empty pipeline stage is accepted with `CQL_EMPTY_STAGE` and omitted from canonical CQL; interior empty stages remain invalid. Boolean operators accept `and`/`AND`, `or`/`OR`, `not`/`NOT`; `like`, `true` and `false` use lower case. Other casing and reserved free-text words report `CQL_RESERVED_WORD`; quote reserved words to search for text.

`head`, `tail`, `sort` and `table` limits are 1..200,000 or `max`; `groupBy` limits are 1..1,000,000 or `max`; `top` requires an integer from 1 to 24,576 (0.75 × the historical maxMapSize of 32,768; see [top](https://library.humio.com/crowdstrike-query-language/functions-top.html)). See [sort](https://library.humio.com/crowdstrike-query-language/functions-sort.html) and [groupBy](https://library.humio.com/crowdstrike-query-language/functions-groupby.html).

`tail` warns with `CQL_TAIL_WITHOUT_TIMESTAMP` when the known input projection lacks `@timestamp`; unknown projections do not warn. Writing `@timestamp` again restores it. See [tail](https://library.humio.com/crowdstrike-query-language/functions-tail.html). `text:editDistanceAsArray.asArray` must end in `[]`, for example `asArray="distances[]"`; its output members use that array base.

Unquoted filter values and free-text words cannot contain `$`; put such text in double quotes. `?name` requires a supplied parameter value; `?{name=default}` substitutes its default while retaining surrounding text. A bare `?` requires double quotes. Inside double quotes or regex literals, `$` and `?` are ordinary characters.
