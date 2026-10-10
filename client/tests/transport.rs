use serde_json::json;
use silicon_commit_client::{Client, Error, Mutation};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path, query_param},
};

#[tokio::test]
async fn conditional_writes_and_empty_deletes_match_the_api_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/notification-settings"))
        .and(header("if-match", "\"0\""))
        .and(header("authorization", "Bearer eyJ.access.token"))
        .and(header("x-commit-supported-versions", "2"))
        .and(header("x-commit-client", "rust-client"))
        .and(|r: &wiremock::Request| {
            r.headers.contains_key("idempotency-key") && !r.headers.contains_key("x-org-id")
        })
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-commit-api-version", "2")
                .set_body_json(json!({"version":1})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/todos/id"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri())?
        .with_bearer("eyJ.access.token")
        .with_mutation(Mutation::new().if_match(0)?);
    let body = json!({"webhook_url":null,"todo_list_subscription":null});
    assert_eq!(
        client.update_notification_settings(&body).await?["version"],
        1
    );
    assert!(client.delete_todo("id").await?.is_null());
    assert!(!format!("{client:?}").contains("eyJ.access.token"));
    Ok(())
}

#[tokio::test]
async fn resource_ids_are_single_segments_and_redirects_are_not_followed()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", target.uri()))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri())?.with_bearer("private");
    assert!(matches!(
        client.get_project("name/diary?admin=true").await,
        Err(Error::Api(_))
    ));
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests[0].url.path(),
        "/api/v1/projects/name%2Fdiary%3Fadmin=true"
    );
    assert!(
        target
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty()
    );
    assert!(client.get_todo("..").await.is_err());
    Ok(())
}

#[test]
fn api_urls_are_validated_and_plain_http_is_for_this_machine_only()
-> Result<(), Box<dyn std::error::Error>> {
    for url in [
        "file:///tmp/a",
        "https://user:secret@example.com/",
        "https://example.com/other",
        "https://example.com/?token=x",
        "http://example.com",
        "http://10.0.0.5:8080",
        "not a url",
    ] {
        let error = Client::new(url).err().ok_or("accepted")?;
        assert_eq!(error.code(), "invalid_url", "{url}");
    }
    let plain = Client::new("http://commit.example.com")
        .err()
        .ok_or("accepted")?;
    assert!(plain.to_string().contains("plain http"), "{plain}");
    for url in [
        "https://example.com",
        "https://example.com/api/v1",
        "https://example.com/api/v1/",
    ] {
        assert_eq!(
            Client::new(url)?.base_url().as_str(),
            "https://example.com/api/v1/"
        );
    }
    for url in [
        "http://127.0.0.1:4141",
        "http://localhost:4141/api/v1",
        "http://[::1]:4141",
    ] {
        assert!(Client::new(url).is_ok(), "{url}");
    }
    Ok(())
}

#[test]
fn mutation_keys_follow_the_api_limits() {
    assert!(Mutation::with_key("12345678").is_ok());
    assert!(Mutation::with_key("1234567").is_err());
    assert!(Mutation::with_key("has a space").is_err());
    assert!(Mutation::new().if_match(-1).is_err());
}

