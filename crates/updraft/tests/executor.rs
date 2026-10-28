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
        .args(["call", "luau.create-task", "universe_id=1", "place_id=2", "--body", "{}", "--no-wait"])
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
        .args(["--verbose", "call", "luau.create-task", "universe_id=1", "place_id=2", "--body", "{}", "--no-wait"])
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
        let out = command(&server.base_url()).args(["call", "universes.get", "universe_id=1"]).output().unwrap();
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
    fn collect_strings(value: &serde_json::Value, strings: &mut Vec<String>) {
        match value {
            serde_json::Value::String(value) => strings.push(value.clone()),
            serde_json::Value::Array(values) => values.iter().for_each(|value| collect_strings(value, strings)),
            serde_json::Value::Object(values) => {
                strings.extend(values.keys().cloned());
                values.values().for_each(|value| collect_strings(value, strings));
            }
            _ => {}
        }
    }

    let encoded = percent_encoding::utf8_percent_encode(KEY, percent_encoding::NON_ALPHANUMERIC).to_string();
    let mixed = encoded.replace("%2B", "%2b").replace("%2F", "%2f");
    let unicode: String = KEY.chars().map(|c| format!("\\u{:04X}", c as u32)).collect();
    let slash = KEY.replace('/', "\\/");
    for status in [200, 400, 403, 503] {
        for body in [
            format!(
                r#"{{"plain":"{KEY}","slash":"{slash}","unicode":"{unicode}","percent":"{mixed}","url":"https://cdn.test/file?signature=private"}}"#
            ),
            format!("not JSON {slash} {unicode} {mixed} {KEY} https://cdn.test/file?signature=private\n"),
        ] {
            let server = MockServer::start();
            server.mock(|w, t| {
                w.method(GET).path("/assets/v1/assets/42");
                t.status(status).body(&body);
            });
            let out = command(&server.base_url()).args(["call", "assets.get", "assetId=42"]).output().unwrap();
            let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            for form in [KEY, &encoded, &mixed, &unicode, &slash] {
                assert!(!combined.contains(form), "secret form must be redacted for HTTP {status}");
            }
            let d = document(&out);
            let mut decoded = vec![String::from_utf8_lossy(&out.stderr).into_owned()];
            collect_strings(&d, &mut decoded);
            for form in [KEY, &encoded, &mixed, &unicode, &slash] {
                assert!(
                    decoded.iter().all(|text| !text.contains(form)),
                    "decoded JSON strings and stderr must redact every secret form for HTTP {status}"
                );
            }
            if status >= 400 {
                assert_eq!(d["error"]["message"], format!("HTTP {status}"));
                assert!(!combined.contains("signature=private"), "signed queries are stripped from errors");
                if body.starts_with('{') {
                    assert_eq!(d["error"]["detail"]["slash"], "<redacted>");
                    assert_eq!(d["error"]["detail"]["unicode"], "<redacted>");
                }
            }
        }
    }
}

#[test]
fn error_urls_keep_the_host_when_the_path_contains_an_at_sign() {
    let server = MockServer::start();
    server.mock(|_, t| {
        t.status(400).json_body(json!({"url":"https://cdn.test/folder/@asset/file?signature=private"}));
    });
    let out = command(&server.base_url()).args(["call", "assets.get", "assetId=42"]).output().unwrap();
    assert_eq!(document(&out)["error"]["detail"]["url"], "https://cdn.test/folder/@asset/file");
}

