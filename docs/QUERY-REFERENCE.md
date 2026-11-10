# Query authoring reference

This is the executable authoring surface of profile `cqlmatic-ngsiem-core-v2`, schema version 1. It describes construction and offline checks, not the entire CrowdStrike Query Language (CQL). Start with [the workflow](FLOW.md) and [assurance boundaries](COVERAGE.md). Use [contracts](CONTRACTS.md) for command framing and [result assessment](RESULT-ASSESSMENT.md) after execution. A `ready` result means the implemented checks passed; execution is still `not-run`. It does not establish field meaning, vendor compiler parity, target capability, execution completeness, or the accuracy of unrestricted English interpretation.

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

Context supports optional `environment`, `confirmed_fields: []`, `require_observed: false`, `accept_mixed_record_kinds: false`, and `accept_partial_coverage: false`. The CLI merges `--require-observed`, `--accept-mixed-record-kinds`, and `--accept-partial-coverage` with request context; `grounding.policy` reports the effective values on every analysis outcome. Confirmed fields record caller-supplied context, not observations. Use the environment/grounding workflow for strict evidence; do not add confirmations merely to clear a diagnostic. CLI target selection for API or MCP execution is separate from query source scope: `#repo` filters rows inside the selected API/MCP target; it does not choose that target, switch tenants, authorize access, or establish repository availability.

Strict grounding normally requires qualifying source and field provenance with `complete`, `observed-only`, or `reconciled` coverage. `--accept-partial-coverage` also accepts positive, current source/field observations whose deployment provenance is marked `partial`; `CQL_PARTIAL_COVERAGE` retains the affected field, retrieval date and observation window (or identifies an unrecorded window). It does not accept stale, absent, nonobserved, ambiguous-source or mixed-record-kind evidence. With strict grounding disabled, `CQL_GROUNDING_LENIENT` warns only when a ready assessment relies on weak deployment evidence. Fully observed fields, built-ins and query-produced values do not produce that warning. Both advisories retain `ready`; `--fail-on-warning` returns exit 1.

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

An argument is `{"value": EXPRESSION}` for a positional argument or `{"name":"ARG","value":EXPRESSION}` for a named argument. `name` defaults to absent. Argument order is retained; duplicate resolved names, excess positional arguments, unknown arguments, and missing required arguments are diagnosed. Prefer named arguments when multiple roles could be confused.

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

Typed `head`/`tail` require a JSON integer. Generic calls and raw CQL additionally accept the literal `max` for `head`, `tail`, and `table` (StateRowLimit, default 200,000); `sort`, `top`, and `collect` remain integer-only. A row cap does not prove complete input processing.

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
- Bare identifiers admit alphanumeric characters, `_`, `@`, `#`, `.`, qualified `:` segments, and physical numeric indices such as `items[0].name`. Quote other filter/output names with double quotes. Expression fields must be bare-compatible. Backtick/single-quote syntax is outside this subset.
- Double-quoted strings use JSON escapes and cannot span raw lines. Filter patterns can be quoted/unquoted text, `/REGEX/flags`, or `?parameter`. Unresolved parameters are reported as unsupported; the checker does not substitute them.
- Filters support `=`, `!=`, `<`, `<=`, `>`, `>=`, `like`, standalone text/regex, `true`, `false`, `NOT`/`!`, parentheses, explicit AND and implicit whitespace AND. CQL filter **OR binds before AND**: `a=1 AND b=2 OR c=3` means `a=1 AND (b=2 OR c=3)`. Parenthesize intentionally. This differs from bounded English, which rejects mixed operators without grouping.
- Function calls use comma-separated positional/named arguments. Function lookup is case-insensitive; argument names/accepted value spellings must match their contracts. Assignments use `:=`. Expressions accept unary `!`, `-`, `+`; `* / %`, then `+ -`, then comparisons `== != <= >= < >`; binary operations associate left. Boolean `AND`/`OR` or `&&` are not scalar-expression operators.
- Arrays use `[VALUE,...]`; an aggregate list as a pipeline stage is parsed as `stats([...])`. Unlabeled subqueries use `{ PIPELINE }`; named queries use `Name: { PIPELINE }`. Named attributes are `include: ...`, `after: ...`, `before: ...`, with the context checks below.
- Ordered branches use `case { PIPELINE; PIPELINE }` and `FIELD match { PATTERN => PIPELINE; * => PIPELINE }`. A final semicolon is accepted. Links use `FIELD <=> Label.FIELD` in qualified correlation branches.

