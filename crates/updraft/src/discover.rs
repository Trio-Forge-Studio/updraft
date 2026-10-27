use serde_json::{Value, json};
use updraft_registry::{Auth, Op, Registry};
pub fn describe(registry: &Registry, op: &Op) -> Value {
    fn expand(registry: &Registry, value: Value, depth: usize) -> Value {
        match value {
            Value::Object(mut o) => {
                if o.get("type") == Some(&json!("ref"))
                    && depth > 0
                    && let Some(name) = o["name"].as_str()
                    && let Some(shape) = registry.schemas.get(name)
                {
                    return expand(registry, serde_json::to_value(shape).expect("a Shape serializes"), depth - 1);
                }
                for v in o.values_mut() {
                    *v = expand(registry, v.take(), depth);
                }
                Value::Object(o)
            }
            Value::Array(a) => Value::Array(a.into_iter().map(|v| expand(registry, v, depth)).collect()),
            v => v,
        }
    }
    let mut doc = serde_json::to_value(op).expect("an Op serializes");
    doc["url"] = json!(format!("{}{}", op.server.origin(), op.path));
    let scopes = match &op.auth {
        Auth::ApiKey { scopes } => scopes.clone(),
        Auth::None => Some(vec![]),
    };
    doc["scopeMetadata"] = json!(if scopes.is_some() { "declared" } else { "unknown" });
    doc["requiredScopes"] = json!(scopes);
    if let Some(body) = doc.get_mut("body") {
        *body = expand(registry, body.take(), 2);
    }
    doc["response"] = expand(registry, doc["response"].take(), 2);
    let params = op.params.iter().filter(|p| p.required).map(|p| format!(" {}=<value>", p.name)).collect::<String>();
    doc["call"] = json!(format!(
        "updraft call {}{params}{}",
        op.name,
        if op.body.as_ref().is_some_and(|b| b.required) { " --body @<file>" } else { "" }
    ));
    doc
}
