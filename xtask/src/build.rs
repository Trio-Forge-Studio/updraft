use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;
use updraft_registry::path::{Template, fold};
use updraft_registry::*;

use crate::naming;
use crate::overlay::Overlay;

pub struct Input<'a> {
    pub spec: &'a Value,
    pub source: SpecSource,
    pub previous: Option<&'a Registry>,
    pub overlay: &'a Overlay,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub operations: usize,
    pub api_key_operations: usize,
    pub with_operation_id: usize,
    pub without_operation_id: usize,
    pub kept_names: usize,
    pub name_collisions: Vec<Collision>,
    pub unaliased_cloud_collections: BTreeSet<String>,
    pub ambiguous_templates: Vec<Vec<String>>,
    pub paginated: usize,
    pub long_running: usize,
    pub patched: usize,
    pub added: usize,
    pub unresolved_alternatives: Vec<String>,
    pub schemas: usize,
    pub duplicate_scope_operations: usize,
}

#[derive(Debug, Serialize)]
pub struct Collision {
    pub derived: String,
    pub assigned: Vec<(String, String)>,
}

pub fn build(input: Input<'_>) -> Result<(Registry, Report), Vec<String>> {
    let spec = input.spec;
    let mut report = Report::default();
    let mut outliner = Outliner { spec, reachable: BTreeSet::new() };
    let mut drafts = Vec::new();

    let paths = spec["paths"].as_object().into_iter().flatten();
    for (path, item) in paths {
        for (method_name, method) in METHODS {
            let Some(operation) = item.get(*method_name) else { continue };
            report.operations += 1;
            let accepts_key = operation["security"]
                .as_array()
                .is_some_and(|schemes| schemes.iter().any(|scheme| scheme.get("roblox-api-key").is_some()));
            if !accepts_key {
                continue;
            }
            if let Some(scopes) = operation["x-roblox-scopes"].as_array() {
                let names: BTreeSet<_> = scopes.iter().map(|scope| scope["name"].as_str()).collect();
                report.duplicate_scope_operations += usize::from(names.len() < scopes.len());
            }
            drafts.push(draft(&mut outliner, path, *method, item, operation));
        }
    }
    report.api_key_operations = drafts.len();
    report.with_operation_id = drafts.iter().filter(|op| op.operation_id.is_some()).count();
    report.without_operation_id = drafts.len() - report.with_operation_id;

    let mut errors = Vec::new();
    let upstream_keys: BTreeSet<_> = paths_from_spec(spec).collect();
    for op in &input.overlay.add {
        if upstream_keys.contains(&normalized_key(&op.key)) {
            errors.push(format!("upstream now has {}; turn the add into a patch or delete it", op.key));
        }
        drafts.push(op.clone());
        report.added += 1;
    }
    let mut keys = BTreeSet::new();
    for op in &drafts {
        if !keys.insert(&op.key) {
            errors.push(format!("duplicate key {}", op.key));
        }
    }
    errors.extend(input.overlay.apply(&mut drafts));
    report.patched = drafts.iter().filter(|op| !op.patches.is_empty()).count();

    naming::assign(&mut drafts, input.previous, &input.overlay.vocabulary, &mut report);
    errors.extend(naming::consistency(&drafts, &input.overlay.vocabulary));
    errors.extend(
        drafts
            .iter()
            .filter(|op| {
                op.method == Method::Post
                    && op.safety == Safety::Unsafe
                    && [op.name.clone(), naming::derive(op, &input.overlay.vocabulary)].iter().any(|name| {
                        name.split_once('.').is_some_and(|(_, action)| {
                            ["get-", "list-", "query-", "find-", "check-"].iter().any(|verb| action.starts_with(verb))
                        })
                    })
                    && !op.patches.iter().any(|note| note.starts_with("Unsafe read verb:"))
            })
            .map(|op| {
                format!("{}: unsafe POST with a read verb needs an Unsafe read verb: overlay explanation", op.key)
            }),
    );
    resolve_alternatives(spec, &mut drafts, &mut report);
    report.ambiguous_templates = ambiguous_templates(&drafts);
    errors.extend(check_param_folds(&drafts));

    let mut schemas = BTreeMap::new();
    let mut pending: Vec<String> = outliner.reachable.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        if schemas.contains_key(&name) {
            continue;
        }
        let before = outliner.reachable.clone();
        let shape = outliner.shape(&spec["components"]["schemas"][&name]);
        schemas.insert(name, shape);
        pending.extend(outliner.reachable.difference(&before).cloned());
    }
    report.schemas = schemas.len();

    drafts.sort_by(|a, b| a.name.cmp(&b.name));
    for pair in drafts.windows(2) {
        if pair[0].name == pair[1].name {
            errors.push(format!("duplicate name {}", pair[0].name));
        }
    }
    report.paginated = drafts.iter().filter(|op| op.pagination.is_some()).count();
    report.long_running = drafts.iter().filter(|op| op.completion.is_some()).count();
    errors.extend(completion_coverage(&drafts, &schemas));
    errors.extend(upstream_completion_coverage(spec, &drafts));

    let registry = Registry { source: input.source, ops: drafts, schemas };
    errors.extend(completion_readers(&registry));
    if errors.is_empty() { Ok((registry, report)) } else { Err(errors) }
}

