//! Silicon Commit one-shot privileged migration process.
//!
//! ```text
//! commit-migrate                                   apply the schema migrations
//! commit-migrate link-identities --file MAP.csv [--dry-run] [--offline]
//!                                                  re-point IAM-era rows to Silicon Accounts
//! commit-migrate link-identities --plan            print a mapping template (CSV) to fill in
//! ```

use std::collections::BTreeMap;

use anyhow::{Context as _, bail};
use secrecy::ExposeSecret as _;
use silicon_accounts_client::AccountsClient;
use silicon_commit::{
    config::{DEFAULT_ACCOUNTS_URL, DEFAULT_APP_ID, MigrationSettings},
    domain::ActorType,
    infrastructure::postgres::{self, identity_links},
    telemetry,
};

const USAGE: &str = "usage:
  commit-migrate
      Apply Commit's schema migrations (COMMIT_MIGRATOR_DATABASE_URL, COMMIT_SCHEMA_OWNER).
  commit-migrate link-identities --file MAPPING.csv [--dry-run] [--offline]
      Re-point IAM-era rows to Silicon Accounts accounts in one transaction and print a JSON report.
      MAPPING.csv has a header `iam_principal_id,accounts_uuid[,org_id]`; iam_principal_id is the
      IAM principal UUID or the IAM-era c:/si: id (an id matches production principals only;
      name a former testing-environment principal by its UUID); an empty accounts_uuid unlinks.
      --dry-run reports and rolls back. --offline skips the Silicon Accounts lookups that check
      each account's kind and current id (ACCOUNTS_URL, COMMIT_APP_ID, COMMIT_APP_SECRET).
  commit-migrate link-identities --plan [--offline]
      Print the production IAM-era principals as a mapping template, with the uuid of the
      account Silicon Accounts knows under the same id today when it is reachable.";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let settings = MigrationSettings::from_env()?;
    telemetry::init(&settings.log_filter)?;
    match arguments.first().map(String::as_str) {
        None => {
            let pool = postgres::connect_migrator(&settings.database, "commit-migrate").await?;
            postgres::migrate(&pool, &settings.schema_owner).await?;
            pool.close().await;
            Ok(())
        }
        Some("link-identities") => link_identities(&settings, &arguments[1..]).await,
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown command `{other}`\n\n{USAGE}"),
    }
}

async fn link_identities(settings: &MigrationSettings, arguments: &[String]) -> anyhow::Result<()> {
    let mut file = None;
    let mut dry_run = false;
    let mut offline = false;
    let mut plan = false;
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--file" => file = Some(iter.next().context("--file needs a path")?.clone()),
            "--dry-run" => dry_run = true,
            "--offline" => offline = true,
            "--plan" => plan = true,
            other => bail!("unknown option `{other}`\n\n{USAGE}"),
        }
    }
    let pool = postgres::connect_migrator(&settings.database, "commit-migrate").await?;
    let accounts = if offline { None } else { accounts_client()? };

    if plan {
        let rows = identity_links::plan(&pool).await?;
        let mut suggestions = BTreeMap::new();
        if let Some((client, app_id, secret)) = &accounts {
            let app = client.as_app(app_id.as_str(), secret.expose_secret());
            for id in identity_links::distinct_ids(&rows) {
                if let Ok(account) = app.lookup_by_id(&id).await {
                    suggestions.insert(id, account.uuid);
                }
            }
        }
        println!("iam_principal_id,accounts_uuid,org_id");
        for (principal, public_id, org_id, _kind, linked) in rows {
            let uuid = linked
                .or_else(|| suggestions.get(&public_id).cloned())
                .unwrap_or_default();
            println!("{principal},{uuid},{org_id}    # {public_id}");
        }
        pool.close().await;
        return Ok(());
    }

    let path = file.context("link-identities needs --file MAPPING.csv (or --plan)")?;
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read the mapping file {path}"))?;
    // Strip the trailing `# id` comments --plan writes.
    let text = text
        .lines()
        .map(|line| {
            line.split_once('#')
                .map_or(line, |(data, _)| data)
                .trim_end()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let rows = identity_links::parse_mapping(&text)?;
    let mut known = BTreeMap::new();
    if let Some((client, app_id, secret)) = &accounts {
        let app = client.as_app(app_id.as_str(), secret.expose_secret());
        for uuid in rows.iter().filter_map(|row| row.accounts_uuid.clone()) {
            let account = app.lookup(&uuid).await.with_context(|| {
                format!("Silicon Accounts has no account with the uuid {uuid} (or is unreachable; --offline skips this check)")
            })?;
            known.insert(
                uuid,
                identity_links::KnownAccount {
                    kind: match account.kind {
                        silicon_accounts_client::AccountKind::Carbon => ActorType::Carbon,
                        silicon_accounts_client::AccountKind::Silicon => ActorType::Silicon,
                    },
                    public_id: account.id.clone(),
                    custodian: account
                        .custodian
                        .as_ref()
                        .map(|custodian| custodian.uuid.clone()),
                },
            );
        }
    }
    let report = identity_links::apply(
        &pool,
        &rows,
        &identity_links::mapping_digest(&text),
        &known,
        dry_run,
    )
    .await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    pool.close().await;
    Ok(())
}

/// A Silicon Accounts app client when `COMMIT_APP_SECRET` is configured.
fn accounts_client() -> anyhow::Result<Option<(AccountsClient, String, secrecy::SecretString)>> {
    let Some(secret) = std::env::var("COMMIT_APP_SECRET")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let url = std::env::var("ACCOUNTS_API_URL")
        .or_else(|_| std::env::var("ACCOUNTS_URL"))
        .unwrap_or_else(|_| DEFAULT_ACCOUNTS_URL.to_owned());
    let app_id = std::env::var("COMMIT_APP_ID").unwrap_or_else(|_| DEFAULT_APP_ID.to_owned());
    let client = AccountsClient::builder()
        .base_url(url)
        .build()
        .context("ACCOUNTS_URL is not a usable Silicon Accounts URL")?;
    Ok(Some((client, app_id, secrecy::SecretString::from(secret))))
}