Regex syntax checks include literals, escaped metacharacters, character classes/ranges, alternation, capturing/noncapturing/named groups, repetition, anchors, common shorthand classes and word boundaries. Literal suffix flags are distinct `i`, `m`, `d`; bounded inline `i/m/s` modifiers are checked. Lookarounds, backreferences, non-ASCII case folding, Unicode property/quoted escapes and engine-switch flags are outside the profile. Function regex arguments are strings and have their own flags argument; omission uses the checked function default (`m` in regex validation). The checker parses syntax without executing a substitute regex engine.

## Function contracts

The index below is the complete 67-function output of `catalog --functions --limit 100`. It describes the bundled contract, not all vendor defaults. Every row's placement metadata is “pipeline; nested use requires the scalar, aggregate or subquery context checked by this profile.” `scalar=yes` is permission for qualified scalar forms, not every argument combination. Use `catalog FUNCTION --full` to find broader attributed knowledge; that knowledge can cover forms outside executable support.

Argument type codes:

| Code | Checked shape |
| --- | --- |
| `f`, `F`, `A` | One field name; one or array of field names; array of field names |
| `s`, `S`, `s[]` | String/bare word; string or array; array only |
| `b`, `n`, `i`, `j` | Boolean; number; unsigned integer literal; signed integer within the checked integer contract |
| `l` | Unsigned integer or literal `max` |
| `v` | Simple scalar/word, including a negative numeric literal; not an arbitrary expression |
| `e`, `p`, `e[]` | Scalar expression; predicate expression (same shape check); array of scalar expressions |
| `r` | Quoted regex-pattern string, checked separately |
| `d` | Positive whole-number fixed duration |
| `y` | Bare-compatible array name ending `[]`; no nested empty-index prefix/backtick member syntax |
| `u` | Nonaggregate inline function or unlabeled pipeline |
| `q`, `q[]` | Unlabeled query with no attributes; array of filter-only queries |
| `Q`, `Q[]` | Named query; array of named correlation aggregation queries |
| `g` | Aggregate call or aggregate-call array, with aggregate context checks |
| `P` | Scalar rename source or array of explicit `[old,new]` pairs |
| `a` | Syntactic array only; unqualified element/execution contract is unsupported |

Fixed durations are whole numbers with `ms/millisecond(s)`, `s/sec/second(s)`, `m/min/minute(s)`, `h/hour(s)`, `d/day(s)`, or `w/week(s)`; spaces before the unit are accepted in quoted values. Zero, fractions, calendar units and anchored duration forms are unsupported. Signed anchor offsets additionally require `+` or `-`; a signed zero offset is syntactically supported but remains subject to interval/dimension checks.

### Complete function contract index

“Required” and “positional” are independent: other listed arguments may be named, and aliases below resolve before duplicate checking. `eval` takes dynamic named expressions, not arbitrary unknown options. All rows also remain subject to the contextual rules after the table.

