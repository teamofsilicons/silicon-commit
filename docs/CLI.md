# Commit CLI

Build with `cargo build -p commit` and run `commit --help`. Set `COMMIT_API_URL` and `COMMIT_ACCESS_TOKEN`, or pass `--api-url` and `--token`. Commands currently include `health`, `version`, `todos`, and `projects`; output is JSON for scripting. Prefix requests with `--test <test-key>` to target an isolated test environment.
