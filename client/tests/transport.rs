use serde_json::json;
use silicon_commit_client::{Client, Error, Mutation};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

#[tokio::test]
async fn login_uses_versioned_route_keeps_test_context_and_stable_retry_key()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/v1/auth/login"))
        .and(header("x-testing-environment-key","abcdefghijklmnopqrstuvwxyz123456"))
        .and(header("idempotency-key","reusable-login-key-001"))
        .and(body_json(json!({"slt":"slt_one_time"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"access_token":"oat_secret","refresh_token":"ort_secret","token_type":"Bearer","expires_in":3600,"scope":"profile.read","actor":{},"org_id":"tos"}))).expect(2).mount(&server).await;
    let client = Client::new(&server.uri())?
        .with_test_key("abcdefghijklmnopqrstuvwxyz123456")?
        .with_mutation(Mutation::with_key("reusable-login-key-001")?);
    for _ in 0..2 {
        let session = client.login_with_slt("slt_one_time").await?;
        assert_eq!(session.access_token, "oat_secret");
        assert!(!format!("{session:?}").contains("oat_secret"));
    }
    Ok(())
}

#[tokio::test]
async fn conditional_writes_and_empty_deletes_match_the_backend_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/notification-settings"))
        .and(header("if-match", "\"0\""))
        .and(header("x-org-id", "tos"))
        .and(header("authorization", "Bearer oat_private"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version":1})))
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
        .with_org_id("tos")
        .with_bearer("oat_private")
        .with_mutation(Mutation::new().if_match(0)?);
    assert_eq!(
        client
            .update_notification_settings(
                &json!({"webhook_url":null,"todo_list_subscription":null})
            )
            .await?["version"],
        1
    );
    assert!(client.delete_todo("id").await?.is_null());
    assert!(!format!("{client:?}").contains("oat_private"));
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
        Err(Error::Api { .. })
    ));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        requests[0].url.path(),
        "/api/v1/projects/name%2Fdiary%3Fadmin=true"
    );
    assert!(target.received_requests().await.unwrap().is_empty());
    Ok(())
}

#[tokio::test]
async fn malformed_test_keys_and_base_urls_cannot_reach_a_production_route()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    let client = Client::new(&server.uri())?.with_test_key("bad-key");
    assert!(matches!(client, Err(Error::Invalid(_))));
    assert!(server.received_requests().await.unwrap().is_empty());
    for url in [
        "file:///tmp/a",
        "https://user:secret@example.com/",
        "https://example.com/other",
        "https://example.com/?token=x",
    ] {
        assert!(Client::new(url).is_err());
    }
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
    Ok(())
}

#[test]
fn mutation_accepts_backend_minimum_idempotency_key_length() {
    assert!(Mutation::with_key("12345678").is_ok());
    assert!(Mutation::with_key("1234567").is_err());
}

#[tokio::test]
async fn login_status_distinguishes_rejected_credentials_from_service_failures()
-> Result<(), Box<dyn std::error::Error>> {
    for status in [401, 403, 503] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/auth/status"))
            .respond_with(
                ResponseTemplate::new(status).set_body_json(json!({"error":{"code":"test_error"}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let result = Client::new(&server.uri())?
            .with_bearer("invalid")
            .login_status()
            .await;
        if status == 401 {
            assert_eq!(result?["authenticated"], false);
        } else {
            assert!(result.is_err());
        }
    }
    Ok(())
}

#[tokio::test]
async fn collaborative_consumer_negotiates_and_preserves_secret_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    let secret = format!("ask_{}", "a".repeat(43));
    Mock::given(method("POST"))
        .and(path("/api/v1/projects/project/tasks/task/claim"))
        .and(header("x-commit-supported-versions", "1"))
        .and(header("x-testing-environment-key", secret.as_str()))
        .and(header("x-commit-telemetry", "off"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-commit-api-version", "1")
                .set_body_json(
                    json!({"todo_id":"linked","assigned_to":{"type":"carbon","id":"alice"}}),
                ),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = Client::new(&server.uri())?
        .with_test_app_secret(secret)?
        .with_telemetry(false);
    assert_eq!(
        c.claim_project_task("project", "task").await?["todo_id"],
        "linked"
    );
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-commit-api-version", "2")
                .set_body_json(json!({})),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(matches!(
        c.get_project("project").await,
        Err(Error::Invalid(_))
    ));
    Ok(())
}
