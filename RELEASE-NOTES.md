# CQLmatic v0.1.8

`check` corrects offline verdicts for many query forms it misclassified in 0.1.7, accepting more supported forms and rejecting more invalid ones. It also warns about queries that run but search for text where a function or operator was meant, recognizes the hosted search tool's error answers, and lets a discovery attempt end on them instead of blocking.

## What changed

- **Queries that do not run are rejected.** Free text after the first aggregate (`groupBy(y) | head 10`), a bare function name as an argument (`stats(count)`), `field := f()` for a function without an `as` parameter, an aggregate or join inside a `case` or `match` branch, operators in mixed case (`a=1 Or b=2`), curly quotes, single quotes and backticks as delimiters, an unquoted `$`, row limits outside 1 to 200,000, group limits outside 1 to 1,000,000, and `top` limits above 24,576 are now errors. `parseTimestamp`, `math:mod` and `setTimeInterval` follow their real argument rules.
- **Queries that do run are accepted.** Quoted numbers and booleans as arguments (`head("10")`, `distinct="true"`), a single value where a list is expected (`in(a, values=1)`), numbers in value lists, regex literal flags `g`, `m`, `i`, `d` and `F`, parameters with a default (`?{name="x"}`), backslash sequences such as `"C:\Windows"`, one leading or trailing empty pipe, and negated transforms are no longer rejected or reported as unknown.
- **Warnings for queries that mean something else.** A function name without parentheses (`| count`), a command word from another query language (`where a=1`), `&&`, a comparison with the bare word `null`, `or` mixed with `and` without parentheses, a tab or newline escape inside a path (`"C:\temp"`), `tail()` after a step that removes `@timestamp`, and a negation that has no effect each get a diagnostic. The query stays accepted.
- **Hosted search answers.** An answer whose `result` is an object with an error is reported as a timeout or a tool error instead of an unsupported shape, and the guides explain how to tell whether a hosted search finished. `assess` asks for a positive control before a zero result is read as "no matches".
- **Discovery attempts can end.** In a resumable discovery run, a hosted cutoff, a start rejection, a failure after start, or a completed result that cannot be used now ends the attempt. The attempt stays charged, the run pauses, and an explicit resume sends the probe again as a new attempt. A lost response still blocks until it is settled.

## Compatibility

The profile is still `cqlmatic-ngsiem-core-v3`. Its knowledge revision changes. Packs written by 0.1.7 remain readable and keep their original binding; a 0.1.7 binary rejects packs using the new knowledge revision with `PACK_PROFILE` (the producer version alone does not determine admission). Request, status and pack format versions remain 1. Inventory state remains version 4, as in 0.1.7; the optional field-evidence version 3 and discovery checkpoint formats 2 and 3 are unchanged. A 0.1.7 binary rejects checkpoints produced by 0.1.8 with `MCP_STATE_TAMPERED`.

Checkpoints written by 0.1.0 to 0.1.7 remain inspectable. Continuing a run stays bound to the release that wrote it, and an attempt that was already blocked under an earlier release stays blocked: the new attempt endings apply to runs started with 0.1.8. Finish unfinished discovery with the release that started it before you upgrade.

Some queries that 0.1.7 accepted are now rejected, including `head(0)` and `table([a], type=any)`. Some that it rejected or could not classify are now accepted, and accepted queries can carry new warnings. The [changelog](CHANGELOG.md) lists each change with an example.

Functions and forms that need a newer version or a feature a target may not have, such as `wildcard()` inside `if()` or the newer array functions, are still accepted with `CQL_TARGET_REQUIREMENT`: whether they run depends on the target.

## Before using it

Basic query writing needs no setup or discovery. Start with [setup](docs/SETUP.md), then inspect [inventory and status](docs/INVENTORY.md) when environment evidence is needed. Authorize the target, window and finite budgets before any search. Full discovery can take substantial time and assistant tokens; parser inventory requires a separate Falcon API client with NGSIEM Parsers: READ.

The npm package targets macOS Apple silicon and Linux x86_64 without install scripts or additional binary downloads. Standalone packaging supports Linux x86_64 and Windows x64. Verify release archives against `SHA256SUMS` before extraction. Linux requires glibc 2.34 or newer and `libgcc_s.so.1`; the unsigned Windows executable uses a static CRT. Source builds require Rust 1.99.1.

`ready` describes local construction and the selected grounding policy. Neither it nor saved-result assessment proves vendor compilation, execution completeness or analyst-answer correctness. Saved-query observations retain the caller's assertion about query, target, window and response. Confirm operational results through your authorized CrowdStrike client; see [coverage and limits](docs/COVERAGE.md).

## Feedback

Include the version, platform and a minimal synthetic reproduction when reporting a problem. Keep credentials, tenant identifiers, packs and raw telemetry out of public issues. Use the [tester feedback form](https://github.com/mychaelconnolly/cqlmatic/issues/new/choose).