| Function | Named arguments and type codes | Required | Positional order | Base effect | Scalar |
| --- | --- | --- | --- | --- | --- |
| `cidr` | `field:f`, `subnet:S`, `file:s`, `column:f`, `negate:b` | `field` | `field` | filter | yes |
| `base64Decode` | `field:f`, `as:s`, `charset:s` | `field` | `field` | transform | yes |
| `urlDecode` | `field:f`, `as:s` | `field` | `field` | transform | yes |
| `shannonEntropy` | `field:f`, `as:s` | `field` | `field` | transform | yes |
| `splitString` | `field:f`, `by:r`, `as:s`, `index:j` | `by` | `field` | transform | yes |
| `coalesce` | `expressions:e[]`, `as:s`, `ignoreEmpty:b` | `expressions` | `expressions` | transform | yes |
| `getField` | `source:e`, `as:s` | `source` | `source` | transform | yes |
| `count` | `field:f`, `distinct:b`, `as:s` | — | `field` | aggregate | no |
| `groupBy` | `field:F`, `function:g`, `limit:l` | `field` | `field` | aggregate | no |
| `stats` | `function:g` | — | `function` | aggregate | no |
| `sum` | `field:f`, `as:s` | `field` | `field` | aggregate | no |
| `avg` | `field:f`, `as:s` | `field` | `field` | aggregate | no |
| `min` | `field:f`, `as:s` | `field` | `field` | aggregate | no |
| `max` | `field:f`, `as:s` | `field` | `field` | aggregate | no |
| `collect` | `fields:F`, `limit:i`, `multival:b`, `separator:s` | `fields` | `fields` | aggregate | no |
| `selectFromMax` | `field:f`, `include:F` | `field`, `include` | `field` | aggregate | no |
| `selectFromMin` | `field:f`, `include:F` | `field`, `include` | `field` | aggregate | no |
| `selectLast` | `fields:A` | `fields` | `fields` | aggregate | no |
| `top` | `field:F`, `as:s`, `error:n`, `limit:i`, `max:f`, `percent:b`, `rest:s`, `sum:f` | `field` | `field` | aggregate | no |
| `formatTime` | `format:s`, `as:s`, `field:e`, `locale:s`, `timezone:s`, `unit:s` | `format` | `format` | transform | yes |
| `select` | `fields:F` | `fields` | `fields` | transform | no |
| `drop` | `fields:A` | `fields` | `fields` | transform | no |
| `rename` | `field:P`, `as:s` | `field` | `field` | transform | no |
| `head` | `limit:l` | — | `limit` | aggregate | no |
| `tail` | `limit:l` | — | `limit` | aggregate | no |
| `sort` | `field:F`, `order:S`, `type:S`, `limit:i`, `reverse:b` | — | `field` | aggregate | no |
| `table` | `fields:F`, `sortby:F`, `order:S`, `type:S`, `limit:l`, `reverse:b` | `fields` | `fields` | aggregate | no |
| `fieldset` | — | — | — | aggregate | no |
| `eval` | dynamic named expressions | — | — | transform | no |
| `test` | `expression:p` | `expression` | `expression` | filter | no |
| `in` | `field:f`, `values:s[]`, `ignoreCase:b` | `field`, `values` | `field` | filter | yes |
| `wildcard` | `pattern:s`, `field:F`, `ignoreCase:b`, `includeEverythingOnAsterisk:b` | `pattern` | `pattern` | filter | yes |
| `regex` | `regex:r`, `field:f`, `flags:s`, `strict:b`, `repeat:b`, `limit:i` | `regex` | `regex` | filter | yes |
| `parseJson` | `field:f`, `prefix:s`, `exclude:F`, `include:F`, `handleNull:s`, `removePrefixes:s[]`, `excludeEmpty:b` | — | `field` | transform | no |
| `kvParse` | `field:F`, `as:s`, `prefix:s`, `excludeEmpty:b`, `onDuplicate:s`, `separator:s`, `separatorPadding:s`, `override:b` | — | `field` | transform | no |
| `lower` | `field:f`, `as:s`, `locale:s`, `type:s` | `field` | `field` | transform | yes |
| `upper` | `field:f`, `as:s`, `locale:s` | `field` | `field` | transform | yes |
| `length` | `field:f`, `as:s` | `field` | `field` | transform | yes |
| `concat` | `field:A`, `as:s`, `separator:s` | `field` | `field` | transform | yes |
| `split` | `field:f`, `strip:b` | — | `field` | transform | no |
| `replace` | `regex:r`, `with:s`, `field:f`, `as:s`, `flags:s` | `regex` | `regex` | transform | yes |
| `format` | `format:s`, `field:F`, `as:s`, `timezone:s` | `format`, `field` | `format` | transform | yes |
| `default` | `field:F`, `value:v`, `replaceEmpty:b` | `field`, `value` | `field` | transform | no |
| `round` | `field:f`, `as:s`, `how:s` | `field` | `field` | transform | yes |
| `if` | `condition:e`, `then:e`, `else:e`, `as:s` | `condition`, `then`, `else` | `condition` | transform | yes |
| `array:contains` | `array:f`, `value:e` | `array`, `value` | `array` | filter | yes |
| `array:length` | `array:f`, `as:s` | `array` | `array` | transform | yes |
| `array:eval` | `array:y`, `asArray:y`, `function:u`, `var:s` | `array`, `function` | `array` | transform | no |
| `array:filter` | `array:y`, `asArray:y`, `function:u`, `var:s` | `array`, `function` | `array` | transform | no |
| `objectArray:eval` | `array:y`, `asArray:y`, `function:u`, `var:s` | `array`, `asArray`, `function` | `array` | transform | no |
| `objectArray:exists` | `array:y`, `condition:u`, `var:s` | `array`, `condition` | `array` | filter | yes |
| `join` | `query:q`, `field:F`, `key:F`, `include:F`, `mode:s`, `limit:i`, `max:i`, `start:s`, `end:s`, `view:s`, `repo:s`, `live:b` | `query`, `field` | `query` | filter | no |
| `defineTable` | `name:s`, `query:q`, `include:F`, `start:s`, `end:s`, `view:s` | `name`, `query`, `include` | `query` | definition | no |
| `match` | `file:s`, `table:s`, `field:F`, `column:F`, `include:F`, `strict:b`, `ignoreCase:b`, `nrows:l`, `glob:b`, `mode:s` | `file`, `field` | `file` | filter | no |
| `asn` | `field:f`, `as:s` | — | `field` | transform | no |
| `ipLocation` | `field:f`, `as:s` | — | `field` | transform | no |
| `ioc:lookup` | `field:F`, `type:s`, `confidenceThreshold:s`, `include:s[]`, `prefix:s`, `strict:b` | `field`, `type` | `field` | filter | no |
| `selfJoinFilter` | `field:F`, `where:q[]`, `prefilter:b` | `field`, `where` | `field` | filter | no |
| `correlate` | `query:Q`, `sequence:b`, `within:d`, `globalConstraints:F`, `sequenceBy:F`, `root:s`, `maxPerRoot:i`, `iterationLimit:i`, `jitterTolerance:d`, `includeConstraintValues:b`, `includeMatchesOnceOnly:b`, `aggregation:Q[]` | `query` | `query` | correlation | no |
| `bucket` | `field:F`, `function:g`, `span:s`, `buckets:i`, `limit:i`, `minSpan:s`, `timezone:s`, `unit:S` | — | `span` | aggregate | no |
| `timeChart` | `series:f`, `function:g`, `span:s`, `buckets:i`, `limit:i`, `minSpan:s`, `timezone:s`, `unit:S` | — | `series` | aggregate | no |
| `session` | `function:g`, `maxpause:d` | — | `function` | aggregate | no |
| `series` | `collect:F`, `maxpause:d`, `maxduration:d`, `startmatch:q`, `endmatch:q`, `separator:s`, `memlimit:s` | `collect` | `collect` | aggregate | no |
| `neighbor` | `include:F`, `direction:s`, `distance:i`, `prefix:s` | `include` | `include` | transform | no |
| `readFile` | `file:S`, `table:S`, `include:F`, `limit:i` | `file` | `file` | source | no |
| `findTimestamp` | `addErrors:b`, `as:s`, `field:f`, `timezone:s`, `timezoneField:f`, `timezoneAs:s` | — | — | transform | no |
| `createEvents` | `rawstring:s[]` | `rawstring` | `rawstring` | source | no |