fn completion_readers(registry: &Registry) -> Vec<String> {
    let mut errors = Vec::new();
    for from in registry.ops.iter().filter(|op| op.completion.is_some()) {
        let root = Template::parse(&from.path).root_len();
        let prefix: Vec<_> = from.path.split('/').skip(1).take(root).collect();
        let readers: Vec<_> = registry
            .ops
            .iter()
            .filter(|op| {
                op.method == Method::Get
                    && op.completion == from.completion
                    && op.path.split('/').skip(1).take(root).eq(prefix.iter().copied())
            })
            .collect();
        if readers.is_empty() {
            errors.push(format!("{} has no completion reader", from.key));
        }
        for reader in readers {
            let template = Template::parse(&reader.path);
            let path = template
                .without_root(template.root_len())
                .render(|_| Some("42".into()))
                .expect("reader parameters render");
            match registry.reader_of(from, &path) {
                Ok((found, _)) if found.key != reader.key => {
                    errors.push(format!("{} completion reader {} resolves to {}", from.key, reader.key, found.key))
                }
                Err(error) => errors.push(format!(
                    "{} completion reader {} must resolve exactly once: {error:?}",
                    from.key, reader.key
                )),
                _ => {}
            }
        }
    }
    errors
}

fn normalized_key(key: &str) -> String {
    let mut normalized = key.to_owned();
    for name in Template::parse(key).param_names() {
        normalized = normalized.replace(&format!("{{{name}}}"), "{}");
    }
    normalized
}

fn paths_from_spec(spec: &Value) -> impl Iterator<Item = String> + '_ {
    spec["paths"].as_object().into_iter().flatten().flat_map(|(path, item)| {
        METHODS.iter().filter_map(move |(method_name, method)| {
            item.get(*method_name).map(|_| normalized_key(&format!("{} {path}", method_label(*method))))
        })
    })
}

pub fn completion_coverage(ops: &[Op], schemas: &BTreeMap<String, Shape>) -> Vec<String> {
    ops.iter()
        .filter(|op| {
            let Payload::Json { shape: Shape::Ref { name } } = &op.response else { return false };
            let shaped = match schemas.get(name) {
                Some(Shape::Object { fields }) => {
                    fields.iter().any(|f| f.name == "done")
                        && fields.iter().any(|f| f.name == "response" || f.name == "error")
                }
                _ => false,
            };
            (name == "Operation" || name.ends_with(".Operation") || shaped)
                && op.completion.is_none()
                && !op.patches.iter().any(|note| note.starts_with("No completion:"))
        })
        .map(|op| format!("{} returns an Operation but has no completion or overlay explanation", op.key))
        .collect()
}

