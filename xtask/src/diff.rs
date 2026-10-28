use std::collections::BTreeMap;

use serde::Serialize;
use updraft_registry::{Op, Registry};

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct Diff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<Changed>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Changed {
    pub name: String,
    pub fields: Vec<&'static str>,
}

pub fn diff(old: &Registry, new: &Registry) -> Diff {
    let old_ops: BTreeMap<&str, &Op> = old.ops.iter().map(|op| (op.key.as_str(), op)).collect();
    let new_ops: BTreeMap<&str, &Op> = new.ops.iter().map(|op| (op.key.as_str(), op)).collect();
    let mut diff = Diff::default();
    for (key, op) in &new_ops {
        match old_ops.get(key) {
            None => diff.added.push(format!("{} ({key})", op.name)),
            Some(before) => {
                let fields = changed_fields(before, op, old, new);
                if !fields.is_empty() {
                    diff.changed.push(Changed { name: op.name.clone(), fields });
                }
            }
        }
    }
    for (key, op) in &old_ops {
        if !new_ops.contains_key(key) {
            diff.removed.push(format!("{} ({key})", op.name));
        }
    }
    diff
}

fn changed_fields(a: &Op, b: &Op, old: &Registry, new: &Registry) -> Vec<&'static str> {
    let mut fields = Vec::new();
    let mut check = |name, same: bool| {
        if !same {
            fields.push(name);
        }
    };
    check("name", a.name == b.name);
    check("server", a.server == b.server);
    check("operationId", a.operation_id == b.operation_id);
    check(
        "params",
        a.params
            .iter()
            .map(|p| (&p.name, p.location, p.required, &p.kind))
            .eq(b.params.iter().map(|p| (&p.name, p.location, p.required, &p.kind))),
    );
    check("body", a.body == b.body);
    check("response", a.response == b.response);
    check("auth", a.auth == b.auth);
    check("stability", a.stability == b.stability);
    check("safety", a.safety == b.safety);
    check("pagination", a.pagination == b.pagination);
    check("completion", a.completion == b.completion);
    check("retry", a.retry == b.retry);
    check("schema", reachable(a, old) == reachable(b, new));
    fields
}

fn reachable(op: &Op, registry: &Registry) -> BTreeMap<String, updraft_registry::Shape> {
    fn visit(value: &serde_json::Value, registry: &Registry, found: &mut BTreeMap<String, updraft_registry::Shape>) {
        match value {
            serde_json::Value::Object(object) => {
                if object.get("type").and_then(|v| v.as_str()) == Some("ref")
                    && let Some(name) = object.get("name").and_then(|v| v.as_str())
                    && !found.contains_key(name)
                    && let Some(shape) = registry.schemas.get(name)
                {
                    found.insert(name.into(), shape.clone());
                    visit(&serde_json::to_value(shape).expect("a Shape serializes"), registry, found);
                }
                for child in object.values() {
                    visit(child, registry, found);
                }
            }
            serde_json::Value::Array(array) => {
                for child in array {
                    visit(child, registry, found);
                }
            }
            _ => {}
        }
    }
    let mut found = BTreeMap::new();
    visit(&serde_json::json!([op.body, op.response, op.params]), registry, &mut found);
    found
}
