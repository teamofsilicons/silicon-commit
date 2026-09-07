# Silicon IAm integration

Commit authenticates through the configured IAm application (`COMMIT_IAM_APP_ID`, `COMMIT_IAM_APP_SECRET`, and `COMMIT_IAM_BASE_URL`). Store application secrets only in the deployment secret store or an ignored local `.env` file. IAM membership and capability responses are authoritative for organization access. Configure the Commit webhook callback in IAm using the deployment endpoint and the separately provisioned signing secret.
