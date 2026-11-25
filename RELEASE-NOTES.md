# CQLmatic v0.1.14

Mapping an environment takes fewer searches, and a source with no events in the window no longer stops the sources that have them. Setup can use an MCP server that is already configured in your client. Inventory status says what is still open, several inventory actions fit in one command, and parser reviews survive a new parser capture. Many refusals now say what stopped the action and what to do next.

## Discovery

- **Fewer searches for fields.** Field discovery counts tag candidates in batches of at most 20 columns per source, replacing one search per tag. With the standard candidates and the default 20-field population cap, a Quick sources + fields source needs at least 4 searches and at most 21; it took at least 13 and up to 30. A window of 15 minutes or less takes one population search where it took two.
- **Quiet sources.** A selected source with zero events in the discovery window costs only its volume search. The run measures and activates the sources that have events, leaves the quiet source's earlier records unchanged, and fails without activation only when every source is quiet. Status shows the zero and its window, and the source stays pending.
- **Volume first.** Discovery measures every selected source's volume before any other search, then lists every source with events before any source's tag, kind, population or value searches.
- **Refusals before anything is written.** `prepare-discovery` refuses a `max_queries` below the searches needed when every selected source has events and names that minimum. It also refuses a window that ends after the current time and a request with more than 100 sources. Search figures add `minimum_all_live` and `below_expected_high`, and Full discovery now has an upper figure.
- **Saved results.** Saved search responses with MCP metadata or text annotations, JSON-RPC response wrappers, bare text blocks and a UTF-8 byte order mark are accepted, and the original bytes are kept as evidence. A result with a trace or job identifier is refused when an earlier attempt in the run already retained the same result.
- **Failures explained.** The answer reply and later polls say why discovery failed and how to recover. Replies, polls and status report the search row ceiling.

## Setup

- **Use the server you already have.** `setup` accepts `registration: {"mode":"host-managed"}` for Codex, VS Code Local, Claude Code and the Copilot clients, not only for `external`. Setup then uses the MCP server already configured in that client, runs no client command and changes nothing in its configuration. 0.1.13 refused this with `SETUP_REGISTRATION_MODE`.
- **Client files with comments.** Setup reads client MCP files that contain comments or trailing commas, so VS Code Local no longer stops with `SETUP_NATIVE_CONFIG` when its server is already configured.
- **Clearer mismatches.** `SETUP_DISCOVERY_BINDING` names the report field that does not match the selection, or the tool count, without repeating the supplied value.
- **Capture hook offered.** A verified `setup` for Claude Code, Codex or a Copilot client names the command that installs the capture hook.
- **Equivalent inputs.** Uppercase Falcon CIDs and console checksum suffixes are accepted, and explicit RFC 3339 search-window offsets are converted to UTC.

## Inventory

- **Status says what is open.** Full and summary status add `completion_detail` with the counts of expected sources whose reviews or measurements are not done and a `next_action`, the first listed step. Status and `reconcile` advise next steps in one shared order. Text status shows `census saved`, `captured`, `evidence review current` and `run complete, pack activated` where it showed `DONE` or `complete`; JSON values are unchanged.
- **`reconcile` returns what it recorded:** `recorded` for a new evidence fingerprint and `already-current` when the saved one already matched.
- **Full map completion.** `full_map_complete` also requires a search-all census below the 10,000-row ceiling. A Full inventory whose search-all censuses all reached the ceiling is no longer reported complete, and status says why.
- **Several actions at once.** `env inventory` accepts a `batch` request of 1 to 32 `review-source`, `bind-parser`, `exclude-parser`, `parser-not-applicable`, `import-parsers` and `reconcile` actions under one writer lock. Either every item is saved or none is.
- **Parser reviews are kept.** `import-parsers` and `fetch-parsers` no longer delete parser bindings, their line reviews or parser exclusions. 0.1.13 deleted each one whose exact reviewed bytes were missing from the new capture. A review now stays active, carries over to the same parser saved as other bytes, or is suspended until the same definition is saved again.
- **Pack imports.** Imports can update matched records while preserving discovery scope, observation ordering and earlier snapshots. An import that would displace an unfinished discovery run reports how to finish or recover it and requires explicit acknowledgement. Destinations that resolve to an input snapshot, environment descriptor or legacy component path are refused.
- **Pruning.** Historical inventory snapshots can be previewed and pruned while active evidence, run indices and saved proofs are retained.
- **Clearer request errors.** Unknown action kinds and unexpected request fields are named, and a failed source selection lists the valid source IDs with their selectors.

## Capability checks

- `prepare-capabilities` refuses a `max_seconds` that does not exceed the host evidence's `max_search_seconds` and names the smallest value that can dispatch. 0.1.13 accepted such a run, and no later dispatch from it could pass.
- `dispatch-capability` says which condition stopped it and what to do next, where 0.1.13 gave one message for all of them.
- `submit-capability` records an attempt's terminal settlement after 21 unresolved reports. 0.1.13 refused that settlement, so the attempt stayed unresolved and blocked later inventory searches.

## Capture hook

- `env capture --install` updates recognized commands for the same client, server and state in place, keeps distinct custom matchers, removes repeated entries for the same matcher, and removes recognized commands with `--hooks none`.
- The hook still prints nothing and exits 0. It counts its own failures in `tally.json` when the state file exists.
- The hook no longer queues a new source for a search that names only the repository of a confirmed typed source.

## Checking and grounding

- Strict grounding uses the single field namespace at the query's exact repository and type scope when multiple namespaces match, and excludes positive pack evidence from event families that differ from the query's exact event-name literal.
- Unmapped literal source warnings are kept when checking or generating with strict grounding.
- A recent zero no longer hides presence that a whole-window count or a dataset or kind facet saw in the same run, so strict `check --require-observed` grounds those fields.
- Offline checking, generation and assessment stay available while the default inventory is scouting before target confirmation.
- Catalog knowledge and function contract lookups rank exact function names first.

## Upgrading

- Discovery checkpoints prepared by 0.1.13 and earlier stay readable for status and completed evidence but cannot be continued by 0.1.14. Prepare a new run, or keep the 0.1.13 executable to resume or settle its unfinished runs. CQLmatic 0.1.14 can inspect 0.1.13 (and earlier 0.1.0–0.1.12) discovery checkpoints in formats 2 and 3.
- Going back to 0.1.13 is not guaranteed. 0.1.13 and earlier cannot read a setup state that holds a `host-managed` selection for a named client, an inventory state that holds a suspended or carried parser review, or an inventory state while a scout plan prepared again is awaiting its reply.
- 0.1.13 refuses a `batch` request with exit 2 and changes nothing. Before sending a batch to an executable you have not checked, confirm that `cqlmatic --schema inventory` offers the `batch` kind.
- If your capture hooks name a version-specific executable path, re-run `cqlmatic env capture --client CLIENT --server SERVER --install` after moving to the new executable.

See [installation](docs/SETUP.md) for the package for your platform and the [changelog](CHANGELOG.md) for every change.
