# IAM 5 session contexts

Commit's ordinary session broker requires one Carbon or Silicon and one
organization. Login and refresh reject missing organization/actor context and
OBO scopes. Testing actor login accepts `org_id`; an issued code must agree with
any explicitly selected organization. Conflicting body/header organization
selectors are rejected.

Status and organization discovery inspect the token's singular authorization
snapshot. Legacy unscoped or multiple-organization snapshots are rejected.
The audience, expiration, actor, membership, selected testing world, and
organization are checked before exposing the ordinary session context.

Validation: nine focused session tests, including malformed/legacy contexts,
identity mismatch, ordinary/OBO separation, and Carbon/Silicon status. Strict
backend Clippy passed. Live IAM 5 validation remains a release gate.