fn upstream_completion_coverage(spec: &Value, ops: &[Op]) -> Vec<String> {
    ops.iter()
        .filter(|op| op.completion.is_none() && !op.patches.iter().any(|note| note.starts_with("No completion:")))
        .filter(|op| {
            let method = method_label(op.method).to_ascii_lowercase();
            spec["paths"][&op.path][&method]["responses"]
                .as_object()
                .into_iter()
                .flatten()
                .filter(|(status, _)| status.starts_with('2'))
                .any(|(_, response)| {
                    deref(spec, response)["content"].as_object().into_iter().flatten().any(|(_, media)| {
                        let schema = &media["schema"];
                        let named = schema["$ref"]
                            .as_str()
                            .is_some_and(|name| name.ends_with("/Operation") || name.ends_with(".Operation"));
                        let fields = properties(spec, schema);
                        named
                            || (fields.contains_key("done")
                                && (fields.contains_key("response") || fields.contains_key("error")))
                    })
                })
        })
        .map(|op| format!("{} has an Operation success response but no completion or overlay explanation", op.key))
        .collect()
}

const METHODS: &[(&str, Method)] = &[
    ("get", Method::Get),
    ("post", Method::Post),
    ("put", Method::Put),
    ("patch", Method::Patch),
    ("delete", Method::Delete),
];

fn draft(outliner: &mut Outliner<'_>, path: &str, method: Method, item: &Value, operation: &Value) -> Op {
    let spec = outliner.spec;
    let mut params: Vec<Param> = Vec::new();
    for raw in item["parameters"].as_array().into_iter().chain(operation["parameters"].as_array()).flatten() {
        let raw = deref(spec, raw);
        let location = match raw["in"].as_str() {
            Some("path") => Location::Path,
            Some("query") => Location::Query,
            Some("header") => Location::Header,
            _ => continue,
        };
        let name = raw["name"].as_str().unwrap_or_default().to_owned();
        params.retain(|existing| !(existing.name == name && existing.location == location));
        params.push(Param {
            name,
            location,
            required: raw["required"].as_bool().unwrap_or(location == Location::Path),
            kind: value_kind(spec, &raw["schema"]),
            description: first_paragraph(raw["description"].as_str().unwrap_or_default()),
        });
    }

    let server = match operation["servers"][0]["url"].as_str().or(item["servers"][0]["url"].as_str()) {
        Some(url) if url.contains("thumbnails.roblox.com") => Server::Thumbnails,
        _ => Server::Apis,
    };
    let body = request_body(outliner, &operation["requestBody"]);
    let response = response_payload(outliner, &operation["responses"]);
    let response_schema = success_schema(spec, &operation["responses"]);
    let pagination = response_schema.and_then(|schema| detect_pagination(spec, &params, schema));
    let completion = response_schema
        .filter(|schema| {
            let fields = properties(spec, schema);
            operation.get("x-long-running-operation-parameters").is_some()
                || (fields.contains_key("done")
                    && (fields.contains_key("path") || fields.contains_key("response") || fields.contains_key("error")))
        })
        .map(|_| Completion::Operation);
    let scopes = operation["x-roblox-scopes"].as_array().map(|scopes| {
        let mut names = BTreeSet::new();
        scopes
            .iter()
            .filter(|scope| names.insert(scope["name"].as_str().unwrap_or_default()))
            .map(|scope| Scope {
                name: scope["name"].as_str().unwrap_or_default().to_owned(),
                target: scope["targetResourceSpecifier"].as_str().filter(|t| !t.is_empty()).map(str::to_owned),
            })
            .collect()
    });

    Op {
        name: String::new(),
        key: format!("{} {path}", method_label(method)),
        operation_id: operation["operationId"].as_str().map(str::to_owned),
        method,
        server,
        path: path.to_owned(),
        summary: first_paragraph(operation["summary"].as_str().unwrap_or_default()),
        description: first_paragraph(operation["description"].as_str().unwrap_or_default()),
        tags: operation["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tag| tag.as_str().map(str::to_owned))
            .collect(),
        params,
        body,
        response,
        auth: Auth::ApiKey { scopes },
        stability: match operation["x-roblox-stability"].as_str() {
            Some("STABLE") => Stability::Stable,
            Some("BETA") => Stability::Beta,
            Some("EXPERIMENTAL") => Stability::Experimental,
            _ => Stability::Unspecified,
        },
        legacy: path.trim_start_matches('/').starts_with("legacy-"),
        safety: match method {
            Method::Get => Safety::Read,
            Method::Put | Method::Delete => Safety::Idempotent,
            Method::Post | Method::Patch => Safety::Unsafe,
        },
        pagination,
        completion,
        retry: None,
        alternatives: Vec::new(),
        patches: Vec::new(),
    }
}

pub fn method_label(method: Method) -> &'static str {
    match method {
        Method::Get => "GET",
        Method::Post => "POST",
        Method::Put => "PUT",
        Method::Patch => "PATCH",
        Method::Delete => "DELETE",
    }
}