### Rules not encoded by a table cell

Argument aliases are `replace.replacement -> with`, `kvParse.prefix -> as`, `match.table/readFile.table -> file`, and `join.repo -> view`. Supplying alias and canonical name together is a duplicate, not an override. A table alias also triggers declaration validation; a file name does not by itself establish that an external file exists.

Effects vary with arguments: `regex`/`match` with `strict=false` retain rows and resolve to transform; `ioc:lookup` filters only with `strict=true`, otherwise transforms; join with `include` or `mode=left` resolves to transform. Predicate calls must resolve to filtering forms. Negating retaining transforms is unsupported. Scalar calls additionally reject retaining filter forms, regex with named captures or `repeat=true`, and `splitString` without `index` (array output). `test` is a pipeline filter, not a scalar computation.

Argument roles matter as much as shape: `join.key/include`, `defineTable.include`, `match.column/include`, `readFile.include`, and `cidr.column` refer to foreign/exported fields. `parseJson.include/exclude` are selector values, not input-field reads. `as`, `asArray`, `var`, and `timezoneAs` describe outputs/bindings. `drop.fields` removes fields; `rename.field` records renames. Scalar `field` expressions read fields; a literal string passed to a field-name argument names a field rather than supplying that field's content.

Query arguments normally start independent source/lineage contexts. `series.startmatch/endmatch` and `selfJoinFilter.where` continue the incoming context and must contain filters only. Array `function/condition` arguments establish scoped element bindings and prohibit aggregate/source/definition/correlation effects, including inside branches. Aggregate `function` arguments require aggregate calls, not arbitrary scalar calls. Context checks are recursive; listing a function in this index does not authorize using it in every nested position.

Default reads, outputs, conditional writes, projection and source replacement are tracked:

