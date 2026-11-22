# CQLmatic v0.1.11

With an environment map, query checks warn when a query names a source or field the map lacks. For Claude Code and Codex, a search guard checks each search before it runs and tells your assistant what `check` found. Checks accept more valid queries, catch more join, sorting, correlation, table-match, and filter traps, and report eleven general notes as informational output instead of warnings. The companion skill starts with writing and checking a query.

## Query checks

- With an environment map, `check` and `generate` warn about what the map lacks. `CQL_UNMAPPED_SOURCE` reports a `#repo` or `#type` value that matches no mapped source and names the mapped selectors. `CQL_UNMAPPED_FIELD` reports a field that no selected source lists, or one the map measured and did not observe. Fields that the query creates itself and `@` metadata fields are not reported.
- `CQL_DROPPED_FIELD` warns when a query reads a field that an earlier step removed, with or without a map.
- `CQL_GROUNDING_LENIENT` is removed. The evidence for each field stays in `grounding.basis`.
- `CQL_JOIN_MAX` warns when a `join` without `max` brings in a field that a later filter or `test()` compares with `<`, `<=`, `>` or `>=`, as a time-window test does. Without `max`, `join` keeps one matching row per key, so the test may see only one of several matches.
- `CQL_UNEXPORTED_FIELD` names a field that a `correlate` does not export, and the named query it was expected from.
- `CQL_SORT_HEAD` warns when `head` directly follows `sort`, unless the sort already sets a `limit` no larger than the `head`. From a typed plan with `sort` followed by `head(N)`, `generate` writes `sort(..., limit=N)`.
- `CQL_CORRELATION_SEQUENCE_BY` warns when a sequenced `correlate` sets `sequenceBy` to a field already equated across mandatory queries by `globalConstraints` or a same-field `<=>` link, where equal values fall back to later `sequenceBy` fields and then `@id` instead of default timestamp ordering. `correlate` accepts bracketed mandatory query lists (`query=[...]` or unnamed `[...]`) alongside unnamed query lists and a single `query=label:{...}`, and rejects mixing `query=` or a query list with extra query arguments (`CQL_DUPLICATE_ARGUMENT`).
- `CQL_TIME_BUCKET_SERIES_LIMIT` warns when `bucket` supplies a non-empty `field` or `timeChart` supplies `series` without `limit`, noting the default 10-series cap (maximum 500; `limit=max` is not supported).
- `CQL_MATCH_NROWS` warns when a non-negated `match` against an in-query `defineTable` imports a non-key column that a later filter or `test()` compares by order (`<`, `<=`, `>`, `>=`) without statically proving the table fits the effective `nrows` bound. External lookup files and `remoteTable` are not reported.
- Four filter-trap warnings cover common query mistakes. `CQL_EMPTY_STRING_NEGATION` warns on `field != ""` or `not field = ""` without a prior or sibling-`AND` presence guarantee on `field` (`test(field != "")` is not reported). `CQL_OR_FREE_TEXT` warns on pre-aggregate `or` filters that mix a field comparison with bare free text. `CQL_REGEX_PATH_ESCAPE` warns on single-backslash regex escapes in Windows-path contexts. `CQL_FILTER_PAREN_SPLIT` (`check` only) warns when an unquoted `(` immediately follows a filter comparison value.
- `CQL_ABSOLUTE_TIME_FILTER` is an informational note for a leading filter that compares `@timestamp` with a fixed number. Put the time window in the search request's `start` and `end` instead. The note does not change `ready`, and `--fail-on-warning` ignores it.
- Eleven general-fact diagnostics move from warnings to informational notes: `CQL_GROUPBY_LIMIT` with a numeric or omitted limit, `CQL_TARGET_REQUIREMENT`, `CQL_JOIN_COST`, `CQL_JOIN_AFTER_AGGREGATE`, `CQL_APPROXIMATE_DISTINCT`, `CQL_CORRELATION_CAPABILITY`, `CQL_RAW_SCAN_COST`, `CQL_LOOKUP_COMPLETENESS`, `CQL_TIME_BUCKET_LIMIT`, `CQL_COLLECT_LIMIT`, and `CQL_SPLIT_SHAPE`. Their messages and conditions do not change, they still appear in output, `--fail-on-warning` ignores them, and the search guard no longer reports them.
- `check` accepts more valid queries, including positional fields in `select`, a leading literal `]` in a regex character class, quoted numbers in `sample` percentages and percentile lists, omitted fields in `xml:prettyPrint` and `time:dayOfWeek`, fallbacks for missing tags in `default`, custom `top` remainder labels, and aliases in aggregate lists. `formatTime` requires `as` unless a direct `name := formatTime(...)` assignment names the output field.

See [the query reference](docs/QUERY-REFERENCE.md) and [coverage](docs/COVERAGE.md) for each rule.

## Search guard

`cqlmatic env capture --client CLIENT --server SERVER --install` registers a search guard for Claude Code and Codex alongside capture. Before each NG-SIEM search runs, the guard checks the query and passes your assistant a short note when `check` finds an error or a warning. A clean query, or one with only informational notes, gets no note. The guard never stops a search, makes no permission decision, and saves nothing. Use `--hooks capture,guard`, `capture`, `guard`, or `none` to choose which hooks to keep. See [checking each search before it runs](docs/SETUP.md#check-each-search-before-it-runs).

## Companion skill

The skill opens with the query loop: find the map, take the search target from it, look up source selectors and field names, write or generate the query, check it, run the search with its window in `start` and `end`, and assess the result. Mapping steps moved to a checklist at the end of [the MCP companion guide](docs/MCP-COMPANION.md#agent-mapping-checklist).

## Upgrading

- After upgrading, run `cqlmatic env capture --client CLIENT --server SERVER --install` again. It replaces the capture entry that 0.1.10 wrote for the same program, server, and state, and adds the guard.
- Before going back to an earlier release, remove both hooks with this release's `--hooks none`, then install with the earlier release.
- `--fail-on-warning` results can change in both directions. A query that 0.1.10 failed only because of `CQL_GROUNDING_LENIENT` or one of the eleven now-informational notes passes. A query with a map or one of the new query-logic traps can now fail.
- A few queries that 0.1.10 reported as ready are now invalid: out-of-range percentile values and quoted function names.
- An installed hook entry counts as installed only when it matches the entry the installer writes. An entry with an added key is reported as `manual`. In the install report, `event` is replaced by the list `events`, and a new `hooks` object gives each hook's result.
- Arrays that a query builds with `splitString` or writes by index are no longer saved as source field leads.
- CQLmatic 0.1.11 can inspect 0.1.10 discovery checkpoints in formats 2 and 3. Keep the 0.1.10 executable to resume or settle its unfinished runs. See [installation](docs/SETUP.md) for the package for your platform.
