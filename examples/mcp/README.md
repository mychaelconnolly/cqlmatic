# Synthetic MCP discovery walkthrough

`discover.request.json` binds an illustrative all-zero account, `view: "search-all"`
and `synthetic_repo`. The other JSON files are fabricated `falcon_search_ngsiem`
responses for offline testing, not deployment evidence. Do not send this identity
or fixture scopes to a live search service.

Initialize the offline walkthrough with new protected local output paths:

```sh
cqlmatic env discover --mcp --request examples/mcp/discover.request.json --state .local/state.json --pack .local/pack.json --receipt .local/receipt.json
```

The fixture-to-probe mapping and full offline assessment loop are in
[CI](../../.github/workflows/ci.yml). Submit the matching fixture for each emitted
probe ID with:

```sh
cqlmatic env discover --mcp --state .local/state.json --answer PROBE_ID=examples/mcp/MATCHING_FIXTURE.json
```

Multiple `--answer` flags are accepted, but answers persist sequentially. If a
later file fails, earlier answers may already be accepted: resume without answers
to inspect outstanding probes before retrying. Never blindly replay a batch.
Exit `3` and `needs-probes` means continue; exit `3` and `activation-blocked`
means inspect the diagnostic and retry without answers after resolving it.
The pack may already be visible while receipt/final-state recovery remains pending.
Exit `0` means completed activation with its matching receipt. Exit `2` and
terminal `failed` state requires a corrected run with new state and receipt paths;
failures before activation have no receipt.

For authorized operational discovery, create a separate request with the confirmed
`environment.view`, sources, UTC bounds and budgets. Relay every emitted
`mcp.arguments` exactly, save the entire untrusted MCP result exactly as received,
and submit that protected file without editing, trimming or reconstructing it.
Preserve the confirmed target for final query execution as well; `search-all` here
is only the synthetic example's target. No Falcon credentials are loaded by
`--mcp`, and CQLmatic does not start, stop or attest completeness of those MCP jobs.
The agent/tool host owns job cleanup. Discovery rejects responses over 8 MiB or
with 10,000 rows or more. State format 2 is bounded at 64 MiB; raw hex captures
consume that budget. Its hashes are unkeyed: they detect corruption and inconsistent
edits, not a party who rewrites state consistently. The agent is trusted to relay
MCP output.

See [the MCP workflow](../../docs/MCP-COMPANION.md),
[discovery options](../../docs/ENVIRONMENT.md), [contracts](../../docs/CONTRACTS.md)
and [result assessment](../../docs/RESULT-ASSESSMENT.md).
