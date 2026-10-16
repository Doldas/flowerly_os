# Troubleshooting

## Start with the standard checks

```sh
just check
just test
just lint
```

If a service fails during local development, inspect its terminal output and structured logs first.

## Runtime does not start

Run a clean build:

```sh
cargo build --workspace
```

Then retry:

```sh
just serve
```

Check that no stale development runtime or Unix socket is left from an earlier interrupted run.

## `just status` cannot reach a service

Verify that `just serve` is still running. Flowerly OS services use local IPC and should not require internet access for basic status operations.

## SQLite or migration errors

Do not edit the database schema manually. The repository uses ordered migrations. Preserve the failing database if the error may indicate a migration bug and include the relevant logs in a bug report.

## Documentation build fails

Install the pinned documentation dependencies:

```sh
python -m pip install -r requirements-docs.txt
mkdocs build --strict
```

## Reporting bugs

Open a GitHub issue with:

- revision/commit
- host distribution
- Rust version
- command executed
- expected behavior
- actual behavior
- relevant logs

Security-sensitive reports should follow [SECURITY.md](https://github.com/Doldas/flowerly_os/blob/master/SECURITY.md).
