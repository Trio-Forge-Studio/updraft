use super::*;
use std::collections::BTreeSet;
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
    let expected: BTreeSet<_> = spec["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, item)| {
            ["get", "post", "put", "patch", "delete"].into_iter().filter_map(move |method| {
                let operation = item.get(method)?;
                operation["security"]
                    .as_array()?
                    .iter()
                    .any(|security| security.get("roblox-api-key").is_some())
                    .then(|| format!("{} {path}", method.to_ascii_uppercase()))
            })
        })
        .collect();
    let added: BTreeSet<_> = overlay.add.iter().map(|op| op.key.as_str()).collect();
    let included: BTreeSet<_> =
        actual.ops.iter().filter(|op| !added.contains(op.key.as_str())).map(|op| op.key.clone()).collect();
    assert_eq!(included, expected, "every API-key operation in the current spec enters the registry");
    assert_eq!(report.api_key_operations, expected.len());
    assert_eq!(actual.ops.len(), expected.len() + overlay.add.len());
    assert_eq!(actual, previous, "rebuilding preserves every field and name");
}

#[test]
fn added_operations_are_discoverable_without_renaming_existing_commands() {
    let (mut spec, overlay, previous) = inputs();
    let key = "GET /assets/v99/future-items/{itemId}";
    spec["paths"]["/assets/v99/future-items/{itemId}"] = json!({"get":{"security":[{"roblox-api-key":[]}],"parameters":[{"name":"itemId","in":"path","required":true,"schema":{"type":"string"}}],"responses":{"200":{"content":{"application/json":{"schema":{"type":"object"}}}}}}});
    let (actual, _) = build::build(build::Input {
        spec: &spec,
        overlay: &overlay,
        source: previous.source.clone(),
        previous: Some(&previous),
    })
    .unwrap();
    let added = actual.by_key(key).expect("a new API-key operation enters the registry");
    assert!(actual.find("future items", 10).iter().any(|hit| hit.name == added.name));
    for old in &previous.ops {
        assert_eq!(actual.by_key(&old.key).unwrap().name, old.name, "an unrelated addition preserves {}", old.name);
    }
    assert_eq!(diff::diff(&previous, &actual).added.len(), 1);
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
        assert!(r.reader_of(r.by_key(key).unwrap(), path).is_ok(), "{path} resolves through the shared matcher");
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
#[test]
fn every_concrete_url_resolves_except_the_documented_localization_pair() {
    let (_, _, r) = inputs();
    let mut resolved = 0;
    let mut ambiguous = 0;
    for op in &r.ops {
        let path = updraft_registry::path::Template::parse(&op.path).render(|_| Some("42".into())).unwrap();
        match r.resolve(&OpRef::Url { method: op.method, path, query: vec![] }) {
            Ok(found) => {
                assert_eq!(found.op.key, op.key, "the concrete URL selects {}", op.name);
                resolved += 1;
            }
            Err(ResolveError::Ambiguous { names }) => {
                assert!(
                    op.path.starts_with("/legacy-localization-tables/v1/localization-table/tables/"),
                    "unexpected ambiguity for {}: {names:?}",
                    op.name
                );
                ambiguous += 1;
            }
            Err(error) => panic!("{} must resolve: {error:?}", op.name),
        }
    }
    assert_eq!(ambiguous, 2);
    assert_eq!(resolved, r.ops.len() - 2);
}

#[test]
fn overlay_add_cannot_replace_an_upstream_key() {
    let (mut spec, overlay, previous) = inputs();
    spec["paths"]["/api-keys/v1/introspect"] = json!({"post":{"security":[{"roblox-api-key":[]}],"responses":{}}});
    let errors = build::build(build::Input {
        spec: &spec,
        source: previous.source.clone(),
        previous: Some(&previous),
        overlay: &overlay,
    })
    .unwrap_err();
    assert!(
        errors.iter().any(
            |e| e.contains("upstream now has POST /api-keys/v1/introspect; turn the add into a patch or delete it")
        ),
        "upstream additions require an explicit overlay decision: {errors:?}"
    );
}

#[test]
fn duplicate_overlay_keys_fail_the_build() {
    let (spec, mut overlay, previous) = inputs();
    let mut duplicate = overlay.add[0].clone();
    duplicate.name = "api-keys.another-name".into();
    overlay.add.push(duplicate);
    let errors = build::build(build::Input {
        spec: &spec,
        source: previous.source.clone(),
        previous: Some(&previous),
        overlay: &overlay,
    })
    .unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("duplicate key POST /api-keys/v1/introspect")),
        "distinct names cannot hide a duplicate identity: {errors:?}"
    );
}

