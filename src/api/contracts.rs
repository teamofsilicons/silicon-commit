//! API negotiation and local compatibility lifecycle governance (no telemetry).
use super::AppState;
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse as _, Response},
};
use serde_json::json;
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
                text == "1"
            } else {
                text.split(',').any(|v| v.trim() == "1")
            };
            if !supported {
                return Err("This endpoint supports contract 1. Read /api/v1/contracts.");
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
        return (StatusCode::NOT_ACCEPTABLE,Json(json!({"error":{"code":"unsupported_contract","message":message},"supported_versions":[1]}))).into_response();
    }
    let testing = request.headers().contains_key("x-testing-environment-key")
        || request.headers().contains_key("x-testing-app-secret");
    let mut lifecycle = "active".to_owned();
    if state.contract_store && !request.uri().path().ends_with("/contracts") {
        match sqlx::query_scalar::<_,String>("SELECT commit.admit_contract(1,$1)").bind(testing).fetch_one(&state.pool).await {
            Ok(value) if value=="active"||value=="deprecated"=>lifecycle=value,
            Ok(_)=>return (StatusCode::GONE,Json(json!({"error":{"code":"contract_sunset","message":"This contract has retired. Read /api/v1/contracts for a supported version."}}))).into_response(),
            Err(error)=>return crate::error::AppError::from(error).into_response(),
        }
    }
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("x-commit-api-version", HeaderValue::from_static("1"));
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
        vec![json!({"version":1,"status":"active"})]
    };
    Ok(Json(
        json!({"service":"silicon-commit","service_version":env!("CARGO_PKG_VERSION"),"contracts":versions,"compatibility":[{"contract":1,"clients":["0.1.x (legacy operations)","0.2.x (collaboration and automatic sandbox selection)"],"cli":"0.2.x"}],"policy":{"breaking_changes":"new version; retain existing consumers","additive_changes":"optional fields and new endpoints","sunset":"deprecated versions retire after seven days without production requests; active versions never auto-retire","testing":"testing requests do not change production usage counters"},"docs":"https://docs.commit.teamofsilicons.com/contracts/"}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn negotiation_accepts_legacy_default_and_common_version_and_rejects_duplicates() {
        let mut h = HeaderMap::new();
        assert!(select(&h).is_ok());
        h.insert(
            "x-commit-supported-versions",
            HeaderValue::from_static("2, 1"),
        );
        assert!(select(&h).is_ok());
        h.insert("x-commit-api-version", HeaderValue::from_static("2"));
        assert!(select(&h).is_err());
        h.insert("x-commit-api-version", HeaderValue::from_static("1"));
        h.append("x-commit-api-version", HeaderValue::from_static("1"));
        assert!(select(&h).is_err());
    }
}