- Omitted input reads: regex/replace/parseJson/kvParse/splitString use `@rawstring`; formatTime uses `@timestamp`; asn/ipLocation use `ip`; split uses `_events[]`; temporal aggregates use `@timestamp`. `findTimestamp` also reads `@timezone` unless timezone/timezoneField supplies it. Wildcard with omitted/empty fields reads original event fields, not a caller-created placeholder.
- Common default scalar output names: `_lower`, `_upper`, `_length` (length/array:length), `_concat`, `_format`, `_if`, `_base64Decode`, `_urldecode`, `_shannonentropy`, `_coalesce`, `_getField`, `_splitstring`. `replace` defaults to rewriting its field or `@rawstring`; `round` defaults to its input field. Explicit `as` overrides the relevant default. Standalone `formatTime` requires `as`; an assignment supplies its output context. `getField` with a dynamic field name cannot prove which deployment field was read.
- Projection/aggregates retire input columns unless the specific aggregate retains them. Count defaults to `_count`; group/stats/bucket/timeChart/session combine declared keys/functions (default count); bucket/timeChart add `_bucket`; series exports collected fields, `@timestamp`, `_duration`; fieldset exports `field`. Collect/selectLast export their fields; selectors export `include`; head/tail/sort retain inputs. Top exports its key(s), ranking output (`_count`, `_sum`, or `_max`, unless `as`), plus `percent` when requested. Review target runtime values and ties.
- `createEvents` emits raw strings; parse before comparing extracted fields. `createEvents`/`readFile` replace the stream, so earlier filters do not constrain replacement rows. `readFile(table=...)` can preserve a known declared export schema, while unknown all-field exports remain unknown. Creating a local field with the same name does not prove ownership of unknown imported members.
- Nullable parsing, retaining regex, left joins and enrichments may leave incoming values or no value. A query-produced name is not proof of value presence or meaning. Conditional branch writes merge conservatively. Tag namespace mutation remains rejected, including via aliases/imports.

### Endpoint and aggregation caveats

`base64Decode.charset` accepts UTF-8, UTF-16LE, UTF-16BE, ISO-8859-1. `kvParse.onDuplicate` accepts keepFirst/keepLast; `separatorPadding` yes/no/unknown; separator must be one character. Scalar rename requires `as`; pair renames cannot also use `as`, must have unique targets, and overlapping input/output pairs are unsupported. `round.how` is round/ceil/floor. `lower.type` is accepted with a warning because its distinction from locale is unresolved; verify target behavior.

`sort`/`table` order is asc/desc and type is number/string. `top` cannot combine sum and max ranking; fractional error has a target-support warning. Collect has distinct-value/memory caps and undefined value order. SelectFromMax/Min ties choose an unspecified event; selectLast depends on event time/ties. Standalone formatTime with explicit seconds/milliseconds unit requires an exact signed-long integer timestamp literal, or warns for an unverified runtime expression; auto is also accepted. Formatting is presentation, not proof that its input was correctly interpreted.

### Arrays

Use physical paths (`items[0].name`) for projection; a logical `items[]` marker is not whole-array projection or evidence that all indices/members exist. `array:eval` binds a scalar; objectArray functions bind members (`x.name`). Explicit `var` is a simple data identifier. Bindings and temporary inline fields do not escape into the outer event. Object member grounding remains tied to the source array, including under binding-name shadowing.

Eval functions must explicitly write the output base on every surviving inline branch; a later drop or projection can invalidate that proof. `objectArray:eval` requires a different output array. Eval/filter output reuse warns: old physical indices after a gap may survive, and no cleanup is inserted. A fresh prefix reduces collision risk but is not runtime shape proof. Nullable writes and unknown imported array members cannot establish complete ownership. `splitString` without index produces indexed output; scalar assignment requires index. `split` expands physical indices, adds `_index`, can rewrite `@id`, defaults to `_events[]`, and warns about shape/memory; verify strip mappings.

### Mixed sources, joins and tables

Join/match keys must be nonempty and have equal arity; repeated slots still count. Join mode is inner/left. Join limit is 1..200,000; `max` must be positive. Joining with omitted include imports no extra fields in the profile. Check cardinality and subquery caps; post-join row order is not established. External windows/views remain independent from source selectors and require deployment review.

`defineTable` declarations form a top-level preamble before any input operation, at most ten tables. They cannot coexist with join/selfJoinFilter. For a declaration preamble, leave plan source/filters empty and place source filters after declarations. Includes are explicit exported fields or scalar all-fields `*`; wildcard list entries are unsupported. `match(table=...)`/`readFile(table=...)` require a preceding declaration. All-fields exports preserve unknown shape when the defining query lacks an exact projection; unknown does not prove a column present or absent.

