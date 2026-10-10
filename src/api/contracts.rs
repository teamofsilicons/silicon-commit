//! API negotiation and local compatibility lifecycle governance (no telemetry).
//!
//! Contract 2 is the Silicon Accounts contract: no organizations or tags,
//! every account reference carries its permanent `uuid`, and Silicon webhooks
//! use payload version 3. Contract 1 authenticated with Silicon IAM; it is
//! deprecated and sunsets after seven idle days like any replaced contract.
use super::AppState;
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use serde_json::json;

/// The contract this build serves.
pub(crate) const CURRENT: u16 = 2;

fn select(headers: &HeaderMap) -> Result<(), &'static str> {
    for name in ["x-commit-api-version", "x-commit-supported-versions"] {
        let mut values = headers.get_all(name).iter();
        let first = values.next();
        if values.next().is_some() {
            return Err("Provide each version header once.");
        }
        if let Some(raw) = first {
            let text = raw.to_str().map_err(|_| "Invalid version header.")?;
            let supported = if name == "x-commit-api-version" {
                text.trim() == "2"
            } else {
                text.split(',').any(|v| v.trim() == "2")
            };
            if !supported {
                return Err(
                    "This API serves contract 2 (Silicon Accounts sign-in, no organizations). Contract 1 (Silicon IAM) has ended; update the client and read /api/v1/contracts.",
                );
            }
        }
    }
    Ok(())
}

pub(super) async fn negotiate(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if let Err(message) = select(request.headers()) {
        return (StatusCode::NOT_ACCEPTABLE,Json(json!({"error":{"code":"unsupported_contract","message":message},"supported_versions":[CURRENT]}))).into_response();
    }
    let mut lifecycle = "active".to_owned();
    if state.contract_store && !request.uri().path().ends_with("/contracts") {
        match sqlx::query_scalar::<_,String>("SELECT commit.admit_contract($1,false)").bind(i32::from(CURRENT)).fetch_one(&state.pool).await {
            Ok(value) if value=="active"||value=="deprecated"=>lifecycle=value,
            Ok(_)=>return (StatusCode::GONE,Json(json!({"error":{"code":"contract_sunset","message":"This contract has retired. Read /api/v1/contracts for a supported version."}}))).into_response(),
            Err(error)=>return crate::error::AppError::from(error).into_response(),
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-commit-api-version", HeaderValue::from(CURRENT));
    if lifecycle == "deprecated" {
        response
            .headers_mut()
            .insert("deprecation", HeaderValue::from_static("true"));
        response.headers_mut().append(
            "link",
            HeaderValue::from_static(
                "<https://docs.commit.teamofsilicons.com/contracts/>; rel=\"deprecation\"",
            ),
        );
    }
    response
}
pub(super) async fn describe(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, crate::error::AppError> {
    let versions = if state.contract_store {
        sqlx::query("SELECT commit.sunset_idle_contracts()")
            .execute(&state.pool)
            .await?;
        sqlx::query_scalar::<_,serde_json::Value>("SELECT jsonb_build_object('version',version,'status',status,'deprecated_at',deprecated_at,'sunset_at',sunset_at) FROM commit.contract_versions ORDER BY version").fetch_all(&state.pool).await?
    } else {
        vec![
            json!({"version":1,"status":"deprecated"}),
            json!({"version":2,"status":"active"}),
        ]
    };
    Ok(Json(
        json!({"service":"silicon-commit","service_version":env!("CARGO_PKG_VERSION"),"current_contract":CURRENT,"contracts":versions,"compatibility":[{"contract":1,"clients":["0.1.x to 0.4.x (Silicon IAM sign-in, organizations)"],"cli":"0.4.x and older","status":"ended: these clients cannot sign in any more"},{"contract":2,"clients":["0.5.x (Silicon Accounts sign-in, no organizations)"],"cli":"0.5.x"}],"policy":{"breaking_changes":"new version; retain existing consumers","additive_changes":"optional fields and new endpoints","sunset":"deprecated versions retire after seven days without production requests; active versions never auto-retire"},"docs":"https://docs.commit.teamofsilicons.com/contracts/"}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negotiation_serves_contract_two_and_refuses_contract_one() {
        let mut h = HeaderMap::new();
        assert!(select(&h).is_ok());
        h.insert(
            "x-commit-supported-versions",
            HeaderValue::from_static("2, 1"),
        );
        assert!(select(&h).is_ok());
        h.insert("x-commit-api-version", HeaderValue::from_static("1"));
        assert!(select(&h).is_err());
        h.insert("x-commit-api-version", HeaderValue::from_static("2"));
        assert!(select(&h).is_ok());
        h.append("x-commit-api-version", HeaderValue::from_static("2"));
        assert!(select(&h).is_err());
        let mut only_old = HeaderMap::new();
        only_old.insert("x-commit-supported-versions", HeaderValue::from_static("1"));
        assert!(select(&only_old).is_err());
    }
}
