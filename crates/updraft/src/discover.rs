use serde_json::{Value, json};
use updraft_registry::{Auth, BodyMedia, Op, Payload, Registry};

fn outline(registry: &Registry, value: Value, depth: usize) -> Value {
    match value {
        Value::Object(mut object) => {
            if object.get("type") == Some(&json!("ref")) {
                let name = object["name"].as_str().unwrap_or_default();
                if depth > 0
                    && let Some(shape) = registry.schemas.get(name)
                {
                    return outline(registry, serde_json::to_value(shape).expect("a Shape serializes"), depth - 1);
                }
                return json!({"ref":name});
            }
            if let Some(shape) = object.remove("shape").or_else(|| object.remove("kind")) {
                let mut shape = outline(registry, shape, depth);
                if let Some(fields) = shape.as_object_mut() {
                    fields.extend(object.into_iter().map(|(k, v)| (camel(&k), outline(registry, v, depth))));
                    return shape;
                }
            }
            Value::Object(object.into_iter().map(|(k, v)| (camel(&k), outline(registry, v, depth))).collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(|v| outline(registry, v, depth)).collect()),
        value => value,
    }
}
fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in name.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}
pub fn describe(registry: &Registry, op: &Op) -> Value {
    let mut doc = json!({"name":op.name,"method":op.method,"url":format!("{}{}",op.server.origin(),op.path),"path":op.path,"operationId":op.operation_id,"summary":op.summary,"description":op.description,"params":outline(registry,json!(op.params),2),"safety":op.safety,"legacy":op.legacy,"stability":op.stability,"patches":op.patches,"alternatives":op.alternatives});
    doc.as_object_mut()
        .expect("description is an object")
        .extend(crate::auth::scope_metadata(op).as_object().expect("scope metadata is an object").clone());
    doc["body"] = match &op.body {
        None => Value::Null,
        Some(body) => {
            let mut media = match &body.media {
                BodyMedia::Json { content_type, shape } => {
                    json!({"type":"json","contentType":content_type,"schema":outline(registry,json!(shape),2)})
                }
                BodyMedia::Binary { content_type } => json!({"type":"binary","contentType":content_type}),
                BodyMedia::Multipart { parts } => json!({"type":"multipart","parts":outline(registry,json!(parts),2)}),
            };
            media["required"] = json!(body.required);
            media
        }
    };
    if let Auth::BodyKey { field } = &op.auth {
        doc["body"]["credentialField"] = json!(field);
        doc["body"]["suppliedBy"] = json!("selectedKey");
    }
    doc["response"] = match &op.response {
        Payload::Json { shape } => outline(registry, json!(shape), 2),
        other => json!(other),
    };
    doc["retry"] = outline(registry, json!(op.retry), 2);
    doc["pagination"] = outline(registry, json!(op.pagination), 2);
    doc["completion"] = json!(op.completion);
    let params = op
        .params
        .iter()
        .filter(|p| p.required)
        .map(|p| {
            format!(
                " {}=<{}>",
                p.name,
                match p.kind {
                    updraft_registry::ValueKind::Integer => "integer",
                    updraft_registry::ValueKind::Number => "number",
                    updraft_registry::ValueKind::Boolean => "boolean",
                    _ => "string",
                }
            )
        })
        .collect::<String>();
    let body = match op.body.as_ref().map(|body| &body.media) {
        _ if matches!(op.auth, Auth::BodyKey { .. }) => String::new(),
        Some(BodyMedia::Multipart { parts }) => {
            parts.iter().map(|part| format!(" --part {}=@<file>", part.name)).collect()
        }
        Some(_) => " --body @<file>".into(),
        None => String::new(),
    };
    doc["call"] = json!(format!("updraft call {}{params}{body}", op.name));
    doc
}
