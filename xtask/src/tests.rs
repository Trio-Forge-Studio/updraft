use super::*;
use updraft_registry::*;

fn inputs() -> (Value, overlay::Overlay, Registry) {
    (
        serde_json::from_str(include_str!("../../spec/openapi.json")).unwrap(),
        toml::from_str(include_str!("../../spec/overlay.toml")).unwrap(),
        serde_json::from_str(include_str!("../../spec/registry.json")).unwrap(),
    )
}

#[test]
fn rebuild_is_a_noop_and_covers_every_api_key_operation() {
    let (spec, overlay, previous) = inputs();
    let (actual, report) = build::build(build::Input {
        spec: &spec,
        source: previous.source.clone(),
        previous: Some(&previous),
        overlay: &overlay,
    })
    .unwrap();
    assert_eq!(report.api_key_operations, 299, "all API-key operations enter the registry");
    assert_eq!(actual.ops.len(), 301, "the overlay adds two operations");
    assert_eq!(actual, previous, "rebuilding preserves every field and name");
}

#[test]
fn missing_scopes_remain_unknown() {
    let (mut spec, overlay, previous) = inputs();
    spec["paths"]["/assets/v1/assets/{assetId}"]["get"].as_object_mut().unwrap().remove("x-roblox-scopes");
    let (actual, _) = build::build(build::Input {
        spec: &spec,
        source: previous.source.clone(),
        previous: Some(&previous),
        overlay: &overlay,
    })
    .unwrap();
    assert_eq!(actual.op("assets.get-asset").unwrap().auth, Auth::ApiKey { scopes: None });
}

#[test]
fn stale_overlay_expectations_fail_build() {
    let (mut spec, overlay, previous) = inputs();
    spec["paths"]["/universes/v1/{universeId}/places/{placeId}/versions"]["post"]["requestBody"] = serde_json::json!({"required":true,"content":{"application/octet-stream":{"schema":{"type":"string","format":"binary"}}}});
    let errors = build::build(build::Input {
        spec: &spec,
        source: previous.source.clone(),
        previous: Some(&previous),
        overlay: &overlay,
    })
    .unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("expect.body mismatch")),
        "a real upstream body invalidates the correction"
    );
}

#[test]
fn operation_response_coverage_detects_a_missing_completion() {
    let (_, _, mut registry) = inputs();
    assert!(build::completion_coverage(&registry.ops, &registry.schemas).is_empty());
    let op = registry.ops.iter_mut().find(|op| op.key == "POST /assets/v1/assets").unwrap();
    op.completion = None;
    assert_eq!(build::completion_coverage(&registry.ops, &registry.schemas).len(), 1);
}

#[test]
fn naming_survives_path_moves_and_new_collisions() {
    let (_, _, previous) = inputs();
    let mut ops = vec![previous.op("assets.get-asset").unwrap().clone()];
    ops[0].name.clear();
    ops[0].path = "/assets/v2/assets/{assetId}".into();
    ops[0].key = "GET /assets/v2/assets/{assetId}".into();
    naming::assign(&mut ops, Some(&previous), &mut build::Report::default());
    assert_eq!(ops[0].name, "assets.get-asset", "an operationId preserves the command after a move");
    assert_eq!(naming::derive(previous.op("assets.get-asset").unwrap()), "assets.get-asset");
    assert_eq!(
        naming::derive(previous.op("universes.create-place-version").unwrap()),
        "universes.create-place-version"
    );
}

#[test]
fn matcher_handles_urls_polling_and_ambiguous_templates() {
    let (_, _, r) = inputs();
    let resolved =
        r.resolve(&OpRef::Url { method: Method::Get, path: "/assets/v1/assets/42".into(), query: vec![] }).unwrap();
    assert_eq!(resolved.op.name, "assets.get-asset");
    for (key, path) in [
        ("POST /assets/v1/assets", "operations/abc"),
        ("POST /cloud/v2/universes/{universe_id}/memory-store:flush", "universes/1/memory-store/operations/x"),
        (
            "POST /cloud/v2/universes/{universe_id}/places/{place_id}/luau-execution-session-tasks",
            "universes/1/places/2/versions/3/luau-execution-sessions/s/tasks/t",
        ),
    ] {
        assert!(r.reader_of(r.by_key(key).unwrap(), path).is_some(), "{path} resolves through the shared matcher");
    }
    assert!(matches!(
        r.resolve(&OpRef::Url {
            method: Method::Get,
            path: "/legacy-localization-tables/v1/localization-table/tables/7".into(),
            query: vec![]
        }),
        Err(ResolveError::Ambiguous { .. })
    ));
}

#[test]
fn detection_pagination_and_search_use_registry_data() {
    let (_, _, r) = inputs();
    assert!(r.ops.iter().filter(|o| o.pagination.is_some()).count() >= 38);
    assert!(r.ops.iter().filter(|o| o.completion.is_some()).count() >= 18);
    let hits = r.find("luau logs", 5);
    assert!(!hits.is_empty());
    assert!(hits.iter().all(|h| h.path.contains("luau") && h.path.contains("logs")));
    assert!(
        matches!(r.resolve(&OpRef::Name("assets.get-asse".into())), Err(ResolveError::Unknown { suggestions }) if suggestions.contains(&"assets.get-asset".into()))
    );
}

#[test]
fn diff_reports_reachable_schema_changes() {
    let (_, _, old) = inputs();
    let mut new = old.clone();
    new.schemas.insert("Asset".into(), Shape::Any);
    let changes = diff::diff(&old, &new);
    assert!(changes.changed.iter().any(|c| c.name == "assets.get-asset" && c.fields.contains(&"schema")));
}

#[test]
fn reused_operation_id_cannot_take_a_surviving_keys_name() {
    let (_, _, previous) = inputs();
    let survivor = previous.op("assets.get-asset").unwrap().clone();
    let mut added = survivor.clone();
    added.key = "GET /assets/v2/assets/{assetId}".into();
    added.path = "/assets/v2/assets/{assetId}".into();
    let mut ops = vec![added, survivor.clone()];
    for op in &mut ops {
        op.name.clear();
    }
    naming::assign(&mut ops, Some(&previous), &mut build::Report::default());
    assert_eq!(
        ops[1].name, survivor.name,
        "a surviving key keeps its name even if another path reuses its operationId"
    );
    assert_ne!(ops[0].name, survivor.name);
}

#[test]
fn coverage_checks_every_success_response() {
    let (mut spec, overlay, previous) = inputs();
    spec["paths"]["/assets/v1/assets/{assetId}"]["get"]["responses"]["201"] =
        json!({"content":{"application/json":{"schema":{"$ref":"#/components/schemas/Operation"}}}});
    let errors = build::build(build::Input {
        spec: &spec,
        overlay: &overlay,
        source: previous.source.clone(),
        previous: Some(&previous),
    })
    .unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("Operation success response")),
        "a secondary 2xx response cannot evade completion coverage"
    );
}

#[test]
fn diff_reports_command_identity_and_server_changes() {
    let (_, _, old) = inputs();
    let mut new = old.clone();
    let op = new.ops.iter_mut().find(|op| op.name == "assets.get-asset").unwrap();
    op.name = "assets.renamed".into();
    op.server = Server::Thumbnails;
    op.operation_id = Some("NewIdentity".into());
    let changes = diff::diff(&old, &new);
    assert!(
        changes.changed.iter().any(|op| op.fields.contains(&"name")
            && op.fields.contains(&"server")
            && op.fields.contains(&"operationId")),
        "spec updates report changes to routing and command identity"
    );
}
