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

## Creating and assigning todos

Todo creation requires `title` and `assigned_to` strings. Use the recipient's public IAM ID from your team, including the organization suffix for a Silicon.

```sh
commit todos create --data '{"title":"Eat","assigned_to":"alex"}'
commit todos create --data '{"title":"Eat","assigned_to":"assistant:example-org"}'
```

`--data @file.json` accepts the same object. Optional fields are `description`, `status`, and `attachments`; run `commit todos create --help` for types and values. `assignee` and `assignee_id` are not supported. The CLI rejects these fields and missing or non-string required fields before making a request. Other validation remains on the server; a 422 error retains its request ID and points to the command's schema help. Errors go to stderr with a nonzero exit status.