Match mode is string/glob/cidr; deprecated `glob` cannot be combined with mode, and glob matching takes one scalar field/column rather than arrays. `nrows` is 1..500 or its supported max sentinel. Omitted include imports all non-key columns (tracked only when table columns are known); explicit include gives a bounded contract. `strict=false` retains unmatched rows. Enrichment capability/data are unverified: asn produces `.asn/.org`, ipLocation `.country/.city/.lon/.lat`, and ioc:lookup produces `.detected` plus nullable indexed members. IOC type is domain/ip_address/url; confidence is unverified/low/medium/high; include is limited to indicator, labels, last_updated, malicious_confidence, published_date, type. Do not interpret stale/incoming retained values as a fresh enrichment hit.

### Temporal and correlation

Bucket/timeChart span accepts auto or positive fixed duration; minSpan supports positive fixed duration, not numeric/calendar/anchored forms. Buckets are 1..1500, series limit 1..500; unit arrays have one element or one per aggregate. Boundaries depend on window/timezone; trailing data and series/memory caps affect completeness. Timestamp detection is nullable and can use current date/year; verify fully dated inputs and epoch values across timezone/DST. Session/series depend on timestamp order, key scope, gaps and memory; series memlimit is 1 byte..1 KiB with B/KB/KiB spelling. Neighbor requires input; direction is preceding/succeeding and distance 1..10,000. Establish order directly before neighbor when required; aggregation can invalidate prior order.

Correlation must be top-level in an independent query, cannot follow aggregate/source replacement, and cannot combine with join. Generic correlation can inherit an ordinary prefix context; the specialized typed stage has stricter sole-stage placement. Mandatory queries cannot aggregate/replace/define streams or nest correlation. Aggregation queries can aggregate but cannot replace/define/nest correlation. Labels are unique across both lists. Links target a different mandatory label and bare field; place them directly in named pipelines. Mandatory links are last (only further links may follow); aggregation links precede aggregates. No links/globalConstraints across multiple mandatory queries warns about a Cartesian constellation.

Mandatory named queries may specify include fields or explicit all-fields. Aggregation queries may specify after/before anchors referring to mandatory names; other attribute/context combinations are unsupported. `sequenceBy` and `jitterTolerance` require sequence=true; jitter must not exceed within. Root names a mandatory query; iterationLimit is 1..10. Temporal offsets cannot use custom non-timestamp sequenceBy dimensions. A single open interval offset must be smaller than within; same-anchor zero/reversed intervals and provably reversed timestamp order are rejected. Different anchors retain a runtime-window warning: actual timestamps determine the interval, and within does not bound a closed interval.

Aggregation queries require a static at-most-one-row proof. Count/scalar reducers, qualified stats and head(1)/tail(1) can establish it; multiplicity-expanding operations can invalidate it. `collect(multival=false)` alone does not prove one row: add a later head(1)/tail(1). A reducer after repeated extraction can restore the bound. Correlation exports fields under `Label.field`, built-in event IDs/timestamps, and enabled constraint values; includeConstraintValues=false removes that implicit export route. Unknown include-all shapes remain unknown, not universal evidence for names below that prefix. Acceptance always retains a target capability/timestamp/runtime warning.

## Runnable examples and expected results

[Query-reference examples](../examples/query-reference/README.md) provide complete synthetic requests, canonical rendered CQL, expected diagnostics and offline commands for rate aggregation, arrays, independent-source joins, table lookup, buckets/order and named correlation anchors. Existing [endpoint classification](../examples/endpoint-classify.request.json) and [process grouping](../examples/process-count.plan.json) cover endpoint filters/ordered branches and grouping; reuse them instead of treating narrative examples as executable contracts.

These examples run with no pack and therefore exercise query construction/static acceptance, not observed grounding. No target query is executed. Review actual result warnings, source bindings, missing policies, caps, event ordering and field meaning before using a query against real data.

## Knowledge beyond executable coverage

Use `cqlmatic catalog 'TOPIC' --limit 10` for attributed knowledge, `catalog --functions --limit 100` for executable function contracts, `catalog --recipes` for recipes, and `catalog --profile` for limits. See [bundled knowledge provenance](../data/provenance.json) and the [official CQL documentation](https://library.humio.com/crowdstrike-query-language/) for broader language details. Lookup support does not expand checker coverage. For example, `dedup` remains unsupported; grouping plus projection only answers the narrower unique-key-row request, discards other columns, and still has grouping ceilings.