#[tokio::test]
async fn api_errors_keep_the_service_envelope_but_never_foreign_bodies()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/todos"))
        .respond_with(
            ResponseTemplate::new(422).insert_header("x-request-id", "req-422").set_body_json(json!({
                "error": {"code":"validation_failed","message":"The request contains invalid data.",
                          "request_id":"body-id","details":{"assigned_to":"unknown account"},"hint":"Name an existing account."}
            })),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/todos"))
        .respond_with(
            ResponseTemplate::new(502)
                .set_body_string("<html>proxy page with secret-looking text</html>"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/projects"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "7")
                .set_body_json(json!({"error":{"code":"rate_limited","message":"Slow down.","details":{"blob":"x".repeat(8000)}}})),
        )
        .mount(&server)
        .await;
    let client = Client::new(&server.uri())?.with_bearer("eyJ.secret.token");
    let error = client
        .create_todo(&json!({"title":"t","assigned_to":"c:nobody"}))
        .await
        .err()
        .ok_or("accepted")?;
    let api = error.as_api().ok_or("not an API error")?;
    assert_eq!((api.status, api.code.as_str()), (422, "validation_failed"));
    assert_eq!(api.request_id.as_deref(), Some("req-422"));
    assert_eq!(api.details, Some(json!({"assigned_to":"unknown account"})));
    assert_eq!(error.hint().as_deref(), Some("Name an existing account."));
    let text = error.to_string();
    for part in [
        "422",
        "validation_failed",
        "req-422",
        "unknown account",
        "Name an existing account.",
    ] {
        assert!(text.contains(part), "{text}");
    }
    assert!(!text.contains("eyJ.secret.token"));
    let proxy = client.list_todos(&[]).await.err().ok_or("accepted")?;
    assert_eq!(proxy.code(), "http_502");
    assert!(proxy.is_transient());
    assert!(!proxy.to_string().contains("proxy page"), "{proxy}");
    let limited = client.list_projects(&[]).await.err().ok_or("accepted")?;
    let limited = limited.as_api().ok_or("not an API error")?;
    assert_eq!(limited.retry_after_seconds, Some(7));
    assert!(limited.details.is_none(), "oversized details are dropped");
    Ok(())
}

#[tokio::test]
async fn the_client_negotiates_contract_two_and_refuses_others()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/project/tasks/task/claim"))
        .and(header("x-commit-supported-versions", "2"))
        .and(header("x-commit-telemetry", "off"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-commit-api-version", "2")
                .set_body_json(json!({"todo_id":"linked","assigned_to":{"type":"carbon","id":"c:alice","uuid":"zQo"}})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri())?
        .with_bearer("token")
        .with_telemetry(false);
    assert_eq!(
        client.claim_project_task("project", "task").await?["todo_id"],
        "linked"
    );
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-commit-api-version", "1")
                .set_body_json(json!({})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let error = client
        .get_project("project")
        .await
        .err()
        .ok_or("accepted")?;
    assert!(
        matches!(&error, Error::UnsupportedContract { served } if served == "1"),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
async fn proofs_use_the_proof_scheme_and_public_routes_send_no_credentials()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/todos"))
        .and(header("authorization", "Proof sap_test_proof"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items":[]})))
        .expect(1)
        .mount(&server)
        .await;
    for route in ["/api/v1/accounts", "/api/v1/version", "/healthz", "/readyz"] {
        Mock::given(method("GET"))
            .and(path(route))
            .and(|r: &wiremock::Request| !r.headers.contains_key("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"app_id":"commit"})))
            .expect(1)
            .mount(&server)
            .await;
    }
    let proof = Client::new(&server.uri())?.with_proof("sap_test_proof")?;
    assert_eq!(proof.list_todos(&[]).await?["items"], json!([]));
    assert!(!format!("{proof:?}").contains("sap_test_proof"));
    let signed_in = Client::new(&server.uri())?.with_bearer("eyJ.bearer");
    assert_eq!(signed_in.accounts().await?["app_id"], "commit");
    signed_in.version().await?;
    signed_in.health().await?;
    signed_in.ready().await?;
    for bad in ["sapr_refresh_token", "eyJ.not.a.proof", ""] {
        assert!(
            Client::new(&server.uri())?.with_proof(bad).is_err(),
            "{bad}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn account_routes_and_custodian_selectors_reach_their_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/notification-settings"))
        .and(query_param("silicon", "si:scout"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version":3})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/notification-settings"))
        .and(query_param("silicon", "si:scout"))
        .and(header("if-match", "\"3\""))
        .and(body_json(
            json!({"webhook_url":"https://hooks.example/commit","todo_list_subscription":null}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version":4})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/silicons/si:scout/allowed-accounts"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"allowed":[]})))
        .expect(1)
        .mount(&server)
        .await;
    for verb in ["PUT", "DELETE"] {
        Mock::given(method(verb))
            .and(path("/api/v1/silicons/si:scout/allowed-accounts/c:alice"))
            .and(|r: &wiremock::Request| r.headers.contains_key("idempotency-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"allowed":[]})))
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"uuid":"zQo","id":"c:ada"})))
        .expect(1)
        .mount(&server)
        .await;
    let client = Client::new(&server.uri())?.with_bearer("token");
    assert_eq!(
        client.notification_settings_of("si:scout").await?["version"],
        3
    );
    let body = json!({"webhook_url":"https://hooks.example/commit","todo_list_subscription":null});
    let update = client.clone().with_mutation(Mutation::new().if_match(3)?);
    assert_eq!(
        update
            .update_notification_settings_of("si:scout", &body)
            .await?["version"],
        4
    );
    client.silicon_allowlist("si:scout").await?;
    client.allow_account("si:scout", "c:alice").await?;
    client.disallow_account("si:scout", "c:alice").await?;
    assert_eq!(client.me().await?["uuid"], "zQo");
    Ok(())
}

#[tokio::test]
async fn errors_classify_refusals_and_temporary_failures() -> Result<(), Box<dyn std::error::Error>>
{
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/me"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error":{"code":"session_ended","message":"Signed out."}})),
        )
        .mount(&server)
        .await;
    let error = Client::new(&server.uri())?
        .with_bearer("old")
        .me()
        .await
        .err()
        .ok_or("accepted")?;
    assert!(error.is_unauthenticated() && !error.is_transient());
    assert_eq!(error.code(), "session_ended");
    let closed = Client::new("http://127.0.0.1:9")?
        .health()
        .await
        .err()
        .ok_or("accepted")?;
    assert!(closed.is_transient(), "{closed}");
    assert_eq!(closed.code(), "connection_failed");
    assert!(!closed.to_string().contains("healthz"), "{closed}");
    Ok(())
}