fn request_body(outliner: &mut Outliner<'_>, raw: &Value) -> Option<Body> {
    let raw = deref(outliner.spec, raw);
    let content = raw["content"].as_object()?;
    let required = raw["required"].as_bool().unwrap_or(false);
    if let Some(multipart) = content.get("multipart/form-data") {
        let schema = deref(outliner.spec, &multipart["schema"]);
        let required_parts: BTreeSet<&str> =
            schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let parts = properties(outliner.spec, schema)
            .into_iter()
            .map(|(name, part)| {
                let resolved = deref(outliner.spec, part);
                let is_file = resolved["format"] == "binary" || resolved["items"]["format"] == "binary";
                let kind = if is_file {
                    PartKind::File
                } else if part.get("$ref").is_some() || resolved["type"] == "object" {
                    PartKind::Json { shape: outliner.shape(part) }
                } else {
                    PartKind::Text
                };
                Part { required: required_parts.contains(name.as_str()), name, kind }
            })
            .collect();
        return Some(Body { required, media: BodyMedia::Multipart { parts } });
    }
    if let Some(binary) = content.keys().find(|media| media.as_str() == "application/octet-stream") {
        return Some(Body { required, media: BodyMedia::Binary { content_type: binary.clone() } });
    }
    let json = ["application/json", "text/json", "application/*+json", "application/json-patch+json"]
        .iter()
        .find_map(|media| content.get(*media).map(|value| (*media, value)));
    let (content_type, value) = match json {
        Some(found) => found,
        None => {
            let (media, _) = content.iter().next()?;
            return Some(Body { required, media: BodyMedia::Binary { content_type: media.clone() } });
        }
    };
    let content_type = if content_type == "application/*+json" { "application/json" } else { content_type };
    Some(Body {
        required,
        media: BodyMedia::Json { content_type: content_type.to_owned(), shape: outliner.shape(&value["schema"]) },
    })
}

fn success_entry<'a>(spec: &'a Value, responses: &'a Value) -> Option<&'a Value> {
    let responses = responses.as_object()?;
    let (_, entry) = responses
        .iter()
        .find(|(status, _)| status.starts_with('2'))
        .or_else(|| responses.iter().find(|(status, _)| *status == "default"))?;
    Some(deref(spec, entry))
}

fn success_schema<'a>(spec: &'a Value, responses: &'a Value) -> Option<&'a Value> {
    let content = success_entry(spec, responses)?["content"].as_object()?;
    ["application/json", "text/json"].iter().find_map(|media| content.get(*media)).map(|media| &media["schema"])
}

