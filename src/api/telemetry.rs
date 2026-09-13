//! Request diagnostics with fixed fields, explicit opt-out and isolated test storage.
use super::AppState;
use axum::{
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::Response,
};
use serde_json::json;
use std::time::Instant;
pub(super) async fn capture(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let enabled = state.contract_store
        && std::env::var("COMMIT_TELEMETRY").is_ok_and(|v| v != "off" && v != "false" && v != "0");
    // Deployments default on; an explicit request opt-out always takes precedence.
    let enabled = enabled || (state.contract_store && std::env::var("COMMIT_TELEMETRY").is_err());
    let opted_out = request
        .headers()
        .get("x-commit-telemetry")
        .is_some_and(|v| v == "off");
    let selected = request.headers().contains_key("x-testing-environment-key")
        || request.headers().contains_key("x-testing-app-secret");
    let source = request
        .headers()
        .get("x-commit-client")
        .and_then(|v| v.to_str().ok())
        .filter(|s| matches!(*s, "cli" | "browser" | "daemon" | "rust-client"))
        .unwrap_or("api")
        .to_owned();
    let method = request.method().as_str().to_owned();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str)
        .to_owned();
    let start = Instant::now();
    let response = next.run(request).await;
    if enabled && !opted_out {
        let scope = crate::request_context::testing_scope();
        if !selected || scope.is_some() {
            let event = json!({"service":"silicon-commit","version":env!("CARGO_PKG_VERSION"),"source":source,"step":"http_response","progress":1,"event":"request_completed","context":{"method":method,"route":route,"status":response.status().as_u16(),"duration_ms":start.elapsed().as_millis(),"request_id":crate::request_context::current_request_id(),"testing":scope.is_some()}});
            // Diagnostics must never turn a completed product request into a failure.
            let _ = sqlx::query(
                "INSERT INTO commit.telemetry_events(environment_id,event) VALUES($1,$2)",
            )
            .bind(scope.map(|s| s.id))
            .bind(event)
            .execute(&state.pool)
            .await;
        }
    }
    response
}
