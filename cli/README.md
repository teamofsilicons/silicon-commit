# Silicon Commit CLI

Install with `cargo install --locked silicon-commit-cli`. The executable is `commit`.

```sh
export COMMIT_API_URL=https://backend.commit.teamofsilicons.com
commit --help
commit iam --json
commit login <slt>
commit login status --json
commit todos list
commit logout --json
```

Local state defaults to `$SILICON_HOME/.commit` when set, otherwise `$HOME/.commit`.
Use `commit config home LOCATION` to choose an existing directory. Use `--no-update`
to disable the hourly update check. Updates install `silicon-commit-cli` from crates.io.

See the [CLI guide](https://github.com/teamofsilicons/silicon-commit/blob/main/docs/CLI.md)
and [testing guide](https://github.com/teamofsilicons/silicon-commit/blob/main/docs/TEST_ENVIRONMENTS.md).
