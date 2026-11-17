# Synthetic MCP discovery walkthrough

`discover.request.json` binds an illustrative all-zero account, `view: "search-all"`
and `synthetic_repo`. The other JSON files are fabricated `falcon_search_ngsiem`
responses for offline testing, not deployment evidence. Do not send this identity
or fixture scopes to a live search service.

See [hosted search outcomes](../../docs/MCP-COMPANION.md#how-to-tell-whether-a-hosted-search-finished) for completion and error shapes, retry guidance and the positive control required for zero results.

Initialize the offline walkthrough with new protected local output paths:

```sh
cqlmatic env discover --mcp --request examples/mcp/discover.request.json --state .local/state.json --pack .local/pack.json --receipt .local/receipt.json
```

The fixture-to-probe mapping and full offline assessment loop are in
[CI](../../.github/workflows/ci.yml). Submit the matching fixture for each emitted
probe ID. Reserve that exact ID before supplying its matching fixture:

```sh
cqlmatic env discover --mcp --state .local/state.json --dispatch PROBE_ID
cqlmatic env discover --mcp --state .local/state.json --answer PROBE_ID=examples/mcp/MATCHING_FIXTURE.json
```

New runs use durable format 3: dispatch one outstanding probe, then submit its
matching answer before another dispatch. Accepted answers persist and replay
locally; they must not be dispatched or answered again. Poll without answers to
inspect the current frontier. Format-2 states retain their legacy multi-answer
and absolute-deadline behavior; they do not gain pause/resume allowances.
Exit `3` and `needs-probes` means continue; exit `3` and `activation-blocked`
means inspect the diagnostic and retry without answers after resolving it.
The pack may already be visible while receipt/final-state recovery remains pending.
Exit `0` means completed activation with its matching receipt. A format-3 unrecognized rejected
result retains exact bytes/hash and
returns `cleanup-pending` with exit `3`; it does not prove remote termination.
Resolve the same attempt with its acceptable original successful result or exact
bound host cleanup settlement before resuming. Do not bypass it with new paths.
Terminal `failed` states exit `2`; format-2 evidence failures retain that legacy
behavior. Failures before activation have no receipt. See the
[cancellation lifecycle](../../docs/ENVIRONMENT.md#cancel-and-resume-discovery).

For authorized operational discovery, create a separate request with the confirmed
`environment.view`, sources, UTC bounds and budgets. Relay every emitted
`mcp.arguments` exactly, save the entire untrusted MCP result exactly as received,
and submit that protected file without editing, trimming or reconstructing it.
Preserve the confirmed target for final query execution as well; `search-all` here
is only the synthetic example's target. No Falcon credentials are loaded by
`--mcp`, and CQLmatic does not start, stop or attest completeness of those MCP jobs.
The agent/tool host owns job cleanup. `--cancel` retains accepted evidence; an
in-flight call must settle before `--resume` can continue with the remaining
budgets. A cancellation request is not proof of remote cleanup. See
[cancel and resume](../../docs/ENVIRONMENT.md#cancel-and-resume-discovery) for
strict settlement evidence, native owner limits and activation recovery.
Discovery rejects responses over 8 MiB or with 10,000 rows or more.
State is bounded at 64 MiB; raw hex captures
consume that budget. Its hashes are unkeyed: they detect corruption and inconsistent
edits, not a party who rewrites state consistently. The agent is trusted to relay
MCP output.

See [the MCP workflow](../../docs/MCP-COMPANION.md),
[discovery options](../../docs/ENVIRONMENT.md), [contracts](../../docs/CONTRACTS.md)
and [result assessment](../../docs/RESULT-ASSESSMENT.md).

Recognized final answers from the hosted search tool end a format-3 MCP attempt as `timed-out`, `not-started` or `failed-after-start`; a completed list that cannot supply the required measurement ends it as `completed-unusable`. Exact terminal bytes and their hash are retained separately from accepted evidence. The run pauses, the attempt stays charged, and an explicit resume is required before another dispatch. Resume emits the same unaccepted probe. A cutoff does not prove the remote job stopped: at most one leftover job per cut-off attempt may overlap later searches. A missing answer or client wait without a tool answer keeps the attempt unresolved and blocks further dispatch. An unrecognized error shape or wording, malformed or incomplete response leaves it in `cleanup-pending`. Resolve that same attempt with an acceptable original successful result or exact bound cancellation report before resuming. Under the retained-run rules, a format-3 MCP checkpoint from 0.1.6 or 0.1.7 with exhausted active time and one distinct final answer can stop blocking other inventory searches, while lifecycle actions require its producing release. See [hosted search outcomes](../../docs/MCP-COMPANION.md).
