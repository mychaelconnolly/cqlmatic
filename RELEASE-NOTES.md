# CQLmatic v0.1.12

Query checks give clearer rewrite guidance when a query drops or fails to export a field, misses a join or match row limit, or uses bare SQL/KQL stage words. Correlation export tracking now models `Label.<field>` built-in metadata accurately, explicit `match(..., nrows=...)` limits drop from warnings to informational notes, the companion skill sharpens pre-search check and field-path guidance, and four new runnable threat-hunting plan examples ship alongside updated project-page examples.

## Query checks

- **`correlate()` built-in metadata exports:** Mandatory queries now export `@id`, `@timestamp`, `@ingesttimestamp`, and `@timestamp.nanos` under their `Label.<field>` prefix (unless dropped or projected out inside that branch) rather than as bare `@id`, `@timestamp`, or `@ingesttimestamp`. Array elements rebuilt inside a branch after `array:drop(...)` stay exported, while unrebuilt dropped elements remain unavailable.
- **`CQL_MATCH_NROWS` severity split:** When `match()` against an in-query `defineTable` sets `nrows` explicitly (`nrows=max`, numeric `1..=500`, or parameterized), `CQL_MATCH_NROWS` is now an informational note instead of a warning. Omitting `nrows` (default `1`) remains a warning with concrete rewrite guidance.
- **Actionable rewrite guidance:** `CQL_UNEXPORTED_FIELD`, `CQL_DROPPED_FIELD`, `CQL_JOIN_MAX`, and omitted-`nrows` `CQL_MATCH_NROWS` now include concrete rewrite steps (such as adding `include: [field]` on a mandatory query or keeping a dropped field inside its branch pipeline).
- **Expanded `CQL_BARE_COMMAND_WORD` warnings:** `check` and the `PreToolUse` search guard now warn with direct CQL replacement hints on bare `limit`, `take`, `order`, `project`, `project-away`, `project-rename`, `summarize`, `extend`, and `distinct` pipeline stages.

See [the query reference](docs/QUERY-REFERENCE.md), [coverage](docs/COVERAGE.md), and [troubleshooting](docs/TROUBLESHOOTING.md) for each rule.

## Companion skill and examples

- Updated `integrations/agent/SKILL.md` to check every hand-written query before searching, verify parser-specific field paths and encodings against the map, and keep projections compact.
- Added four runnable threat-hunting plan examples in `examples/`: `php2shell-correlate.plan.json`, `node2shell-brotli.plan.json`, `runtime-shell-c2.plan.json`, and `node2shell-outbound.plan.json`.

## Upgrading

- `--fail-on-warning` and search-guard behavior can change in both directions:
  - Queries with explicit `match(..., nrows=...)` that previously warned only on `CQL_MATCH_NROWS` now pass `--fail-on-warning` and no longer trigger a search-guard note.
  - Queries that read bare `@id`/`@timestamp`/`@ingesttimestamp` after `correlate()`, or use bare `limit`/`take`/`order`/`project`/`project-away`/`project-rename`/`summarize`/`extend`/`distinct` stages, now warn.
- Existing `env capture --install` hooks (`capture`, `guard`) from `0.1.11` remain compatible; re-run `cqlmatic env capture --client CLIENT --server SERVER --install` if you move to a new executable path or pinned launcher version.
- CQLmatic 0.1.12 can inspect 0.1.11 (and earlier 0.1.0–0.1.10) discovery checkpoints in formats 2 and 3. Keep the 0.1.11 executable to resume or settle its unfinished runs. See [installation](docs/SETUP.md) for the package for your platform.
