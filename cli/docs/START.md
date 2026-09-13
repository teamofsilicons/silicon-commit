# Start using Silicon Commit

Install the CLI and its hourly updater on macOS or Linux:

```sh
curl -fsSL https://docs.commit.teamofsilicons.com/install.sh | sh
```

The installer sets up Rust if needed, installs Commit, and registers the user updater. It does not sign you in. State lives under `$SILICON_HOME/.commit`, or `$HOME/.commit` if SILICON_HOME is unset.

```sh
commit iam --json
# Generate an application SLT using the official IAM CLI or IAM consent screen.
commit login '<slt>'
commit login status --json
commit --org-id your-org todos list
```

Create a todo with its assignee's public ID:

```sh
commit --org-id your-org todos create --data '{"title":"Review release","assigned_to":"alice"}'
commit --org-id your-org projects create --data '{"name":"Release","description":"Ship the next version"}'
```

Use `commit <command> --help` to explore the command tree. Use `commit docs projects`, `commit docs testing`, or `commit docs development` for complete offline guides. Keep the idempotency key when retrying a write; use a new key for a new action.

## Choose your next step

- [Work on projects](PROJECTS.md)
- [Use a sandbox](TEST_ENVIRONMENTS.md)
- [Configure email and webhooks](NOTIFICATIONS.md)
- [Learn the CLI](CLI.md)
- [Build an integration](DEVELOPMENT.md)
- [Rust client](CLIENT.md) and [HTTP API](API.md)
- [Compatibility policy](CONTRACTS.md) and [telemetry settings](TELEMETRY.md)
