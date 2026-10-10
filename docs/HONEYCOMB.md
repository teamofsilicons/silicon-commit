# Shared testing environments (retired)

Migration note: Commit no longer takes part in shared testing environments. The participant endpoints
(`/internal/honeycomb/…`), the service token, the activity reports and the sandbox encryption key were removed with
the move to Silicon Accounts. Data of former sandboxes stays in the database, unreachable by the runtime roles. See
[test environments](TEST_ENVIRONMENTS.md).
