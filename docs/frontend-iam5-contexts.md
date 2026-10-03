# Commit browser IAM 5 contexts

Every login response must contain one Carbon or Silicon actor and one organization. The browser gateway rejects missing organization identity and any refresh that changes actor kind, public ID, or organization. Organization discovery can describe only this selected organization. An `X-Org-Id` override cannot turn an existing bearer into another workspace.

Each login family has an opaque context ID and its own encrypted, HttpOnly credential cookie, bound cryptographically to the frontend origin, backend origin, and production/testing environment. A separate cookie selects the current context in that environment. Refresh, expiry and sign-out responses modify only the credential cookie for the original context; delayed responses cannot overwrite a newer selection. Separate application secrets remain attached to the matching testing contexts. This uses the existing stateless gateway deployment model and works with the existing multiple-Set-Cookie HTTP adapter.

The account and organization selector uses `POST /auth/context` with a saved context ID. Every ordinary gateway operation carries `X-Commit-Context`; stale tabs receive `409 session_context_changed` before an upstream action. The unauthenticated screen can reopen other saved contexts after signing out. Signing out revokes only the selected family. Existing browser sessions without a context ID require a fresh IAM login; the gateway does not infer one organization from an old bearer.

Frontend resource keys include the context ID. Todo, project, note, diary and notification forms capture that context when they mount, so asynchronous follow-up work cannot retarget another account. Permission and terms-review retries retain their mutation key in the original context. Provider authorization errors do not automatically sign the user out.

Existing Commit feature routes are preserved. This change does not invent provider approval endpoints; any new OBO authorization UI requires a matching provider-owned backend contract.

Run `npm test`, `npm run check`, and `npm run build` from `frontend`. Tests cover saved Carbon/Silicon profiles, same-organization accounts, organization and testing isolation, late renewal/expiry races, immutable refresh retries, stale requests, legacy cookies, and actual bundled browser API behavior. Live IAM 5 acceptance remains required before release. Saved contexts use browser cookies and remain subject to browser cookie storage limits; sign out of unused contexts to remove their saved credentials.
