mod support;
use httpmock::{
    Method::{GET, POST},
    MockServer,
};
use serde_json::json;
use support::*;

#[test]
fn unsafe_429_is_sent_once_and_exits_7() {
    let server = MockServer::start();
    let m = server.mock(|w, t| {
        w.method(POST).path("/cloud/v2/universes/1/places/2/luau-execution-session-tasks");
        t.status(429).body("slow");
    });
    let out = command(&server.base_url())
        .args([
            "call",
            "cloud.create-universe-place-luau-execution-session-task",
            "universe_id=1",
            "place_id=2",
            "--body",
            "{}",
            "--no-wait",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 7, "429 means not processed");
    m.assert_calls(1);
    assert_eq!(document(&out)["error"]["kind"], "unavailable");
}
#[test]
fn refused_unsafe_connection_is_attempted_once_and_exits_7() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let out = command(&format!("http://127.0.0.1:{port}"))
        .args([
            "--verbose",
            "call",
            "cloud.create-universe-place-luau-execution-session-task",
            "universe_id=1",
            "place_id=2",
            "--body",
            "{}",
            "--no-wait",
        ])
        .output()
        .unwrap();
    assert_eq!(code(&out), 7, "a refused connection never left");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).matches("POST /cloud").count(),
        1,
        "unsafe call has one transport attempt"
    );
    assert_eq!(document(&out)["error"]["kind"], "unavailable");
}
#[test]
fn absent_scope_metadata_is_null_on_401_and_403() {
    for status in [401, 403] {
        let server = MockServer::start();
        let m = server.mock(|w, t| {
            w.method(GET).path("/cloud/v2/universes/1");
            t.status(status);
        });
        let out = command(&server.base_url()).args(["call", "cloud.get-universe", "universe_id=1"]).output().unwrap();
        assert_eq!(code(&out), 3);
        let d = document(&out);
        assert!(d["error"]["requiredScopes"].is_null());
        assert_eq!(d["error"]["scopeMetadata"], "unknown");
        assert_eq!(d["error"]["keySource"], "UPDRAFT_API_KEY");
        m.assert_calls(1);
    }
}
#[test]
fn echoed_key_and_encoded_forms_never_reach_either_stream() {
    for status in [200, 403, 503] {
        let server = MockServer::start();
        let encoded = percent_encoding::utf8_percent_encode(KEY, percent_encoding::NON_ALPHANUMERIC).to_string();
        let body = json!({"key":KEY,"encoded":encoded,"url":"https://cdn.test/file?signature=private"});
        server.mock(|w, t| {
            w.method(GET).path("/assets/v1/assets/42");
            t.status(status).json_body(body);
        });
        let out = command(&server.base_url()).args(["call", "assets.get-asset", "assetId=42"]).output().unwrap();
        let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!combined.contains(KEY), "exact key is redacted for HTTP {status}");
        assert!(!combined.contains(&encoded), "encoded key is redacted for HTTP {status}");
        assert!(!combined.contains("signature=private"), "signed query is stripped");
        document(&out);
    }
}
#[test]
fn named_calls_and_url_calls_bind_folded_parameters() {
    let server = MockServer::start();
    let m = server.mock(|w, t| {
        w.method(GET).path("/assets/v1/assets/42").query_param("readMask", "displayName").header("x-api-key", KEY);
        t.json_body(json!({"displayName":"Sofa"}));
    });
    for args in [
        vec!["call", "assets.get-asset", "asset-id=42", "read_mask=displayName"],
        vec!["call", "GET", "/assets/v1/assets/42?readMask=displayName"],
    ] {
        let out = command(&server.base_url()).args(args).output().unwrap();
        assert_eq!(code(&out), 0);
        assert_eq!(document(&out)["displayName"], "Sofa");
    }
    m.assert_calls(2);
}
#[test]
fn invalid_calls_fail_before_sending() {
    let server = MockServer::start();
    let m = server.mock(|_, t| {
        t.status(500);
    });
    for args in [
        vec!["call", "assets.get-asset"],
        vec!["call", "assets.get-asset", "assetId=42", "invented=5"],
        vec!["call", "universes.create-place-version", "universeId=nope", "placeId=2", "--body", "x"],
        vec!["call", "assets.get-asset", "assetId=42", "--body", "{}"],
        vec![
            "call",
            "cloud.create-universe-place-luau-execution-session-task",
            "universe_id=1",
            "place_id=2",
            "--body",
            "{",
        ],
        vec!["call", "assets.get-asset", "assetId=42", "--all"],
    ] {
        let out = command(&server.base_url()).args(args).output().unwrap();
        assert_eq!(code(&out), 2, "invalid call is a usage error");
        document(&out);
    }
    m.assert_calls(0);
}
#[test]
fn public_operation_sends_no_key_and_needs_no_config() {
    let server = MockServer::start();
    let m = server.mock(|w, t| {
        w.method(GET).path("/universes/v1/places/1818/universe").header_missing("x-api-key");
        t.json_body(json!({"universeId":13058}));
    });
    let out = command(&server.base_url())
        .env_remove("UPDRAFT_API_KEY")
        .args(["call", "universes.get-place-universe", "placeId=1818"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(document(&out)["universeId"], 13058);
    m.assert_calls(1);
}
#[test]
fn auth_check_posts_key_and_flattens_measured_scope_shape() {
    let server = MockServer::start();
    let m=server.mock(|w,t|{w.method(POST).path("/api-keys/v1/introspect").json_body(json!({"apiKey":KEY})).header_missing("x-api-key");t.json_body(json!({"name":"assets-key","enabled":true,"expired":false,"scopes":[{"name":"asset","operations":["read","write"],"groupIds":["35"],"userIds":[]}]}));});
    let out = command(&server.base_url()).args(["auth", "check", "--for", "assets.get-asset"]).output().unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(document(&out)["scopes"], json!(["asset:read", "asset:write"]));
    m.assert_calls(1);
}
#[test]
fn explicit_missing_or_empty_profile_does_not_fall_through() {
    let server = MockServer::start();
    let m = server.mock(|_, t| {
        t.status(200).json_body(json!({}));
    });
    let config = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(config.path(),"default_profile='empty'\n[profiles.empty]\nkey_env='UPDRAFT_TEST_EMPTY'\n[profiles.other]\nkey_env='UPDRAFT_API_KEY'").unwrap();
    for name in ["missing", "empty"] {
        let out = command(&server.base_url())
            .env("UPDRAFT_CONFIG", config.path())
            .env("UPDRAFT_TEST_EMPTY", "")
            .args(["--profile", name, "call", "assets.get-asset", "assetId=42"])
            .output()
            .unwrap();
        assert_eq!(code(&out), 3);
        assert_eq!(document(&out)["error"]["kind"], "auth");
    }
    m.assert_calls(0);
}
#[test]
fn auth_rejection_never_tries_another_profile() {
    let server = MockServer::start();
    let m = server.mock(|w, t| {
        w.path("/assets/v1/assets/42").header("x-api-key", KEY);
        t.status(403);
    });
    let config = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        config.path(),
        "[profiles.a]\nkey_env='UPDRAFT_API_KEY'\n[profiles.b]\nkey_env='UPDRAFT_OTHER_TEST_KEY'",
    )
    .unwrap();
    let out = command(&server.base_url())
        .env("UPDRAFT_CONFIG", config.path())
        .env("UPDRAFT_OTHER_TEST_KEY", "another-key")
        .args(["--profile", "a", "call", "assets.get-asset", "assetId=42"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 3);
    assert_eq!(document(&out)["error"]["profiles"], json!(["a", "b"]));
    m.assert_calls(1);
}
#[test]
fn pagination_preserves_page_shape_and_terminal_token() {
    let server = MockServer::start();
    let first = server.mock(|w, t| {
        w.path("/assets/v1/assets/42/versions").query_param_missing("pageToken");
        t.json_body(json!({"assetVersions":[{"id":1}],"nextPageToken":"two"}));
    });
    let second = server.mock(|w, t| {
        w.path("/assets/v1/assets/42/versions").query_param("pageToken", "two");
        t.json_body(json!({"assetVersions":[{"id":2}],"nextPageToken":""}));
    });
    let out = command(&server.base_url())
        .args(["call", "assets.list-asset-versions", "assetId=42", "--all"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(document(&out)["assetVersions"].as_array().unwrap().len(), 2);
    first.assert_calls(1);
    second.assert_calls(1);
}
#[test]
fn generic_completion_polls_the_shared_matcher() {
    let server = MockServer::start();
    let create = server.mock(|w, t| {
        w.method(POST).path("/cloud/v2/universes/1/memory-store:flush");
        t.json_body(json!({"path":"universes/1/memory-store/operations/x","done":false}));
    });
    let poll = server.mock(|w, t| {
        w.method(GET).path("/cloud/v2/universes/1/memory-store/operations/x");
        t.json_body(json!({"done":true,"response":{"ok":true}}));
    });
    let out = command(&server.base_url())
        .args(["call", "cloud.flush-universe-memory-store", "universe_id=1"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stdout));
    assert_eq!(document(&out)["done"], true);
    create.assert_calls(1);
    poll.assert_calls(1);
}
#[test]
fn discovery_needs_no_key() {
    let out =
        command("http://127.0.0.1:1").env_remove("UPDRAFT_API_KEY").args(["find", "luau", "logs"]).output().unwrap();
    assert_eq!(code(&out), 0);
    assert!(!document(&out).as_array().unwrap().is_empty());
    let out = command("http://127.0.0.1:1").args(["describe", "universes.create-place-version"]).output().unwrap();
    assert_eq!(code(&out), 0);
    let d = document(&out);
    assert_eq!(d["body"]["contentType"], "application/octet-stream");
    assert_eq!(d["retry"]["attempts"], 4);
}
#[test]
fn describe_matches_the_public_contract() {
    let out = command("http://127.0.0.1:1").args(["describe", "universes.create-place-version"]).output().unwrap();
    let d = document(&out);
    assert_eq!(code(&out), 0);
    assert_eq!(d["body"], json!({"required":true,"type":"binary","contentType":"application/octet-stream"}));
    assert_eq!(d["response"]["type"], "object");
    assert_eq!(d["response"]["fields"][0]["name"], "versionNumber");
    assert_eq!(d["response"]["fields"][0]["type"], "integer");
    assert_eq!(d["params"][0]["type"], "integer");
    assert_eq!(d["retry"]["delaySeconds"], 60);
    assert_eq!(d["legacy"], false);
}
#[test]
fn array_parameters_use_repeated_query_names() {
    let server = MockServer::start();
    let m = server.mock(|w, t| {
        w.method(GET)
            .path("/legacy-develop/v1/universes/multiget/permissions")
            .query_param("ids", "1")
            .query_param("ids", "2");
        t.json_body(json!([]));
    });
    let out = command(&server.base_url())
        .args(["call", "GET", "/legacy-develop/v1/universes/multiget/permissions", "ids=1", "ids=2"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", document(&out));
    m.assert_calls(1);
    let out = command(&server.base_url())
        .args(["call", "GET", "/legacy-develop/v1/universes/multiget/permissions", "ids=1,2"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "each repeated array parameter validates its item kind");
    m.assert_calls(1);
}
#[test]
fn malformed_introspection_is_a_protocol_failure() {
    let server = MockServer::start();
    let m = server.mock(|w, t| {
        w.method(POST).path("/api-keys/v1/introspect");
        t.json_body(json!({"echo":KEY}));
    });
    let out = command(&server.base_url()).args(["auth", "check"]).output().unwrap();
    assert_eq!(code(&out), 7);
    assert_eq!(document(&out)["error"]["kind"], "unavailable");
    m.assert_calls(1);
}
#[test]
fn invalid_origin_never_sends_a_request() {
    for origin in [
        "http://127.0.0.1:1/?q=private",
        "http://127.0.0.1:1/#fragment",
        "http://user:password@127.0.0.1:1",
        "file:///private",
    ] {
        let out = command(origin).args(["call", "assets.get-asset", "assetId=42"]).output().unwrap();
        assert_eq!(code(&out), 2);
        assert_eq!(document(&out)["error"]["kind"], "usage");
    }
}
