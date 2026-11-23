# CQLmatic v0.1.13

Setup, inventory and capture now work on managed workstations, in sandboxes and in folders you made yourself. Earlier versions refused a state location when another account had rights on it or when its path passed through a link. This version uses the location as it is and relies on your user profile's access rules.

## Storage locations

- **Existing directories are used as they are.** 0.1.12 stopped with `SETUP_PROTECTION` when the state directory, or any directory above it, gave rights to another account, and with `SETUP_LINK` when the path passed through a symbolic link, junction or other reparse point, as `/tmp` does on macOS. On Windows that refused a directory made in Explorer, any directory under `C:\`, and a profile on which a support or backup group has rights.
- **New directories:** still owner-only (`0700`, files `0600`) on macOS and Linux. On Windows a new directory now takes the access rules of the directory it is created in, where 0.1.12 limited it to the user and SYSTEM, and PowerShell is no longer started to create it.
- **Pack and input files:** `LOCAL_DIRECTORY_PERMISSIONS` is removed, so a pack or state directory writable by its group is accepted. The saved query and result files given to `import-query-observations` no longer need owner-only modes.
- **Unchanged:** state, pack and input files must be regular files, not links, and the credential file keeps its `0600` rule on macOS and Linux.
- **Clearer errors:** when a storage location cannot be created, read or locked for writing, the error names it. Where access was denied, it says to allow the location in the sandbox or device policy that blocks it, or to choose another.

The default locations rely on your user profile's access rules. Choose a `--state` location that only accounts you trust can read. See [setup](docs/SETUP.md), [the security policy](SECURITY.md) and [troubleshooting](docs/TROUBLESHOOTING.md).

## Upgrading

- State written by 0.1.12 and earlier is used where it is.
- A state directory that 0.1.13 creates on Windows is refused by 0.1.12 and earlier. To go back to an earlier release after 0.1.13 has created its state, give that release a new `--state` location.
- If your capture hooks name a version-specific executable path, re-run `cqlmatic env capture --client CLIENT --server SERVER --install` after moving to the new executable.
- CQLmatic 0.1.13 can inspect 0.1.12 (and earlier 0.1.0–0.1.11) discovery checkpoints in formats 2 and 3. Keep the 0.1.12 executable to resume or settle its unfinished runs. See [installation](docs/SETUP.md) for the package for your platform.
