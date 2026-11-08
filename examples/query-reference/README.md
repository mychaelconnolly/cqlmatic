# Synthetic query authoring examples

Run from the repository root with a built `cqlmatic` on PATH. These contain invented repository/field names and no environment pack. They verify generation and offline checking; they execute no query and establish no deployment observations. See the [full reference](../../docs/QUERY-REFERENCE.md) for contracts and [coverage](../../docs/COVERAGE.md) for assurance boundaries.

Each request is a complete schema-v1 `GenerateRequest`. Its matching `.cql` file is the actual canonical renderer output. Default generation and checking return `outcome: ready`, `validation.status: accepted`, `validation.execution: not-run`, and exit **0** for every row below. `--fail-on-warning` changes exit to **1** for warning rows; the informational aggregation diagnostic leaves exit **0**.

| Request / rendered query | Purpose | Expected diagnostics |
| --- | --- | --- |
| [aggregation-rate.request.json](aggregation-rate.request.json) / [CQL](aggregation-rate.cql) | Per-host event/failure counts, arithmetic rate, threshold and numeric ranking | `CQL_GROUPBY_LIMIT` **info** |
| [array-map.request.json](array-map.request.json) / [CQL](array-map.cql) | Flat-element binding, explicit squared output, physical-index projection | `CQL_ARRAY_OUTPUT_REUSE` warning |
| [mixed-join.request.json](mixed-join.request.json) / [CQL](mixed-join.cql) | Independent repository source, two ordered key pairs, left join, explicit exports/caps | `CQL_JOIN_COST` warning |
| [mixed-table.request.json](mixed-table.request.json) / [CQL](mixed-table.cql) | Declaration preamble, independent repository, explicit table exports, retaining lookup | `CQL_LOOKUP_COMPLETENESS` warning |
| [temporal-bucket.request.json](temporal-bucket.request.json) / [CQL](temporal-bucket.cql) | UTC fixed buckets, explicit numeric order and preceding neighbor | `CQL_TIME_BUCKET_LIMIT` warning |
| [temporal-correlation.request.json](temporal-correlation.request.json) / [CQL](temporal-correlation.cql) | Named mandatory queries, include fields/all, equality links and bounded count with signed anchors | `CQL_TIME_WINDOW_RUNTIME`, `CQL_CORRELATION_CAPABILITY` warnings |

```sh
cqlmatic generate --request examples/query-reference/aggregation-rate.request.json --json
cqlmatic check --file examples/query-reference/aggregation-rate.cql --json
cqlmatic generate --request examples/query-reference/array-map.request.json --fail-on-warning --json
```

The last command exits 1 because of its advisory warning. No query is withheld for that warning. For all cases, the following focused offline check verifies exact renderer output, ready/accepted status, diagnostic codes/severities, and both warning policies. It uses only the Python standard library and the existing binary:

```sh
python3 - <<'PY'
import json, pathlib, subprocess
folder = pathlib.Path('examples/query-reference')
expected = {
    'aggregation-rate': [('CQL_GROUPBY_LIMIT', 'info')],
    'array-map': [('CQL_ARRAY_OUTPUT_REUSE', 'warning')],
    'mixed-join': [('CQL_JOIN_COST', 'warning')],
    'mixed-table': [('CQL_LOOKUP_COMPLETENESS', 'warning')],
    'temporal-bucket': [('CQL_TIME_BUCKET_LIMIT', 'warning')],
    'temporal-correlation': [('CQL_TIME_WINDOW_RUNTIME', 'warning'),
                             ('CQL_CORRELATION_CAPABILITY', 'warning')],
}
for name, diagnostics in expected.items():
    canonical = (folder / (name + '.cql')).read_text().rstrip('\n')
    modes = [('generate', '--request', folder / (name + '.request.json')),
             ('check', '--file', folder / (name + '.cql'))]
    for mode, option, path in modes:
        for strict in (False, True):
            command = ['cqlmatic', mode, option, str(path), '--json']
            if strict:
                command.append('--fail-on-warning')
            run = subprocess.run(command, capture_output=True, text=True)
            result = json.loads(run.stdout)
            assert result['outcome'] == 'ready', result
            assert result['validation']['status'] == 'accepted', result
            assert result['validation']['execution'] == 'not-run', result
            assert [(d['code'], d['severity']) for d in result['diagnostics']] == diagnostics
            assert run.returncode == int(strict and any(s == 'warning' for _, s in diagnostics))
            if mode == 'generate':
                assert result['query'] == canonical, name
print('All six requests and rendered queries passed offline checks.')
PY
```

For existing endpoint cases, run:

```sh
cqlmatic generate --request examples/endpoint-classify.request.json --json
cqlmatic generate --plan examples/process-count.plan.json --json
```

Both are ready with exit 0; classification is an ordered case with explicit fallback, and process grouping emits informational `CQL_GROUPBY_LIMIT`. Their query fields remain deployment-unverified without a pack.

## All nine recipes

[recipes.jsonl](recipes.jsonl) contains one complete request for each recipe, in catalog order. All nine return ready with exit 0 under the default warning policy:

```sh
cqlmatic generate --jsonl < examples/query-reference/recipes.jsonl
```

Each ID is `recipe-` followed by the recipe name. Grouped recipes and exact distinct retain informational grouping ceilings; join-pivot and sequence add their join/correlation warnings. `--fail-on-warning` therefore makes the stream exit 1 while retaining ready result rows. The requests are synthetic, with no pack or deployment observations. See [recipe parameters and lowering](../../docs/QUERY-REFERENCE.md#all-nine-recipes).

## Negative boundaries

Negative boundary examples (each returns exit 1, without a generated ready query):

```sh
cqlmatic generate --intent 'count events where a equals 1 and b equals 2 or c equals 3' --json
cqlmatic generate --intent 'show events where user is not "admin"' --json
cqlmatic check 'user=?who' --json
cqlmatic check 'regex("x",field=user) OR user="admin"' --json
cqlmatic check 'sort(user,limit=max)' --json
cqlmatic check 'defineTable({true},name="t",include=*) | join({true},field=user)' --json
cqlmatic check 'correlate(A:{true},aggregation=[N:{collect([user],multival=false)}])' --json
```

Expected outcomes respectively: needs-context (`CQL_INTENT_UNRESOLVED`), needs-context (`CQL_INTENT_UNRESOLVED`), unsupported (`CQL_UNSUPPORTED_PARAMETER`), unsupported (`CQL_UNSUPPORTED_SYNTAX`, Boolean function composition), invalid (`CQL_ARGUMENT_TYPE`), invalid (`CQL_TABLE_JOIN_CONFLICT`), unsupported (`CQL_UNSUPPORTED_CORRELATION_ROWS`). Other advisory diagnostics can accompany these failures. An unsupported result is a coverage boundary, not a universal vendor-language rejection.

Inspect warnings before adapting examples. In particular, all grouping limits remain finite in practice; arrays can have gaps/stale indices; joins/lookups can retain nullable outputs; declared tables cannot coexist with join/selfJoinFilter; temporal windows/order/capability depend on the selected execution target. The bucket example's neighbor follows the resulting sorted rows and is not an implicit per-host previous-bucket calculation.
