# CQLmatic v0.1.9

CQLmatic can now learn which fields your assistant sees in searches it already runs. You can install the companion skill from the command line and see how many searches discovery may need before starting. Query checks catch misplaced joins and point out steps that may make a search do extra work. Routine checks of private inventory files are faster on Windows.

## Capture and setup

- Scrappy mode records field evidence from qualifying completed searches against a confirmed target. Use `cqlmatic env capture --client CLIENT --server SERVER` with a client hook. For clients that cannot supply the result through a hook, `cqlmatic env pass-through -- COMMAND` runs the server over standard I/O, forwards messages unchanged and records eligible results after forwarding them.
- Captured observations appear separately in status and can support field grounding with `CQL_CAPTURED_OBSERVATION`. The reconciliation fingerprint excludes captured observations. Eligible searches can also queue a newly seen source for inventory. Each state has its own protected directory for saved copies and outcome counts. [Inventory guide](docs/INVENTORY.md#scrappy-mode-capture-the-agents-searches)
- `cqlmatic skill install --client CLIENT` installs the companion skill and its linked guides in the client's user-level skill directory. Use `--dir DIR` to choose a location. Verified setup surfaces the command for the selected client. [Setup guide](docs/SETUP.md)
- Windows now checks existing private files and directories through the Windows API. The same ownership, access and ancestor requirements apply. Creating a private directory still uses the PowerShell helper.

## Query checks

- `CQL_JOIN_PLACEMENT` rejects `join`, `selfJoin` and `selfJoinFilter` inside another function's arguments. Place them at the top level or in a join subquery. `selfJoin` combined with `defineTable` also reports `CQL_TABLE_JOIN_CONFLICT`.
- Lenient grounding emits one `CQL_GROUNDING_LENIENT` warning listing the affected fields. JSON still carries evidence per field in `grounding.basis`.
- `check` and `generate` can report six informational notes. They leave the outcome and `--fail-on-warning` behavior unchanged.

| Note | What it points out |
| --- | --- |
| `CQL_TAG_FILTER_ORDER` | Tag filters can narrow the input earlier. |
| `CQL_FILTER_AFTER_TRANSFORM` | A filter that uses untouched fields can run before the earlier transform. |
| `CQL_SORT_COST` | Sorting input that has not been narrowed can be costly. |
| `CQL_SPLIT_COST` | Splitting input that has not been narrowed can be costly. |
| `CQL_WILDCARD_IGNORE_CASE` | Case-insensitive wildcard matching can be costly. |
| `CQL_REGEX_ALL_FIELDS` | An unfielded regex can search every field. |

## Mapping and recovery

- Discovery reports searches used, ready and allowed, answer bytes where applicable, and expected search counts where known. Brief output retains these figures. The offline discovery plan also shows the expected range. Quick sources-only uses one search per source. Quick sources and fields plans for 13 to 30 per source with events, plus one per requested value field. Quiet sources can finish earlier. Full discovery gives a lower estimate only. The inventory guide includes a reference cost table. [Mapping cost](docs/INVENTORY.md#cost-recovery-and-freshness)
- A recognized hosted poll failure uses the existing `lifetime-expired` settlement for refresh or capability checks. Use the reply's arrival time as `search_started_at`, wait the retained search lifetime, and bind the saved answer's digest and trace ID. Capability settlement also requires the saved answer as `result_file`. Refresh imports no rows and the capability result stays `unknown`. [Settlement guidance](docs/MCP-COMPANION.md)
- Inventory can continue past a qualifying 0.1.6 or 0.1.7 discovery run once its original active-time allowance expires and its checkpoint proves the last attempt received a distinct final answer. Inspection keeps the original checkpoint. [Inventory recovery](docs/INVENTORY.md)

## Upgrading and compatibility

Upgrade with `npx cqlmatic@0.1.9` on supported macOS and Linux systems. On Windows, verify the release ZIP checksum and extract it to a new directory. Keep the producing executable for any unfinished discovery run. [Installation](docs/SETUP.md)

This release can inspect 0.1.8 checkpoints in formats 2 and 3. Lifecycle actions require the exact producing release, and 0.1.8 rejects discovery checkpoints written by 0.1.9. The executable profile and knowledge revision are unchanged. Inventory state stays at version 4. A state with captured observations contains a new `captured_observations` field and is rejected by 0.1.8. The field is omitted when unused. Keep a protected copy of your inventory and its files before upgrading if you may need to go back.

## For contributors

CI now checks tracked assets for embedded metadata before building. Run `python3 tools/asset_metadata.py` locally before committing an asset. [Development guide](docs/DEVELOPMENT.md)
