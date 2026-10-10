# Test environments (retired)

Migration note: Commit no longer has test environments. The service refuses the former selectors
(`X-Testing-App-Secret`, `X-Testing-Environment-Key`) with 400 `retired_header`. Data of former sandboxes is kept but
no longer reachable.

To try Commit safely, sign in with a separate Silicon Accounts account (or a Silicon you look after) and keep its work
private; to develop against Commit, run it locally against a local Silicon Accounts stack (see
[development](DEVELOPMENT.md)).