#[test]
fn named_calls_and_url_calls_bind_folded_parameters() {
    let server = MockServer::start();
    let m = server.mock(|w, t| {
        w.method(GET).path("/assets/v1/assets/42").query_param("readMask", "displayName").header("x-api-key", KEY);
        t.json_body(json!({"displayName":"Sofa"}));
    });
    for args in [
        vec!["call", "assets.get", "asset-id=42", "read_mask=displayName"],
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
        vec!["call", "assets.get"],
        vec!["call", "assets.get", "assetId=42", "invented=5"],
        vec!["call", "universes.create-place-version", "universeId=nope", "placeId=2", "--body", "x"],
        vec!["call", "assets.get", "assetId=42", "--body", "{}"],
        vec!["call", "luau.create-task", "universe_id=1", "place_id=2", "--body", "{"],
        vec!["call", "assets.get", "assetId=42", "--all"],
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
    let out = command(&server.base_url()).args(["auth", "check", "--for", "assets.get"]).output().unwrap();
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
            .args(["--profile", name, "call", "assets.get", "assetId=42"])
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
        .args(["--profile", "a", "call", "assets.get", "assetId=42"])
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
    let out =
        command(&server.base_url()).args(["call", "assets.list-versions", "assetId=42", "--all"]).output().unwrap();
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
    let out = command(&server.base_url()).args(["call", "memory-store.flush", "universe_id=1"]).output().unwrap();
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
        let out = command(origin).args(["call", "assets.get", "assetId=42"]).output().unwrap();
        assert_eq!(code(&out), 2);
        assert_eq!(document(&out)["error"]["kind"], "usage");
    }
}
#[test]
fn successful_url_payloads_are_preserved() {
    let server = MockServer::start();
    let url = "https://example.test/path?page=2#section";
    server.mock(|w, t| {
        w.path("/assets/v1/assets/42");
        t.json_body(json!({"url":url}));
    });
    let out = command(&server.base_url()).args(["call", "assets.get", "assetId=42"]).output().unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(document(&out)["url"], url);
}
#[test]
fn max_items_preserves_default_page_size_and_bounds_the_last_page() {
    let server = MockServer::start();
    let first = server.mock(|w, t| {
        w.path("/assets/v1/assets/42/versions").query_param_missing("pageToken").query_param_missing("maxPageSize");
        t.json_body(json!({"assetVersions":[1,2,3],"nextPageToken":"next"}));
    });
    let last = server.mock(|w, t| {
        w.path("/assets/v1/assets/42/versions").query_param("pageToken", "next").query_param("maxPageSize", "2");
        t.json_body(json!({"assetVersions":[4,5],"nextPageToken":"rest"}));
    });
    let out = command(&server.base_url())
        .args(["call", "assets.list-versions", "assetId=42", "--all", "--max-items", "5"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", document(&out));
    assert_eq!(document(&out)["assetVersions"], json!([1, 2, 3, 4, 5]));
    assert_eq!(document(&out)["nextPageToken"], "rest");
    first.assert_calls(1);
    last.assert_calls(1);
}
#[test]
fn an_overfull_default_page_is_refetched_with_a_safe_size() {
    let server = MockServer::start();
    let first = server.mock(|w, t| {
        w.path("/assets/v1/assets/42/versions").query_param_missing("maxPageSize");
        t.json_body(json!({"assetVersions":[1,2,3],"nextPageToken":"after-three"}));
    });
    let resized = server.mock(|w, t| {
        w.path("/assets/v1/assets/42/versions").query_param("maxPageSize", "2");
        t.json_body(json!({"assetVersions":[1,2],"nextPageToken":"after-two"}));
    });
    let out = command(&server.base_url())
        .args(["call", "assets.list-versions", "assetId=42", "--all", "--max-items", "2"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", document(&out));
    assert_eq!(document(&out)["assetVersions"], json!([1, 2]));
    assert_eq!(document(&out)["nextPageToken"], "after-two");
    first.assert_calls(1);
    resized.assert_calls(1);
}

#[test]
fn empty_success_bodies_are_not_retried_for_any_declared_payload() {
    for (method, path, name, pairs) in [
        (POST, "/assets/v1/assets/42:archive", "assets.archive", vec!["assetId=42"]),
        (
            httpmock::Method::DELETE,
            "/cloud/v2/universes/1/data-stores/store/entries/entry",
            "data-stores.delete-entry",
            vec!["universe_id=1", "data_store_id=store", "entry_id=entry"],
        ),
    ] {
        let server = MockServer::start();
        let m = server.mock(|w, t| {
            w.method(method).path(path);
            t.status(200).body("");
        });
        let out = command(&server.base_url()).args(["call", name]).args(pairs).output().unwrap();
        assert_eq!(code(&out), 0, "empty HTTP 200 is success: {}", document(&out));
        assert!(document(&out).is_null(), "an empty success prints null");
        m.assert_calls(1);
    }
}

#[test]
fn unknown_operation_has_a_plain_message_and_structured_suggestions() {
    let out = command("http://127.0.0.1:1").args(["call", "assets.nope"]).output().unwrap();
    assert_eq!(code(&out), 2);
    let error = &document(&out)["error"];
    assert_eq!(error["message"], "unknown operation assets.nope");
    assert_eq!(error["suggestions"].as_array().unwrap().len(), 3);
}

#[test]
fn generic_call_rejects_the_removed_output_flag() {
    let server = MockServer::start();
    let m = server.mock(|_, t| {
        t.json_body(json!({}));
    });
    let out =
        command(&server.base_url()).args(["call", "assets.get", "assetId=42", "--out", "unused"]).output().unwrap();
    assert_eq!(code(&out), 2, "only the asset download flow has an output file");
    m.assert_calls(0);
}

#[test]
fn introspection_owns_its_body_key_and_rejects_caller_bodies() {
    let server = MockServer::start();
    let m = server.mock(|w, t| {
        w.method(POST).path("/api-keys/v1/introspect").json_body(json!({"apiKey":KEY})).header_missing("x-api-key");
        t.status(400).json_body(json!({"key":KEY}));
    });
    let out = command(&server.base_url()).args(["call", "api-keys.introspect"]).output().unwrap();
    assert_eq!(code(&out), 4, "the selected body key reaches introspection: {}", document(&out));
    assert_eq!(document(&out)["error"]["detail"]["key"], "<redacted>");
    for body in ["{}", "@missing-key-file.json"] {
        let out = command(&server.base_url()).args(["call", "api-keys.introspect", "--body", body]).output().unwrap();
        assert_eq!(code(&out), 2, "introspection accepts no caller-supplied body");
    }
    m.assert_calls(1);
}

#[test]
fn auth_failures_share_scope_and_profile_metadata() {
    let server = MockServer::start();
    server.mock(|w, t| {
        w.path("/assets/v1/assets/42");
        t.status(403);
    });
    for missing in [true, false] {
        let mut cmd = command(&server.base_url());
        if missing {
            cmd.env_remove("UPDRAFT_API_KEY");
        }
        let out = cmd.args(["call", "assets.get", "assetId=42"]).output().unwrap();
        let error = &document(&out)["error"];
        assert_eq!(code(&out), 3);
        assert_eq!(error["scopeMetadata"], "declared");
        assert!(!error["requiredScopes"].as_array().unwrap().is_empty());
        assert_eq!(error["profiles"], json!([]));
    }
    server.mock(|w, t| {
        w.path("/api-keys/v1/introspect");
        t.status(401);
    });
    let out = command(&server.base_url()).args(["auth", "check"]).output().unwrap();
    assert_eq!(code(&out), 3);
    assert_eq!(document(&out)["error"]["scopeMetadata"], "declared", "body-key introspection requires no scope");
}

#[test]
fn excessive_retry_after_stops_without_waiting_or_retrying() {
    for (name, pairs, status, expected) in [
        ("assets.get", vec!["assetId=42"], 429, 7),
        ("luau.create-task", vec!["universe_id=1", "place_id=2", "--body", "{}"], 503, 5),
    ] {
        let server = MockServer::start();
        let m = server.mock(|_, t| {
            t.status(status).header("retry-after", "301");
        });
        let out = command(&server.base_url()).args(["call", name]).args(pairs).output().unwrap();
        assert_eq!(code(&out), expected);
        assert_eq!(document(&out)["error"]["retryAfter"], 301, "report the requested wait");
        m.assert_calls(1);
    }
}

#[test]
fn max_pages_stops_with_a_continuation_token() {
    let server = MockServer::start();
    let first = server.mock(|w, t| {
        w.query_param_missing("pageToken");
        t.json_body(json!({"assetVersions":[1],"nextPageToken":"two"}));
    });
    let second = server.mock(|w, t| {
        w.query_param("pageToken", "two");
        t.json_body(json!({"assetVersions":[2],"nextPageToken":"three"}));
    });
    let out = command(&server.base_url())
        .args(["call", "assets.list-versions", "assetId=42", "--all", "--max-pages", "2"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", document(&out));
    assert_eq!(document(&out)["assetVersions"], json!([1, 2]));
    assert_eq!(document(&out)["nextPageToken"], "three");
    first.assert_calls(1);
    second.assert_calls(1);
}

#[test]
fn an_unresizable_overfull_page_is_a_usage_error() {
    let server = MockServer::start();
    let m = server.mock(|_, t| {
        t.json_body(json!({"assetVersions":[1,2],"nextPageToken":"next"}));
    });
    let out = command(&server.base_url())
        .args(["call", "assets.list-versions", "assetId=42", "maxPageSize=1", "--all", "--max-items", "1"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 2, "a smaller item cap cannot silently drop records");
    m.assert_calls(1);
}

#[test]
fn retry_after_beyond_integer_range_stops_and_reports_the_server_value() {
    let server = MockServer::start();
    let value = "184467440737095516160";
    let m = server.mock(|_, t| {
        t.status(429).header("retry-after", value);
    });
    let out = command(&server.base_url()).args(["call", "assets.get", "assetId=42"]).output().unwrap();
    assert_eq!(code(&out), 7);
    assert_eq!(document(&out)["error"]["retryAfter"], value);
    m.assert_calls(1);
}
