#[path = "support/sequence.rs"]
mod sequence;
mod support;
use sequence::{Response, Server};
use serde_json::json;
use support::*;
fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ChaletAssets.fbx");
    std::fs::write(&path, "fbx fixture").unwrap();
    (dir, path)
}
fn upload(server: &Server, path: &std::path::Path) -> std::process::Output {
    command(&server.origin)
        .args([
            "asset",
            "upload",
            path.to_str().unwrap(),
            "--group",
            "12345678901234567890",
            "--description",
            "Model for Fragile Delivery.",
        ])
        .output()
        .unwrap()
}
#[test]
fn uploads_one_fbx_with_group_ownership_and_polls_the_accepted_operation() {
    let server = Server::new(vec![
        Response::json(json!({"path":"operations/safe-id"})),
        Response::json(json!({"done":false})),
        Response::json(json!({"done":true,"response":{"assetId":"12345678901234567890"}})),
    ]);
    let (_dir, path) = fixture();
    let out = upload(&server, &path);
    assert_eq!(code(&out), 0, "{}", document(&out));
    let d = document(&out);
    assert_eq!(d["assetId"], "12345678901234567890");
    assert_eq!(d["displayName"], "ChaletAssets");
    assert_eq!(d["assetType"], "Model");
    let calls = server.requests();
    assert_eq!(calls.len(), 3);
    assert!(calls[0].head.starts_with("POST /assets/v1/assets "));
    assert!(calls[1..].iter().all(|c| c.head.starts_with("GET /assets/v1/operations/safe-id ")));
    let multipart = String::from_utf8_lossy(&calls[0].body);
    assert!(multipart.contains("filename=\"ChaletAssets.fbx\""));
    assert!(multipart.contains("Content-Type: model/fbx"));
    assert!(multipart.contains("fbx fixture"));
    let request =
        multipart.split("Content-Type: application/json\r\n\r\n").nth(1).unwrap().split("\r\n").next().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(request).unwrap(),
        json!({"assetType":"Model","displayName":"ChaletAssets","description":"Model for Fragile Delivery.","creationContext":{"creator":{"groupId":"12345678901234567890"}}})
    );
    assert!(calls.iter().all(|c| c.head.to_lowercase().contains(&format!("x-api-key: {}", KEY.to_lowercase()))));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Operation safe-id: waiting"));
}
#[test]
fn rejected_uploads_and_lost_responses_are_distinct_and_never_resent() {
    let (_dir, path) = fixture();
    for (reply, expected) in [
        (Response::Body(403, format!("refused {KEY}")), 3),
        (Response::Body(400, "rejected".into()), 4),
        (Response::Reset, 5),
        (Response::Truncated(200), 5),
        (Response::Truncated(429), 7),
        (Response::Truncated(400), 4),
        (Response::Body(503, "unavailable".into()), 5),
        (Response::Body(200, "not json".into()), 5),
        (Response::json(json!({})), 5),
        (Response::Body(429, "slow".into()), 7),
    ] {
        let server = Server::new(vec![reply]);
        let out = upload(&server, &path);
        assert_eq!(code(&out), expected, "{}", document(&out));
        assert_eq!(server.requests().len(), 1, "the upload is sent exactly once");
        let combined = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(!combined.contains(KEY), "key is redacted on every error path");
        if expected == 5 {
            assert!(document(&out)["error"]["hint"].as_str().unwrap().contains("Check the group's Creations"));
        }
    }
}
#[test]
fn accepts_both_operation_id_shapes_and_rejects_unsafe_paths_after_creation() {
    let (_dir, path) = fixture();
    for body in [json!({"operationId":"token"}), json!({"path":"operations/token"})] {
        let server =
            Server::new(vec![Response::json(body), Response::json(json!({"done":true,"response":{"assetId":"42"}}))]);
        let out = upload(&server, &path);
        assert_eq!(code(&out), 0);
        assert_eq!(server.requests().len(), 2);
    }
    for body in [json!({}), json!({"path":"operations/../bad"}), json!({"operationId":"https://evil/id"})] {
        let server = Server::new(vec![Response::json(body)]);
        let out = upload(&server, &path);
        assert_eq!(code(&out), 5);
        assert_eq!(server.requests().len(), 1);
        assert!(document(&out)["error"]["message"].as_str().unwrap().contains("no valid operation ID"));
    }
}
#[test]
fn invalid_files_and_sizes_never_send_a_request() {
    let (_dir, path) = fixture();
    let server = Server::new(vec![]);
    for size in [0, 20_000_001] {
        std::fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(size).unwrap();
        let out = upload(&server, &path);
        assert_eq!(code(&out), 2);
    }
    for file in ["bad.txt", "missing.fbx"] {
        let out = upload(&server, std::path::Path::new(file));
        assert_eq!(code(&out), 2);
    }
    assert!(server.requests().is_empty());
}
#[test]
fn invalid_arguments_and_resume_paths_never_send_a_request() {
    let server = Server::new(vec![]);
    for args in [
        vec![],
        vec!["a.fbx", "b.fbx"],
        vec!["--resume"],
        vec!["--resume", "../id"],
        vec!["--resume", "https://evil/id"],
        vec!["--resume", "operations/id?x"],
        vec!["--resume", "safe-id", "--group", "35"],
    ] {
        let out = command(&server.origin).args(["asset", "upload"]).args(&args).output().unwrap();
        assert_eq!(code(&out), 2, "{args:?}");
        document(&out);
    }
    for group in ["", "0", "-1", "1e3", "1.5"] {
        let out = command(&server.origin).args(["asset", "upload", "missing.fbx", "--group", group]).output().unwrap();
        assert_eq!(code(&out), 2);
    }
    let help = command(&server.origin).args(["asset", "upload", "--help"]).output().unwrap();
    assert_eq!(code(&help), 0, "standard help is the intentional plain-text exception");
    assert!(server.requests().is_empty());
}
#[test]
fn resumes_without_a_group_and_only_sends_get() {
    for id in ["safe-id", "operations/safe-id"] {
        let server = Server::new(vec![Response::json(json!({"done":true,"response":{"assetId":"42"}}))]);
        let out = command(&server.origin).args(["asset", "upload", "--resume", id]).output().unwrap();
        assert_eq!(code(&out), 0, "{}", document(&out));
        let d = document(&out);
        assert_eq!(d["assetId"], "42");
        assert!(d["displayName"].is_null() && d["assetType"].is_null());
        assert_eq!(server.requests().len(), 1);
        assert!(server.requests()[0].head.starts_with("GET /assets/v1/operations/safe-id "));
    }
}
#[test]
fn resume_stops_at_each_api_or_protocol_failure_and_retains_recovery() {
    for (reply, expected, requests) in [
        (Response::Body(401, "unauthorized".into()), 3, 1),
        (Response::Body(403, "refused".into()), 3, 1),
        (Response::Body(404, "missing".into()), 4, 1),
        (Response::Body(429, format!("slow {KEY}")), 7, 3),
        (Response::Body(500, "broken".into()), 7, 3),
        (Response::json(json!({"done":true,"error":{"code":"BAD","message":"reason"}})), 6, 1),
        (Response::json(json!({"done":true})), 7, 1),
        (Response::json(json!({"done":true,"response":{"assetId":42}})), 7, 1),
        (Response::Body(200, "not json".into()), 7, 3),
        (Response::Reset, 7, 3),
    ] {
        let replies = match reply {
            Response::Body(s, b) => {
                vec![Response::Body(s, b.clone()), Response::Body(s, b.clone()), Response::Body(s, b)]
            }
            other => vec![other, Response::Reset, Response::Reset],
        };
        let server = Server::new(replies);
        let out = command(&server.origin).args(["asset", "upload", "--resume", "safe-id"]).output().unwrap();
        assert_eq!(code(&out), expected, "{}", document(&out));
        assert_eq!(server.requests().len(), requests);
        assert!(server.requests().iter().all(|c| c.head.starts_with("GET ")));
        assert_eq!(document(&out)["error"]["resume"], "updraft asset upload --resume safe-id");
        assert!(!String::from_utf8_lossy(&out.stdout).contains(KEY));
    }
}
#[test]
fn deadline_retains_a_get_only_resume_and_bounded_poll_can_finish() {
    let server = Server::new(vec![]);
    let out =
        command(&server.origin).args(["--deadline", "0", "asset", "upload", "--resume", "safe-id"]).output().unwrap();
    assert_eq!(code(&out), 8);
    assert_eq!(document(&out)["error"]["resume"], "updraft asset upload --resume safe-id");
    assert!(server.requests().is_empty());
    let server = Server::new(vec![
        Response::json(json!({"done":false})),
        Response::json(json!({"done":true,"response":{"assetId":"42"}})),
    ]);
    let out =
        command(&server.origin).args(["--deadline", "1", "asset", "upload", "--resume", "safe-id"]).output().unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(server.requests().len(), 2);
}
#[test]
fn publish_retries_conflicts_four_times_then_rejects() {
    let server = Server::new((0..4).map(|_| Response::Body(409, "conflict".into())).collect());
    let (_dir, file) = fixture();
    let out = command(&server.origin)
        .args(["place", "publish", file.to_str().unwrap(), "--universe", "1", "--place", "2"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 4);
    assert_eq!(server.requests().len(), 4);
    assert!(server.requests().iter().all(|c| c.body == b"fbx fixture"
        && c.head.to_lowercase().contains("content-type: application/octet-stream")
        && c.head.starts_with("POST /universes/v1/1/places/2/versions?versionType=Published ")));
    assert!(String::from_utf8_lossy(&out.stderr).contains("HTTP 409, retrying in 0 s (attempt 2 of 4)"));
}
#[test]
fn publish_retains_uncertainty_until_a_later_success() {
    let (_dir, file) = fixture();
    for (replies, expected, count) in [
        (vec![Response::Body(503, "unavailable".into()), Response::Body(400, "bad".into())], 5, 2),
        (vec![Response::Body(503, "unavailable".into()), Response::json(json!({"versionNumber":7}))], 0, 2),
        (vec![Response::Body(409, "conflict".into()), Response::Body(400, "bad".into())], 4, 2),
        (vec![Response::Reset], 5, 1),
        (vec![Response::Body(200, "not json".into())], 5, 1),
        (vec![Response::json(json!({}))], 5, 1),
        (vec![Response::Body(404, "missing".into())], 4, 1),
    ] {
        let server = Server::new(replies);
        let out = command(&server.origin)
            .args(["place", "publish", file.to_str().unwrap(), "--universe", "1", "--place", "2"])
            .output()
            .unwrap();
        assert_eq!(code(&out), expected, "{}", document(&out));
        assert_eq!(server.requests().len(), count);
        if expected == 0 {
            assert_eq!(
                document(&out),
                json!({"universeId":"1","placeId":"2","versionNumber":7,"versionType":"published"})
            );
        }
    }
}
#[test]
fn downloads_without_a_key_and_refuses_implicit_overwrite() {
    let server = Server::new(vec![
        Response::json(json!({"location":"$ORIGIN/content?signature=private"})),
        Response::Body(200, "asset bytes".into()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.rbxm");
    let out =
        command(&server.origin).args(["asset", "download", "42", "--out", path.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&out), 0, "{}", document(&out));
    assert_eq!(std::fs::read(&path).unwrap(), b"asset bytes");
    assert_eq!(document(&out)["bytes"], 11);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].head.to_lowercase().contains("x-api-key:"));
    assert!(!requests[1].head.to_lowercase().contains("x-api-key:"));
    assert!(!String::from_utf8_lossy(&out.stdout).contains("signature=private"));
    let out =
        command(&server.origin).args(["asset", "download", "42", "--out", path.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&out), 2);
    assert_eq!(server.requests().len(), 2);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no temporary files remain");
}
#[test]
fn failed_download_preserves_target_and_successful_overwrite_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.rbxm");
    std::fs::write(&path, "previous").unwrap();
    let server = Server::new(vec![Response::json(json!({"location":"$ORIGIN/file"})), Response::Truncated(200)]);
    let out = command(&server.origin)
        .args(["asset", "download", "42", "--out", path.to_str().unwrap(), "--overwrite"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 7);
    assert_eq!(std::fs::read(&path).unwrap(), b"previous");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    let server = Server::new(vec![
        Response::json(json!({"location":"$ORIGIN/file"})),
        Response::Body(200, "replacement".into()),
    ]);
    let out = command(&server.origin)
        .args(["asset", "download", "42", "--out", path.to_str().unwrap(), "--overwrite"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0);
    assert_eq!(std::fs::read(&path).unwrap(), b"replacement");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
#[test]
fn download_redirects_are_key_free_and_bounded() {
    use httpmock::{Method::GET, MockServer};
    let server = MockServer::start();
    let delivery = server.mock(|w, t| {
        w.method(GET).path("/asset-delivery-api/v1/assetId/42");
        t.json_body(json!({"location":format!("{}/r0",server.base_url())}));
    });
    let mut redirects = vec![];
    for n in 0..5 {
        redirects.push(server.mock(|w, t| {
            w.method(GET).path(format!("/r{n}")).header_missing("x-api-key");
            t.status(302).header("location", format!("/r{}", n + 1));
        }));
    }
    let file = server.mock(|w, t| {
        w.method(GET).path("/r5").header_missing("x-api-key");
        t.body("download");
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    let out = command(&server.base_url())
        .args(["asset", "download", "42", "--out", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", document(&out));
    delivery.assert_calls(1);
    file.assert_calls(1);
    for r in redirects {
        r.assert_calls(1);
    }
    let server = MockServer::start();
    server.mock(|w, t| {
        w.path("/asset-delivery-api/v1/assetId/42");
        t.json_body(json!({"location":format!("{}/loop",server.base_url())}));
    });
    let looping = server.mock(|w, t| {
        w.path("/loop").header_missing("x-api-key");
        t.status(302).header("location", "/loop");
    });
    let path = dir.path().join("loop");
    let out = command(&server.base_url())
        .args(["asset", "download", "42", "--out", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&out), 7);
    looping.assert_calls(6);
    assert!(!path.exists());
}
#[test]
fn download_rejects_insecure_or_credentialed_destinations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    for location in [
        "http://example.com/file?secret=hidden",
        "https://user:password@example.com/file",
        "file:///private",
        "not a URL",
    ] {
        let server = Server::new(vec![Response::json(json!({"location":location}))]);
        let out = command(&server.origin)
            .args(["asset", "download", "42", "--out", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert_eq!(code(&out), 7);
        assert_eq!(server.requests().len(), 1);
        assert!(!path.exists());
    }
    use httpmock::MockServer;
    let server = MockServer::start();
    server.mock(|w, t| {
        w.path("/asset-delivery-api/v1/assetId/42");
        t.json_body(json!({"location":format!("{}/redirect",server.base_url())}));
    });
    server.mock(|w, t| {
        w.path("/redirect");
        t.status(302).header("location", "http://example.com/file?signature=hidden");
    });
    let out = command(&server.base_url())
        .args(["asset", "download", "42", "--out", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&out), 7);
    assert!(!path.exists());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("signature=hidden"));
}
#[test]
fn luau_reads_all_log_pages_for_success_failure_and_cancellation() {
    for state in ["COMPLETE", "FAILED", "CANCELLED"] {
        let path = "universes/1/places/2/versions/3/luau-execution-sessions/s/tasks/t";
        let server = Server::new(vec![
            Response::json(json!({"path":path,"state":"QUEUED"})),
            Response::json(json!({"state":state,"output":{"results":[42]},"error":{"message":"Script error: boom"}})),
            Response::json(json!({"luauExecutionSessionTaskLogs":[{"messages":["one"]}],"nextPageToken":"two"})),
            Response::json(json!({"luauExecutionSessionTaskLogs":[{"messages":["two"]}]})),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("script.luau");
        std::fs::write(&file, "return 42").unwrap();
        let out = command(&server.origin)
            .args(["luau", "run", file.to_str().unwrap(), "--universe", "1", "--place", "2"])
            .output()
            .unwrap();
        let d = document(&out);
        assert_eq!(code(&out), if state == "COMPLETE" { 0 } else { 6 }, "{d}");
        let detail = if state == "COMPLETE" { &d } else { &d["error"]["detail"] };
        assert_eq!(detail["logs"], json!(["one", "two"]));
        assert_eq!(detail["task"], path);
        assert_eq!(detail["state"], state);
        assert_eq!(detail["results"], json!([42]));
        let requests = server.requests();
        assert_eq!(requests.len(), 4);
        assert!(requests[3].head.contains("pageToken=two"));
        assert!(requests[1..].iter().all(|c| c.head.starts_with("GET ")));
    }
}
#[test]
fn accepted_luau_unmatched_path_is_unknown_and_retains_the_path() {
    let path = "universes/1/places/2/tasks/unmatched";
    let server = Server::new(vec![Response::json(json!({"path":path,"state":"QUEUED"}))]);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("script.luau");
    std::fs::write(&file, "return 42").unwrap();
    let out = command(&server.origin)
        .args(["luau", "run", file.to_str().unwrap(), "--universe", "1", "--place", "2"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 5);
    assert_eq!(document(&out)["error"]["path"], path);
    assert_eq!(server.requests().len(), 1);
}
#[test]
fn luau_reads_stdin_and_preserves_script_failure_if_logs_fail() {
    use std::io::Write;
    let path = "universes/1/places/2/versions/3/luau-execution-sessions/s/tasks/t";
    for state in ["FAILED", "COMPLETE"] {
        let server = Server::new(vec![
            Response::json(json!({"path":path,"state":state})),
            Response::Body(404, "logs missing".into()),
        ]);
        let mut child = command(&server.origin)
            .args(["luau", "run", "-", "--universe", "1", "--place", "2"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"print('stdin')").unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(code(&out), if state == "FAILED" { 6 } else { 0 });
        let d = document(&out);
        let detail = if state == "FAILED" { &d["error"]["detail"] } else { &d };
        assert_eq!(detail["logsComplete"], false);
        assert!(String::from_utf8_lossy(&out.stderr).contains("logs could not be read"));
        assert!(String::from_utf8_lossy(&server.requests()[0].body).contains("print('stdin')"));
    }
}
#[test]
fn accepted_upload_deadline_includes_an_inflight_poll() {
    use httpmock::MockServer;
    let server = MockServer::start();
    let poll = server.mock(|w, t| {
        w.path("/assets/v1/operations/slow");
        t.delay(std::time::Duration::from_secs(5)).json_body(json!({"done":false}));
    });
    let started = std::time::Instant::now();
    let out =
        command(&server.base_url()).args(["--deadline", "1", "asset", "upload", "--resume", "slow"]).output().unwrap();
    assert_eq!(code(&out), 8, "a pending operation retains its deadline even while a lookup runs");
    assert_eq!(document(&out)["error"]["resume"], "updraft asset upload --resume slow");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "the client returns before the delayed poll response"
    );
    poll.assert_calls(1);
}
#[test]
fn lost_download_body_retries_from_an_empty_temporary_file() {
    let server = Server::new(vec![
        Response::json(json!({"location":"$ORIGIN/file"})),
        Response::Truncated(200),
        Response::Body(200, "complete bytes".into()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    let out =
        command(&server.origin).args(["asset", "download", "42", "--out", path.to_str().unwrap()]).output().unwrap();
    assert_eq!(code(&out), 0, "{}", document(&out));
    assert_eq!(std::fs::read(&path).unwrap(), b"complete bytes");
    assert_eq!(server.requests().len(), 3);
    assert!(server.requests()[1..].iter().all(|r| !r.head.to_lowercase().contains("x-api-key:")));
}
#[test]
fn luau_poll_failure_retains_the_get_resume_command() {
    let path = "universes/1/places/2/versions/3/luau-execution-sessions/s/tasks/t";
    let server = Server::new(vec![
        Response::json(json!({"path":path,"state":"QUEUED"})),
        Response::Body(503, "unavailable".into()),
        Response::Body(503, "unavailable".into()),
        Response::Body(503, "unavailable".into()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("script.luau");
    std::fs::write(&file, "return 42").unwrap();
    let out = command(&server.origin)
        .args(["luau", "run", file.to_str().unwrap(), "--universe", "1", "--place", "2"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 8);
    let d = document(&out);
    assert_eq!(d["error"]["resume"], format!("updraft luau run --resume {path}"));
    assert_eq!(d["error"]["last"]["path"], path);
    assert_eq!(server.requests().len(), 4);
    assert_eq!(server.requests().iter().filter(|r| r.head.starts_with("POST ")).count(), 1);
}
#[test]
fn luau_create_without_a_task_path_is_unknown_and_not_repeated() {
    let server = Server::new(vec![Response::json(json!({"state":"QUEUED"}))]);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("script.luau");
    std::fs::write(&file, "return 42").unwrap();
    let out = command(&server.origin)
        .args(["luau", "run", file.to_str().unwrap(), "--universe", "1", "--place", "2"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 5);
    assert_eq!(document(&out)["error"]["kind"], "outcome_unknown");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn pagination_defaults_to_one_hundred_pages() {
    let server = Server::new(
        (0..101).map(|n| Response::json(json!({"assetVersions":[n],"nextPageToken":format!("page-{}",n+1)}))).collect(),
    );
    let out = command(&server.origin).args(["call", "assets.list-versions", "assetId=42", "--all"]).output().unwrap();
    assert_eq!(code(&out), 0, "{}", document(&out));
    assert_eq!(document(&out)["assetVersions"].as_array().unwrap().len(), 100);
    assert_eq!(document(&out)["nextPageToken"], "page-100");
    assert_eq!(server.requests().len(), 100, "unique next tokens cannot make pagination unbounded");
}

#[test]
fn accepted_uploads_never_report_repeat_safe_poll_or_receipt_failures() {
    let (_dir, file) = fixture();
    for reply in [
        Response::Body(401, "unauthorized".into()),
        Response::Body(403, "refused".into()),
        Response::Body(404, "missing".into()),
        Response::Body(429, "slow".into()),
        Response::Body(503, "broken".into()),
        Response::Body(200, "not json".into()),
        Response::Reset,
        Response::json(json!({"done":true})),
        Response::json(json!({"done":true,"response":{"assetId":42}})),
    ] {
        let server = Server::new(vec![
            Response::json(json!({"path":"operations/safe-id"})),
            reply,
            Response::Reset,
            Response::Reset,
        ]);
        let out = upload(&server, &file);
        assert_eq!(code(&out), 8, "accepted uploads resume after any later failure: {}", document(&out));
        assert_eq!(document(&out)["error"]["resume"], "updraft asset upload --resume safe-id");
        assert_eq!(server.requests().iter().filter(|r| r.head.starts_with("POST ")).count(), 1);
    }
}

#[test]
fn generic_acceptance_maps_later_failures_and_readers_wait_on_resume() {
    for reply in [
        Response::Body(403, "forbidden".into()),
        Response::Body(404, "missing".into()),
        Response::Body(503, "down".into()),
        Response::Body(200, "invalid".into()),
    ] {
        let server = Server::new(vec![
            Response::json(json!({"path":"universes/1/memory-store/operations/x","done":false})),
            reply,
            Response::Reset,
            Response::Reset,
        ]);
        let out = command(&server.origin).args(["call", "memory-store.flush", "universe_id=1"]).output().unwrap();
        assert_eq!(code(&out), 8, "{}", document(&out));
        assert_eq!(
            document(&out)["error"]["resume"],
            "updraft call GET /cloud/v2/universes/1/memory-store/operations/x"
        );
        assert_eq!(server.requests().iter().filter(|r| r.head.starts_with("POST ")).count(), 1);
    }
    let server = Server::new(vec![Response::json(json!({}))]);
    let out = command(&server.origin).args(["call", "memory-store.flush", "universe_id=1"]).output().unwrap();
    assert_eq!(code(&out), 5, "a missing operation identity cannot authorize another write");
    assert!(document(&out)["error"]["hint"].as_str().unwrap().contains("Creations"));
}

#[test]
fn luau_resume_waits_reads_logs_and_never_posts() {
    let path = "universes/1/places/2/versions/3/luau-execution-sessions/s/tasks/t";
    for state in ["COMPLETE", "FAILED", "CANCELLED"] {
        let server = Server::new(vec![
            Response::json(json!({"state":"PROCESSING"})),
            Response::json(json!({"state":state,"output":{"results":[42]}})),
            Response::json(json!({"luauExecutionSessionTaskLogs":[{"messages":["resumed"]}]})),
        ]);
        let out = command(&server.origin).args(["luau", "run", "--resume", path]).output().unwrap();
        assert_eq!(code(&out), if state == "COMPLETE" { 0 } else { 6 }, "{}", document(&out));
        let d = document(&out);
        let detail = if state == "COMPLETE" { &d } else { &d["error"]["detail"] };
        assert_eq!(detail["logs"], json!(["resumed"]));
        assert_eq!(server.requests().len(), 3);
        assert!(server.requests().iter().all(|r| r.head.starts_with("GET ")));
    }
    for (status, expected) in [(401, 3), (403, 3), (404, 4), (503, 7)] {
        let server = Server::new(vec![
            Response::Body(status, "failed".into()),
            Response::Body(status, "failed".into()),
            Response::Body(status, "failed".into()),
        ]);
        let out = command(&server.origin).args(["luau", "run", "--resume", path]).output().unwrap();
        assert_eq!(code(&out), expected, "a GET-only resume keeps read failure codes");
        assert!(server.requests().iter().all(|r| r.head.starts_with("GET ")));
    }
}

#[test]
fn generic_readers_wait_by_default_and_no_wait_reads_once() {
    for no_wait in [false, true] {
        let server = Server::new(vec![
            Response::json(json!({"done":false})),
            Response::json(json!({"done":true,"response":{"ok":true}})),
        ]);
        let mut cmd = command(&server.origin);
        cmd.args(["call", "GET", "/assets/v1/operations/x"]);
        if no_wait {
            cmd.arg("--no-wait");
        }
        let out = cmd.output().unwrap();
        assert_eq!(code(&out), 0, "{}", document(&out));
        assert_eq!(document(&out)["done"], !no_wait);
        assert_eq!(server.requests().len(), if no_wait { 1 } else { 2 });
        assert!(server.requests().iter().all(|r| r.head.starts_with("GET ")));
    }
}

#[test]
fn terminal_luau_without_a_log_path_keeps_its_terminal_result() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("probe.luau");
    std::fs::write(&script, "return 42").unwrap();
    for state in ["COMPLETE", "FAILED", "CANCELLED"] {
        let server = Server::new(vec![Response::json(json!({"state":state,"output":{"results":[42]}}))]);
        let out = command(&server.origin)
            .args(["luau", "run", script.to_str().unwrap(), "--universe", "1", "--place", "2"])
            .output()
            .unwrap();
        assert_eq!(code(&out), if state == "COMPLETE" { 0 } else { 6 }, "{}", document(&out));
        let d = document(&out);
        let detail = if state == "COMPLETE" { &d } else { &d["error"]["detail"] };
        assert_eq!(detail["logsComplete"], false);
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn accepted_write_with_a_closed_stdout_keeps_an_unsafe_outcome_code() {
    fn run(mut command: std::process::Command) -> std::process::Output {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        command.stdout(writer).stderr(std::process::Stdio::piped()).output().unwrap()
    }
    for (reply, args, expected, requests) in [
        (Response::json(json!({})), vec!["call", "assets.archive", "assetId=42"], 5, 1),
        (Response::Body(503, "uncertain".into()), vec!["call", "assets.archive", "assetId=42"], 5, 1),
        (Response::json(json!({})), vec!["--deadline", "0", "asset", "upload", "--resume", "x"], 8, 0),
    ] {
        let server = Server::new(vec![reply]);
        let mut cmd = command(&server.origin);
        cmd.args(args);
        let out = run(cmd);
        assert_eq!(code(&out), expected, "stdout failure must not erase an accepted, unknown, or pending outcome");
        assert_eq!(server.requests().len(), requests);
    }
    let (_dir, script) = fixture();
    let server = Server::new(vec![Response::json(json!({"state":"FAILED"}))]);
    let mut cmd = command(&server.origin);
    cmd.args(["luau", "run", script.to_str().unwrap(), "--universe", "1", "--place", "2"]);
    let out = run(cmd);
    assert_eq!(code(&out), 6, "a known remote failure remains authoritative when stdout closes");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn luau_resource_paths_cannot_inject_resume_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("probe.luau");
    std::fs::write(&script, "return 42").unwrap();
    for path in [
        "universes/1/places/2/tasks/t --help",
        "universes/1/places/2/tasks/t;echo",
        "universes/1/places/2/tasks/t\n",
        "https://other.test/task",
    ] {
        let server = Server::new(vec![Response::json(json!({"path":path,"state":"QUEUED"}))]);
        let out = command(&server.origin)
            .args(["luau", "run", script.to_str().unwrap(), "--universe", "1", "--place", "2"])
            .output()
            .unwrap();
        assert_eq!(code(&out), 5, "unsafe task identity cannot become a recovery command");
        assert!(document(&out)["error"]["resume"].is_null());
        assert_eq!(server.requests().len(), 1);
        let out = command(&server.origin).args(["luau", "run", "--resume", path]).output().unwrap();
        assert_eq!(code(&out), 2, "resume arguments must be task resource paths");
        assert_eq!(server.requests().len(), 1);
    }
}

#[test]
fn unpollable_luau_acceptance_has_no_resume() {
    let server = Server::new(vec![Response::json(json!({"path":"universes/1/other/x"}))]);
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("script.luau");
    std::fs::write(&script, "return 42").unwrap();
    let out = command(&server.origin)
        .args(["luau", "run", script.to_str().unwrap(), "--universe", "1", "--place", "2"])
        .output()
        .unwrap();
    let error = &document(&out)["error"];
    assert_eq!(code(&out), 5, "an accepted task without a reader has an unknown outcome");
    assert_eq!(error["kind"], "outcome_unknown");
    assert!(error.get("resume").is_none(), "an unpollable path cannot produce a recovery command");
    assert_eq!(server.requests().len(), 1, "the accepted write is never repeated");
}

#[test]
fn polling_keeps_server_errors_separate_from_the_last_resource() {
    let first = json!({"path":"universes/1/memory-store/operations/x","done":false});
    let server =
        Server::new(vec![Response::json(first.clone()), Response::Body(404, json!({"code":"NOT_FOUND"}).to_string())]);
    let out = command(&server.origin).args(["call", "memory-store.flush", "universe_id=1"]).output().unwrap();
    let error = &document(&out)["error"];
    assert_eq!(code(&out), 8);
    assert_eq!(error["detail"], json!({"code":"NOT_FOUND"}), "the server body stays in detail");
    assert_eq!(error["last"], first, "the last resource is a separate field");
    assert_eq!(error["status"], 404);
    assert_eq!(error["resume"], "updraft call GET /cloud/v2/universes/1/memory-store/operations/x");

    let server = Server::new(vec![
        Response::json(json!({"path":"operations/x"})),
        Response::Body(403, json!({"message":"nope"}).to_string()),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("model.fbx");
    std::fs::write(&file, "fixture").unwrap();
    let out =
        command(&server.origin).args(["asset", "upload", file.to_str().unwrap(), "--group", "1"]).output().unwrap();
    let error = &document(&out)["error"];
    assert_eq!(code(&out), 8);
    assert_eq!(error["detail"], json!({"message":"nope"}));
    assert_eq!(error["last"], json!({"path":"operations/x","done":false}));
    assert_eq!(error["resume"], "updraft asset upload --resume x", "the flow owns its recovery command");
    assert_eq!(error["status"], 403);
    assert_eq!(error["keySource"], "UPDRAFT_API_KEY");
    assert!(error["requiredScopes"].is_array());
}

#[test]
fn every_percent_encoded_key_byte_is_redacted_in_both_cases() {
    let encoded: String = KEY.bytes().map(|b| format!("%{b:02X}")).collect();
    for body in [encoded.clone(), encoded.to_ascii_lowercase()] {
        let server = Server::new(vec![Response::Body(400, body)]);
        let out = command(&server.origin).args(["call", "assets.get", "assetId=42"]).output().unwrap();
        assert_eq!(code(&out), 4);
        assert_eq!(document(&out)["error"]["detail"], "<redacted>", "encoded letters are secret too");
    }
}

#[test]
fn cdn_failures_do_not_blame_the_selected_api_key() {
    for (status, expected, kind) in
        [(401, 4, "rejected"), (403, 4, "rejected"), (429, 7, "unavailable"), (503, 7, "unavailable")]
    {
        let mut replies = vec![Response::json(json!({"location":"$ORIGIN/file?signature=expired"}))];
        replies.extend((0..3).map(|_| Response::Body(status, "expired URL".into())));
        let server = Server::new(replies);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("download");
        let out = command(&server.origin)
            .args(["asset", "download", "42", "--out", path.to_str().unwrap()])
            .output()
            .unwrap();
        let error = &document(&out)["error"];
        assert_eq!(code(&out), expected, "CDN HTTP {status}");
        assert_eq!(error["kind"], kind);
        assert_eq!(error["status"], status);
        for field in ["keySource", "requiredScopes", "scopeMetadata", "profiles"] {
            assert!(error.get(field).is_none(), "a CDN error has no {field}");
        }
        assert!(!path.exists(), "failed downloads leave no output file");
        assert!(server.requests()[1..].iter().all(|r| !r.head.to_ascii_lowercase().contains("x-api-key:")));
    }
}

#[test]
fn download_retry_waits_end_at_the_deadline() {
    let server = httpmock::MockServer::start();
    server.mock(|w, t| {
        w.path("/asset-delivery-api/v1/assetId/42");
        t.json_body(json!({"location":format!("{}/file",server.base_url())}));
    });
    let download = server.mock(|w, t| {
        w.path("/file");
        t.status(503).header("retry-after", "2");
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("download");
    let start = std::time::Instant::now();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_updraft"))
        .env("UPDRAFT_ORIGIN", server.base_url())
        .env("UPDRAFT_API_KEY", KEY)
        .env("UPDRAFT_CONFIG", "nonexistent-updraft-test-config.toml")
        .env_remove("UPDRAFT_PROFILE")
        .args(["--deadline", "0.05", "asset", "download", "42", "--out", path.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(code(&out), 7, "{}", document(&out));
    assert!(start.elapsed() < std::time::Duration::from_secs(1), "the deadline bounds Retry-After sleeps");
    download.assert_calls(1);
    assert!(!path.exists());
}

#[test]
fn introspection_declares_an_empty_scope_requirement() {
    let server = Server::new(vec![Response::Body(401, "unauthorized".into())]);
    let out = command(&server.origin).args(["auth", "check"]).output().unwrap();
    assert_eq!(code(&out), 3);
    let error = &document(&out)["error"];
    assert_eq!(error["requiredScopes"], json!([]), "introspection needs no scopes");
    assert_eq!(error["scopeMetadata"], "declared");
}

#[test]
fn accepted_invalid_json_has_a_short_message_and_redacted_detail() {
    let server = Server::new(vec![Response::Body(200, format!("invalid response containing {KEY}"))]);
    let out = command(&server.origin).args(["call", "memory-store.flush", "universe_id=1"]).output().unwrap();
    assert_eq!(code(&out), 5);
    let error = &document(&out)["error"];
    assert_eq!(error["message"], "invalid JSON response");
    assert_eq!(error["detail"], "invalid response containing <redacted>");
    assert_eq!(server.requests().len(), 1);
}