fn response_payload(outliner: &mut Outliner<'_>, responses: &Value) -> Payload {
    let Some(entry) = success_entry(outliner.spec, responses) else { return Payload::Empty };
    let Some(content) = entry["content"].as_object().filter(|content| !content.is_empty()) else {
        return Payload::Empty;
    };
    match success_schema(outliner.spec, responses) {
        Some(schema) => Payload::Json { shape: outliner.shape(schema) },
        None if content.contains_key("text/plain") => Payload::Text,
        None => Payload::Binary,
    }
}

const TOKEN_PARAMS: &[&str] = &["pageToken", "PageToken", "page_token", "cursor"];
const SIZE_PARAMS: &[&str] = &["maxPageSize", "MaxPageSize", "max_page_size", "pageSize", "limit"];
const NEXT_FIELDS: &[&str] = &["nextPageToken", "nextPageCursor", "nextCursor"];

fn detect_pagination(spec: &Value, params: &[Param], schema: &Value) -> Option<Pagination> {
    let query = |names: &[&str]| {
        params
            .iter()
            .find(|param| param.location == Location::Query && names.contains(&param.name.as_str()))
            .map(|param| param.name.clone())
    };
    let token_param = query(TOKEN_PARAMS)?;
    let fields = properties(spec, schema);
    let next_field = NEXT_FIELDS.iter().find(|name| fields.contains_key(**name))?.to_string();
    let arrays: Vec<&String> =
        fields.iter().filter(|(_, field)| deref(spec, field)["type"] == "array").map(|(name, _)| name).collect();
    let [items_field] = arrays.as_slice() else { return None };
    Some(Pagination { token_param, size_param: query(SIZE_PARAMS), next_field, items_field: (*items_field).clone() })
}

fn resolve_alternatives(spec: &Value, ops: &mut [Op], report: &mut Report) {
    let by_key: BTreeMap<String, String> = ops.iter().map(|op| (op.key.clone(), op.name.clone())).collect();
    let paths = spec["paths"].as_object().into_iter().flatten();
    let mut wanted: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (path, item) in paths {
        for (method_name, method) in METHODS {
            let Some(alternatives) = item[*method_name]["x-roblox-recommended-alternatives"].as_array() else {
                continue;
            };
            let key = format!("{} {path}", method_label(*method));
            if !by_key.contains_key(&key) {
                continue;
            }
            for alternative in alternatives {
                let url = alternative["url"].as_str().unwrap_or_default();
                let target = format!(
                    "{} {}",
                    alternative["httpMethod"].as_str().unwrap_or_default(),
                    url.trim_start_matches("https://apis.roblox.com")
                );
                match by_key.get(&target) {
                    Some(name) if target != key => wanted.entry(key.clone()).or_default().push(name.clone()),
                    Some(_) => {}
                    None => report.unresolved_alternatives.push(format!("{key} -> {target}")),
                }
            }
        }
    }
    for op in ops {
        if let Some(names) = wanted.remove(&op.key) {
            op.alternatives = names;
        }
    }
}

fn ambiguous_templates(ops: &[Op]) -> Vec<Vec<String>> {
    let mut shapes: BTreeMap<(Method, String), Vec<String>> = BTreeMap::new();
    for op in ops {
        let mut shape = op.path.clone();
        for name in Template::parse(&op.path).param_names() {
            shape = shape.replace(&format!("{{{name}}}"), "{}");
        }
        shapes.entry((op.method, shape)).or_default().push(op.name.clone());
    }
    shapes.into_values().filter(|names| names.len() > 1).collect()
}

fn check_param_folds(ops: &[Op]) -> Vec<String> {
    let mut errors = Vec::new();
    for op in ops {
        let mut seen = BTreeSet::new();
        for param in &op.params {
            if !seen.insert(fold(&param.name)) {
                errors.push(format!("{}: parameter {} folds onto another parameter", op.name, param.name));
            }
        }
    }
    errors
}

