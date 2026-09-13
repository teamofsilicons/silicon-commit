# API compatibility and version policy

Read `GET /api/v1/contracts` for the live compatibility matrix. The Rust package and CLI 0.2 use contract 1. Existing 0.1 consumers keep their original operations; optional fields and endpoints extend version 1.

| Consumer | Contract | Behavior |
| --- | --- | --- |
| Client / CLI 0.1.x | 1 | Existing todos, projects and legacy sandbox operations |
| Client / CLI 0.2.x | 1 | Collaborative projects, history, email and automatic sandbox selection |
| HTTP integrations | 1 | Select explicitly or accept the default |

Send `X-Commit-API-Version: 1` or `X-Commit-Supported-Versions: 2,1`. The backend selects version 1 and returns `X-Commit-API-Version: 1`. Omission retains legacy default behavior; unsupported or duplicate negotiation headers return 406. The client refuses an explicitly incompatible response version.

Breaking schema or semantics changes require a new API version and parallel handlers. Within a version, additions are optional and clients should tolerate new response fields. Contract consumer tests cover HTTP method/path, credentials and testing headers, retries, error mapping, and negotiated versions. OpenAPI is checked in CI alongside client and CLI transport tests.

An operator explicitly marks a replaced version deprecated in `commit.contract_versions`, setting `deprecated_at`. Active versions never auto-retire. Deprecated versions receive a `Deprecation` header and documentation link; admission records the last production request. After seven continuous days without production traffic, the worker or next admission retires that version. Testing traffic never changes production counters or postpones sunset. Retired contracts return 410; `/contracts` remains available to discover migration options. Publish and validate the replacement before deprecating a version.
