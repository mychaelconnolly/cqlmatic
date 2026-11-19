# CQLmatic v0.1.10

Your assistant can hand in one discovery result and get its next search in the same step. A client hook can deliver matching results automatically. You can start a fresh run when a paused run has run out of time. Query checks flag more invalid combinations of correlation and joins.

## Discovery and capture

- Use `cqlmatic env inventory --json --next-probe` to reserve a ready search. To submit a saved result and ask for the next search together, add `--run-index N --probe-id ID --result-file FILE`. You can also send a `next-probe` JSON inventory request. A repeat call can show the outstanding reservation without using another search from the budget.
- A capture hook or pass-through can deliver an eligible completed result for the outstanding probe when the search tool and arguments match. After delivery, call `next-probe` without the answer. If it returns the same reserved probe, check for capture completion or submit the saved result. Failed searches and searches with other arguments still need handling by the caller. See [the discovery workflow](docs/INVENTORY.md#relay-one-probe-at-a-time).
- Use `cqlmatic env capture --client CLIENT --server SERVER --install` to register the hook in supported client settings. The installer preserves existing settings. On macOS and Linux it can update the file a settings link points to, leaving the link in place. Use `--dry-run` to preview or `--program TEXT` to specify a launcher. For clients such as Cline or for a read-only settings file, use the reported entry to install the hook by hand. See [capture setup](docs/INVENTORY.md#scrappy-mode-capture-the-agents-searches).
- When a paused run written by this release has used its active-time allowance, `prepare-discovery` can start a new run from the same request. A paused run with time remaining can be resumed with its existing allowance.

## Query checks and mapping guidance

- `check` reports `CQL_CORRELATION_JOIN_CONFLICT` when a query combines `correlate` with `selfJoin` or `selfJoinFilter`.
- Brief responses to discovery actions point to `next-probe` when probes are ready. If an entry in `source_ids` is unknown, use the `source_id` values of expected sources in `status --json --summary`. A missing required inventory field is named in the diagnostic. If there are no expected sources, confirm them before preparing discovery. The companion skill walks through mapping to `reconcile`, including parser review for Full mode.

## State locations and upgrades

- The default inventory location skips empty and relative values of `LOCALAPPDATA`, `XDG_DATA_HOME` and `HOME`. Setup does the same for the variables that locate its own state (`LOCALAPPDATA`, `XDG_STATE_HOME` and `HOME`). To reuse a file saved under the previous default, give its path with `--state FILE`. For `check`, `generate` and `assess`, use `--inventory-state FILE`. These commands can run without a default inventory location.
- CQLmatic 0.1.10 can inspect 0.1.9 discovery checkpoints in formats 2 and 3. Keep the producing executable to resume or settle its unfinished runs. See [installation](docs/SETUP.md) for the package for your platform.