fn first_paragraph(text: &str) -> String {
    let paragraph = text.trim().split("\n\n").next().unwrap_or_default();
    paragraph.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn deref<'a>(spec: &'a Value, mut value: &'a Value) -> &'a Value {
    for _ in 0..32 {
        let Some(reference) = value["$ref"].as_str() else { break };
        value = reference.trim_start_matches("#/").split('/').fold(spec, |node, key| &node[key]);
    }
    value
}

fn properties<'a>(spec: &'a Value, schema: &'a Value) -> BTreeMap<String, &'a Value> {
    let schema = deref(spec, schema);
    let mut fields: BTreeMap<String, &Value> =
        schema["properties"].as_object().into_iter().flatten().map(|(name, value)| (name.clone(), value)).collect();
    for member in schema["allOf"].as_array().into_iter().flatten() {
        fields.extend(properties(spec, member));
    }
    fields
}

fn value_kind(spec: &Value, schema: &Value) -> ValueKind {
    let schema = deref(spec, schema);
    if let Some(values) = schema["enum"].as_array() {
        return ValueKind::Enum { values: values.iter().map(scalar_text).collect() };
    }
    match schema["type"].as_str() {
        Some("integer") => ValueKind::Integer,
        Some("number") => ValueKind::Number,
        Some("boolean") => ValueKind::Boolean,
        Some("array") => ValueKind::Array { items: Box::new(value_kind(spec, &schema["items"])) },
        _ => ValueKind::String,
    }
}

fn scalar_text(value: &Value) -> String {
    value.as_str().map_or_else(|| value.to_string(), str::to_owned)
}

struct Outliner<'a> {
    spec: &'a Value,
    reachable: BTreeSet<String>,
}

impl Outliner<'_> {
    fn shape(&mut self, schema: &Value) -> Shape {
        if let Some(reference) = schema["$ref"].as_str() {
            let name = reference.rsplit('/').next().unwrap_or_default().to_owned();
            self.reachable.insert(name.clone());
            return Shape::Ref { name };
        }
        if let Some(members) = schema["allOf"].as_array() {
            if let [only] = members.as_slice() {
                return self.shape(only);
            }
            return self.object(schema);
        }
        if let Some(variants) = schema["oneOf"].as_array().or(schema["anyOf"].as_array()) {
            return Shape::OneOf { variants: variants.iter().map(|variant| self.shape(variant)).collect() };
        }
        if let Some(values) = schema["enum"].as_array() {
            return Shape::Enum { values: values.iter().map(scalar_text).collect() };
        }
        match schema["type"].as_str() {
            Some("string") => Shape::String { format: schema["format"].as_str().map(str::to_owned) },
            Some("integer") => Shape::Integer,
            Some("number") => Shape::Number,
            Some("boolean") => Shape::Boolean,
            Some("array") => Shape::Array { items: Box::new(self.shape(&schema["items"])) },
            _ if schema.get("properties").is_some() => self.object(schema),
            _ if schema["additionalProperties"].is_object() => {
                Shape::Map { values: Box::new(self.shape(&schema["additionalProperties"])) }
            }
            _ => Shape::Any,
        }
    }

    fn object(&mut self, schema: &Value) -> Shape {
        let spec = self.spec;
        let mut required: BTreeSet<String> = BTreeSet::new();
        collect_required(spec, schema, &mut required);
        let fields = properties(spec, schema)
            .into_iter()
            .map(|(name, field)| Field {
                required: required.contains(&name),
                read_only: deref(spec, field)["readOnly"].as_bool().unwrap_or(false),
                description: first_paragraph(field["description"].as_str().unwrap_or_default()),
                shape: self.shape(field),
                name,
            })
            .collect();
        Shape::Object { fields }
    }
}

fn collect_required(spec: &Value, schema: &Value, into: &mut BTreeSet<String>) {
    let schema = deref(spec, schema);
    into.extend(schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned));
    for member in schema["allOf"].as_array().into_iter().flatten() {
        collect_required(spec, member, into);
    }
}
