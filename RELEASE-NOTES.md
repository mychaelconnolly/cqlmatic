# CQLmatic v0.1.7

CQLmatic now checks many more query functions offline, can test a few functions against your own environment when you ask it to, and says so when it cannot fully verify a saved result. Discovery history from earlier releases stays readable after you upgrade.

## What changed

- **Wider function coverage.** Offline checking and generation cover 194 Query candidate function names with bounded contracts, up from 67, under the new `cqlmatic-ngsiem-core-v3` profile. `CQL_TARGET_REQUIREMENT` warns when a function or form has a documented minimum version, feature or resource prerequisite, and `CQL_FUNCTION_LIFECYCLE` warns about documented experimental, beta or deprecated ones. Placement, sequence, time-interval and argument-bound checks follow the bundled reference data, and `catalog --functions` and `catalog --profile` expose the added details.
- **Optional capability checks.** After a field pack is active, inventory can run five fixed function cases against your target within a separate finite allowance. They are off by default and need the same authorization as any other search. A current matching observation turns that form's `CQL_TARGET_REQUIREMENT` into informational `CQL_TARGET_OBSERVED` for the execution context you name with `--execution-context-id`; observations expire after 30 days.
- **Discovery history across upgrades.** Checkpoints written by 0.1.0–0.1.6 remain inspectable, and completed or terminally failed runs do not block new work. Continuing a run stays bound to the release that wrote it. `discovery-step`, `dispatch-discovery`, `pause-discovery` and `resume-discovery` accept `run_index` to select a retained run, and a run that completes after the inventory changed is recorded as `superseded` instead of failing.
- **Saved-result assessment.** `limit=max` on `sort`, `head`, `tail`, `table` and `groupBy` is assessed against the target's unknown ceiling and reported as inconclusive `RESULT_UPSTREAM_CAP_UNVERIFIED`. A final stage that returns more rows than its own limit allows reports `RESULT_CAP_EXCEEDED`. Column reporting lists at most 1,000 columns while the outcome still covers every column. After `array:drop` or `array:rename` of a literal array such as `a[]`, the elements the query writes from index 0 up to the first gap no longer leave a false `RESULT_NULL_FIELDS` warning under their old names.
- **Search settlement and Windows.** A hosted cancellation counts as terminated only when its saved evidence matches the exact attempt, a probe can have only one successful attempt, and native cleanup attempts every uncertain job it can identify. On Windows, reparse points are rejected for protected writes, and an existing protected input has its directory and file verified in one helper process.

## Compatibility

Inventory state versions 1–3 remain readable. Any state-writing action, including a `check` or `generate` that saves an inventory hint, writes version 4. A 0.1.6 binary cannot read version 4 state, rejects `cqlmatic-ngsiem-core-v3` packs, rejects requests that use `execution_context_id`, `run_index` or a capability action, and cannot open checkpoints that carry inventory owner metadata. Preserve the earlier state file before upgrading if you may return to 0.1.6. Request, status and pack format versions remain 1, and accepted earlier packs remain readable.

Finish unfinished discovery with the release that started it before you upgrade. An unfinished resumable run from an earlier release that has no owner binding blocks guarded inventory work with `MCP_STATE_PRODUCER`, even when safely paused. It can still be finished afterwards with that release's standalone resume, dispatch and answer steps, but a run finished that way is not integrated into the inventory automatically. See [recover an unowned earlier-release run](docs/INVENTORY.md#recover-an-unowned-earlier-release-run).

Some 0.1.6 query forms change outcome or gain warnings, and a saved result that was `no-findings` in 0.1.6 can become `inconclusive`. The [changelog](CHANGELOG.md) lists each change.

Known limits of saved-result assessment in this release: every `groupBy(..., limit=max)` result is inconclusive, including the generated count-by, distinct, top-values and threshold recipes, and a collected value list is not followed through an aggregate assigned to a new name.

## Before using it

Basic query writing needs no setup or discovery. Start with [setup](docs/SETUP.md), then inspect [inventory and status](docs/INVENTORY.md) when environment evidence is needed. Authorize the target, window and finite budgets before any search. Full discovery can take substantial time and assistant tokens; parser inventory requires a separate Falcon API client with NGSIEM Parsers: READ.

The npm package targets macOS Apple silicon and Linux x86_64 without install scripts or additional binary downloads. Standalone packaging supports Linux x86_64 and Windows x64. Verify release archives against `SHA256SUMS` before extraction. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`; the unsigned Windows executable uses a static CRT. Source builds require Rust 1.99.1.

`ready` describes local construction and the selected grounding policy. Neither it nor saved-result assessment proves vendor compilation, execution completeness or analyst-answer correctness. Saved-query observations retain the caller's assertion about query, target, window and response. Confirm operational results through your authorized CrowdStrike client; see [coverage and limits](docs/COVERAGE.md).

## Feedback

Include the version, platform and a minimal synthetic reproduction when reporting a problem. Keep credentials, tenant identifiers, packs and raw telemetry out of public issues. Use the [tester feedback form](https://github.com/mychaelconnolly/cqlmatic/issues/new/choose).
