# Test environments

A test environment is an organization-owned isolated Commit instance linked to an IAM test environment. It is addressed with a generated 32-character key and uses the same application routes as production. Test requests carry `x-testing-environment-key` (or CLI `--test`). Each environment is capped at 10 projects and 100 todos. Rotation invalidates the previous Commit key; deletion enters a 30-day recovery window, and 15 days without activity triggers automatic deletion. Cleanup removes the isolated organization's rows transactionally.

Import Commit's canonical application ID (for example, `tos>commit`) into the IAM testing environment. IAM returns a fresh test application secret. Creating a Commit testing environment requires `name`, `iam_test_key`, `iam_app_id`, and `iam_app_secret`; optional `description` is unchanged. Supply the JSON using `commit test-environments create --data @private-create.json`, authenticated in the production organization, without `--test`. Commit checks the credentials against its configured IAM origin using an inactive-token introspection request, which issues no user token.

Commit stores both the IAM root key and imported app secret encrypted with the existing application-derived encryption key. The complete pair is selected per request for session operations, protected-request introspection, and OBO calls. Directory reads keep the authenticated user's bearer and the same IAM test context. Secrets are neither returned in environment metadata nor logged, and a testing request never substitutes the production app credential.

After migration 0022, an existing environment has no imported app credential until explicitly paired. Its test requests return HTTP 409 `testing_environment_iam_credentials_required`. Retain the existing environment and IAM root key, then use this production control-plane API with its imported test application secret:

```http
PUT /api/v1/test-environments/<existing-environment-id>/iam-credentials
Authorization: Bearer <production-owner-or-manager-token>
X-Org-Id: <owning-organization>
Content-Type: application/json

{"iam_app_id":"tos>commit","iam_app_secret":"<imported-testing-app-secret>"}
```

The caller must be the production organization's owner or hold the explicit `commit.test_environments.manage` capability. The endpoint rejects a testing-environment header, an environment owned by another organization, a different application ID, and credentials rejected by IAM. Keep credential JSON files private. The response contains only environment metadata. The same API rotates a test app secret without changing the linked IAM root key or Commit key; its version increment rejects subsequent writes from an earlier resolved test scope. A concurrent lifecycle change requires retrying against the current environment state.
