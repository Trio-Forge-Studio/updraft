use serde_json::{Value, json};
use std::process::{Command, Output};
fn command() -> Command {
    let key = std::env::var("ROBLOX_ASSETS_API_KEY").expect("ROBLOX_ASSETS_API_KEY is required for live tests");
    assert!(!key.trim().is_empty(), "the live key must be nonempty");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_updraft"));
    cmd.env("UPDRAFT_API_KEY", key).env_remove("UPDRAFT_PROFILE").env_remove("UPDRAFT_ORIGIN").env(
        "UPDRAFT_CONFIG",
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/no-live-config.toml"),
    );
    cmd
}
fn document(out: &Output) -> Value {
    let key = std::env::var("ROBLOX_ASSETS_API_KEY").unwrap();
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&key), "stdout must redact the live key");
    assert!(!String::from_utf8_lossy(&out.stderr).contains(&key), "stderr must redact the live key");
    serde_json::from_slice(&out.stdout).expect("live command returns one JSON document")
}
fn success(out: Output) -> Value {
    let d = document(&out);
    assert!(out.status.success(), "live command failed: {d}");
    d
}
#[test]
#[ignore = "requires a live Assets API key; read-only"]
fn read_only_auth_check() {
    let d = success(command().args(["auth", "check"]).output().unwrap());
    assert_eq!(d["enabled"], true);
    assert_eq!(d["expired"], false);
    assert!(d["scopes"].as_array().unwrap().iter().any(|s| s == "asset:read"));
}
#[test]
#[ignore = "requires a live Assets API key; read-only"]
fn read_only_get_asset() {
    let asset = std::env::var("UPDRAFT_LIVE_ASSET_ID").unwrap_or_else(|_| "6444320592".into());
    let d = success(
        command()
            .args(["call", "assets.get", &format!("assetId={asset}"), "readMask=assetType,displayName,creationContext"])
            .output()
            .unwrap(),
    );
    assert!(d["assetType"].is_string());
    assert!(d["displayName"].is_string());
}
#[test]
#[ignore = "public place lookup; read-only"]
fn read_only_get_place_universe() {
    let place = std::env::var("UPDRAFT_LIVE_PLACE_ID").unwrap_or_else(|_| "132306245523809".into());
    let d = success(
        command().args(["call", "universes.get-place-universe", &format!("placeId={place}")]).output().unwrap(),
    );
    assert!(d["universeId"].as_u64().is_some_and(|id| id > 0));
}
#[test]
#[ignore = "creates and archives one group decal; explicit user approval required"]
fn upload_then_archive_requires_explicit_approval() {
    assert_eq!(
        std::env::var("UPDRAFT_ALLOW_LIVE_UPLOAD").as_deref(),
        Ok("1"),
        "obtain the user's approval before enabling the live upload test"
    );
    let group = std::env::var("ROBLOX_GROUP_ID").expect("ROBLOX_GROUP_ID is required");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("updraft-smoke.png");
    std::fs::write(&path, include_bytes!("fixtures/pixel.png")).unwrap();
    let request = json!({"assetType":"Decal","displayName":"updraft smoke cleanup required","description":"Temporary updraft live smoke fixture.","creationContext":{"creator":{"groupId":group}}});
    let created = command()
        .args([
            "call",
            "assets.create",
            "--no-wait",
            "--part",
            &format!("request={request}"),
            "--part",
            &format!("fileContent=@{}", path.display()),
        ])
        .output()
        .unwrap();
    let first = document(&created);
    assert!(
        created.status.success(),
        "upload failed; if outcome_unknown, check the group's Creations before uploading again: {first}"
    );
    let operation = first["path"]
        .as_str()
        .or_else(|| first["operationId"].as_str())
        .expect("no operation ID; check the group's Creations before uploading again")
        .to_owned();
    let receipt_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/live-upload-receipt.json");
    let mut receipt = json!({"operation":operation,"assetId":null,"archived":false});
    let mut receipt_error = std::fs::create_dir_all(receipt_path.parent().unwrap())
        .and_then(|()| std::fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()))
        .err();
    let mut asset = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let resumed = success(command().args(["asset", "upload", "--resume", &operation]).output().unwrap());
        asset = resumed["assetId"].as_str().map(str::to_owned);
        let id = asset.as_deref().expect("resume must return an asset ID");
        receipt["assetId"] = json!(id);
        let updated = std::fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).err();
        receipt_error = receipt_error.take().or(updated);
        let d = success(
            command()
                .args(["call", "assets.get", &format!("assetId={id}"), "readMask=assetType,creationContext"])
                .output()
                .unwrap(),
        );
        assert_eq!(d["assetType"], "Decal");
        assert_eq!(d["creationContext"]["creator"]["groupId"], group);
    }));
    if asset.is_none() {
        let out = command()
            .args(["asset", "upload", "--resume", &operation])
            .output()
            .unwrap_or_else(|_| panic!("cleanup cannot poll operation {operation}; check the group's Creations"));
        let d = document(&out);
        if out.status.success() {
            asset = d["assetId"].as_str().map(str::to_owned);
        }
    }
    let id = asset.expect(
        "cleanup cannot resolve the pending operation; check the group's Creations and build/live-upload-receipt.json",
    );
    receipt["assetId"] = json!(id);
    let archive = command()
        .args(["call", "assets.archive", &format!("assetId={id}")])
        .output()
        .unwrap_or_else(|_| panic!("cleanup cannot start for asset {id}; archive it manually"));
    let archived = document(&archive);
    assert!(archive.status.success(), "cleanup failed for asset {id}; archive it manually: {archived}");
    let verify = command()
        .args(["call", "assets.get", &format!("assetId={id}"), "readMask=state"])
        .output()
        .unwrap_or_else(|_| panic!("cleanup cannot verify asset {id}; check its archive state manually"));
    let verified = document(&verify);
    assert!(
        verify.status.success(),
        "cleanup verification failed for asset {id}; check and archive it manually: {verified}"
    );
    assert_eq!(
        verified["state"].as_str().unwrap_or_default().to_ascii_uppercase(),
        "ARCHIVED",
        "asset {id} cleanup must be verified"
    );
    receipt["archived"] = json!(true);
    std::fs::write(receipt_path, serde_json::to_vec_pretty(&receipt).unwrap())
        .unwrap_or_else(|_| panic!("asset {id} was archived but its receipt could not be written"));
    assert!(receipt_error.is_none(), "the initial operation receipt could not be written; the asset was archived");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