#[test]
fn read_named_posts_are_safe_to_repeat() {
    let (_, _, registry) = inputs();
    for name in [
        "analytics-benchmark.query-universe-benchmarks",
        "game-internationalization.get-name-description-games-translation-history",
        "groups.get-groups-policy",
        "localization-tables.get-localization-table-table-entries-translation-history",
    ] {
        assert_eq!(registry.op(name).unwrap().safety, Safety::Read, "{name} only reads data");
    }
}

#[test]
fn unsafe_read_verbs_need_an_overlay_explanation() {
    for verb in ["get", "list", "query", "find", "check"] {
        let (mut spec, mut overlay, previous) = inputs();
        let path = format!("/future/v1/items:{verb}");
        spec["paths"][&path] = json!({"post":{"security":[{"roblox-api-key":[]}],"responses":{}}});
        let errors = build::build(build::Input {
            spec: &spec,
            overlay: &overlay,
            source: previous.source.clone(),
            previous: Some(&previous),
        })
        .unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("unsafe POST with a read verb")),
            "{verb} needs a safety decision: {errors:?}"
        );
        let patch: overlay::Overlay = toml::from_str(&format!(
            r#"
[[patch]]
key = "POST {path}"
expect = {{ safety = "unsafe" }}
safety = "unsafe"
note = "Unsafe read verb: this query also records a billable event."
"#
        ))
        .unwrap();
        overlay.patch.extend(patch.patch);
        assert!(
            build::build(build::Input {
                spec: &spec,
                overlay: &overlay,
                source: previous.source.clone(),
                previous: Some(&previous),
            })
            .is_ok(),
            "an explicit side-effect explanation permits {verb}"
        );
    }
}

#[test]
fn overlay_key_guards_ignore_parameter_spelling() {
    let (mut spec, overlay, previous) = inputs();
    spec["paths"]["/universes/v1/places/{place_id}/universe"] = json!({"get":{"responses":{}}});
    let errors = build::build(build::Input {
        spec: &spec,
        overlay: &overlay,
        source: previous.source.clone(),
        previous: Some(&previous),
    })
    .unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("upstream now has GET /universes/v1/places/{placeId}/universe")),
        "an upstream addition matches regardless of parameter spelling: {errors:?}"
    );
}

#[test]
fn completion_reader_must_resolve_to_itself() {
    let (mut spec, overlay, previous) = inputs();
    let reader = spec["paths"]["/assets/v1/operations/{operationId}"].clone();
    spec["paths"]["/assets/v1/operations/42"] = reader;
    let errors = build::build(build::Input {
        spec: &spec,
        overlay: &overlay,
        source: previous.source.clone(),
        previous: Some(&previous),
    })
    .unwrap_err();
    assert!(
        errors.iter().any(|e| e.contains("resolves to GET /assets/v1/operations/42")),
        "a more specific reader cannot stand in for the declared reader: {errors:?}"
    );
}

