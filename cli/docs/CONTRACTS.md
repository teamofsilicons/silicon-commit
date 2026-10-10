# API compatibility and version policy

Read `GET /api/v1/contracts` for the live compatibility matrix. Contract 2 is the Silicon Accounts contract: no organizations or tags, every account reference is `{type, id, uuid}`, and Silicon webhooks use payload version 3. Contract 1 (organisations, Silicon IAM sign-in) has ended: it is deprecated and its requests answer 406, because the credentials it relied on no longer exist.

| Consumer | Contract | Behavior |
| --- | --- | --- |
| Client / CLI 0.1.x–0.2.x | 1 | No longer served (406); upgrade |
| Accounts-era client / CLI | 2 | Silicon Accounts sign-in, accounts and circles |
| HTTP integrations | 2 | Select explicitly or accept the default |

Send `X-Commit-API-Version: 2` or `X-Commit-Supported-Versions: 2`. The backend serves version 2 and returns `X-Commit-API-Version: 2`. Omitting the headers selects version 2; a request that asks only for version 1, or repeats a negotiation header, returns 406. The client refuses an explicitly incompatible response version.

Breaking schema or semantics changes require a new API version and parallel handlers. Within a version, additions are optional and clients should tolerate new response fields. Contract consumer tests cover HTTP method/path, credentials, retries, error mapping, and negotiated versions. OpenAPI is checked in CI alongside client and CLI transport tests.

An operator explicitly marks a replaced version deprecated in `commit.contract_versions`, setting `deprecated_at`. Active versions never auto-retire. Deprecated versions receive a `Deprecation` header and documentation link; admission records the last production request. After seven continuous days without production traffic, the worker or next admission retires that version. Retired contracts return 410; `/contracts` remains available to discover migration options. Publish and validate the replacement before deprecating a version.
