//! Request diagnostics with fixed fields and an explicit opt-out.
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
        let event = json!({"service":"silicon-commit","version":env!("CARGO_PKG_VERSION"),"source":source,"step":"http_response","progress":1,"event":"request_completed","context":{"method":method,"route":route,"status":response.status().as_u16(),"duration_ms":start.elapsed().as_millis(),"request_id":crate::request_context::current_request_id()}});
        // Diagnostics must never turn a completed product request into a failure.
        let _ = sqlx::query("INSERT INTO commit.telemetry_events(event) VALUES($1)")
            .bind(event)
            .execute(&state.pool)
            .await;
    }
    response
}