#[test]
fn resource_names_use_complete_singular_nouns() {
    let (_, _, registry) = inputs();
    for (plural, singular) in [
        ("regexes", "regex"),
        ("boxes", "box"),
        ("statuses", "status"),
        ("aliases", "alias"),
        ("cases", "case"),
        ("houses", "house"),
        ("courses", "course"),
        ("universes", "universe"),
        ("game-passes", "game-pass"),
        ("entries", "entry"),
        ("policies", "policy"),
        ("repositories", "repository"),
        ("patches", "patch"),
        ("forum-categories", "forum-category"),
    ] {
        let mut op = registry.op("assets.get-asset").unwrap().clone();
        op.path = format!("/future/v1/{plural}/{{id}}");
        assert_eq!(
            naming::derive(&op),
            format!("future.get-{singular}"),
            "{plural} must retain its complete singular noun"
        );
    }
}

#[test]
fn reader_reports_ties_instead_of_choosing_one() {
    let (_, _, mut r) = inputs();
    let original = r.op("assets.get-operation").unwrap().clone();
    let mut duplicate = original.clone();
    duplicate.name = "assets.other-reader".into();
    duplicate.key = "GET /assets/v1/operations/{anotherId}".into();
    duplicate.path = "/assets/v1/operations/{anotherId}".into();
    r.ops.push(duplicate);
    assert!(
        matches!(r.reader_of(&original, "operations/x"), Err(ResolveError::Ambiguous { .. })),
        "a tied reader must be reported as ambiguous"
    );
}

#[test]
fn completion_reader_build_check_rejects_a_tied_template() {
    let (mut spec, overlay, previous) = inputs();
    let reader = spec["paths"]["/assets/v1/operations/{operationId}"].clone();
    spec["paths"]["/assets/v1/operations/{otherId}"] = reader;
    let result = build::build(build::Input {
        spec: &spec,
        source: previous.source.clone(),
        previous: Some(&previous),
        overlay: &overlay,
    });
    assert!(result.is_err(), "the build must reject an ambiguous completion reader");
}

#[test]
fn operation_shaped_gets_and_luau_readers_have_completions() {
    let (mut spec, overlay, previous) = inputs();
    spec["paths"]["/assets/v99/operations/{id}"] = json!({"get":{"security":[{"roblox-api-key":[]}],"parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"string"}}],"responses":{"200":{"content":{"application/json":{"schema":{"type":"object","properties":{"done":{"type":"boolean"},"response":{"type":"object"}}}}}}}}});
    let result = build::build(build::Input {
        spec: &spec,
        source: previous.source.clone(),
        previous: Some(&previous),
        overlay: &overlay,
    });
    assert!(result.is_ok(), "Operation-shaped GETs get completion data even without a path field");
    let (r, _) = result.unwrap();
    assert_eq!(r.by_key("GET /assets/v99/operations/{id}").unwrap().completion, Some(Completion::Operation));
    assert!(
        matches!(
            r.op("cloud.get-universe-place-version-luau-execution-session-task").unwrap().completion,
            Some(Completion::State { .. })
        ),
        "Luau resume readers must wait for a terminal state"
    );
}

#[test]
fn semantic_names_match_operation_intent_before_the_first_release() {
    let (spec, overlay, previous) = inputs();
    let (r, _) = build::build(build::Input {
        spec: &spec,
        source: previous.source.clone(),
        previous: Some(&previous),
        overlay: &overlay,
    })
    .unwrap();
    for line in include_str!("../fixtures/name-changes.tsv").lines() {
        let (old, new) = line.split_once('\t').unwrap();
        assert!(r.op(new).is_some(), "the operation must describe its intent as {new}");
        assert!(r.op(old).is_none(), "the misleading pre-release name {old} must disappear");
    }
}

#[test]
fn body_key_introspection_declares_no_required_scopes() {
    let (_, _, registry) = inputs();
    assert_eq!(
        registry.op("api-keys.introspect").unwrap().auth.required_scopes(),
        Some([].as_slice()),
        "introspection requires no scope"
    );
    assert_eq!(Auth::ApiKey { scopes: None }.required_scopes(), None, "missing API-key metadata stays unknown");
}
