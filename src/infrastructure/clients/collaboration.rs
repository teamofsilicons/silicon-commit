//! Fixed-origin clients for Organizations app sessions and scoped Contacts proofs.
use super::{ClientBuildError, http_client, read_bounded};
use crate::error::AppError;
use reqwest::{Client, Method};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use url::Url;

#[derive(Clone)]
pub(crate) struct Collaboration {
    http: Client,
    contacts: Url,
    organizations: Url,
}

#[derive(Deserialize)]
pub(crate) struct OrganizationContext {
    pub account_uuid: String,
    pub groups: Vec<String>,
    pub share_targets: Vec<ShareTarget>,
    pub organizations: Vec<Value>,
}
#[derive(Deserialize, serde::Serialize)]
pub(crate) struct ShareTarget {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
    pub org_id: String,
}

fn origin(env_name: &str, default: &str) -> Result<Url, ClientBuildError> {
    let url = Url::parse(&std::env::var(env_name).unwrap_or_else(|_| default.to_owned()))
        .map_err(|_| ClientBuildError::InvalidEndpoint)?;
    if url.path() != "/"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https"
            || url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
    {
        return Err(ClientBuildError::InvalidEndpoint);
    }
    Ok(url)
}

impl Collaboration {
    pub fn new() -> Result<Self, ClientBuildError> {
        Ok(Self {
            http: http_client(Duration::from_secs(3), Duration::from_secs(12))?,
            contacts: origin("COMMIT_CONTACTS_URL", "https://contacts.teamofsilicons.com")?,
            organizations: origin(
                "COMMIT_ORGANIZATIONS_URL",
                "https://organizations.teamofsilicons.com",
            )?,
        })
    }
    pub async fn organizations(
        &self,
        app: &str,
        token: &str,
        account: &str,
    ) -> Result<OrganizationContext, AppError> {
        let mut url = self.organizations.clone();
        url.path_segments_mut()
            .map_err(|()| AppError::BadGateway)?
            .extend(["v1", "integrations", app, "me"]);
        let res = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| AppError::ProviderUnavailable)?;
        if !res.status().is_success() {
            return Err(AppError::ProviderUnavailable);
        }
        let bytes = read_bounded(res, 2 * 1024 * 1024)
            .await
            .map_err(|_| AppError::BadGateway)?;
        let ctx: OrganizationContext =
            serde_json::from_slice(&bytes).map_err(|_| AppError::BadGateway)?;
        if ctx.account_uuid != account {
            return Err(AppError::BadGateway);
        }
        Ok(ctx)
    }
    pub async fn contacts(
        &self,
        proof: &str,
        method: Method,
        path: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Vec<u8>, AppError> {
        let mut url = self.contacts.clone();
        url.path_segments_mut()
            .map_err(|()| AppError::BadGateway)?
            .push("v1")
            .extend(path.iter().copied());
        let mut req = self
            .http
            .request(method, url)
            .header("authorization", format!("Proof {proof}"))
            .query(query);
        if let Some(body) = body {
            req = req.json(body);
        }
        let res = req
            .send()
            .await
            .map_err(|_| AppError::ProviderUnavailable)?;
        let status = res.status();
        let bytes = read_bounded(res, 5 * 1024 * 1024)
            .await
            .map_err(|_| AppError::BadGateway)?;
        if status.is_success() {
            return Ok(bytes);
        }
        let msg = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "Silicon Contacts refused this request.".to_owned());
        Err(match status.as_u16() {
            401 | 403 => AppError::Denied {
                code: "contacts_access_denied".into(),
                message: msg,
            },
            404 => AppError::NotFound,
            409 => AppError::Conflict {
                code: "contact_conflict".into(),
            },
            400 | 422 => AppError::Validation {
                details: serde_json::json!({"contact":msg}),
            },
            429 => AppError::RateLimited {
                retry_after_seconds: 5,
            },
            _ => AppError::ProviderUnavailable,
        })
    }
    pub async fn contacts_json(
        &self,
        proof: &str,
        method: Method,
        path: &[&str],
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, AppError> {
        let bytes = self.contacts(proof, method, path, query, body).await?;
        serde_json::from_slice(&bytes).map_err(|_| AppError::BadGateway)
    }
}
