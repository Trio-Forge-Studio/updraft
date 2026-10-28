use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use updraft_registry::{Body, Completion, Op, Pagination, Payload, RetryPolicy, Safety};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    #[serde(default)]
    pub vocabulary: BTreeMap<String, Vocabulary>,
    #[serde(default)]
    pub patch: Vec<Patch>,
    #[serde(default)]
    pub add: Vec<Op>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Vocabulary {
    #[serde(default)]
    pub drop: Vec<String>,
    #[serde(default)]
    pub rename: Vec<Rename>,
    #[serde(default)]
    pub exceptions: Vec<NameException>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NameException {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub key: String,
    pub note: String,
    pub expect: BTreeMap<String, Value>,
    pub name: Option<String>,
    pub body: Option<Body>,
    pub response: Option<Payload>,
    pub safety: Option<Safety>,
    pub completion: Option<Completion>,
    pub pagination: Option<Pagination>,
    pub retry: Option<RetryPolicy>,
}

impl Overlay {
    pub fn apply(&self, ops: &mut [Op]) -> Vec<String> {
        let mut errors = Vec::new();
        for patch in &self.patch {
            let Some(op) = ops.iter_mut().find(|op| op.key == patch.key) else {
                errors.push(format!("overlay patch matches no operation: {}", patch.key));
                continue;
            };
            let before = serde_json::to_value(&*op).expect("an Op serializes");
            let changes = [
                ("name", patch.name.is_some()),
                ("body", patch.body.is_some()),
                ("response", patch.response.is_some()),
                ("safety", patch.safety.is_some()),
                ("completion", patch.completion.is_some()),
                ("pagination", patch.pagination.is_some()),
                ("retry", patch.retry.is_some()),
            ];
            let mut mismatched = patch.note.trim().is_empty() || patch.expect.is_empty();
            for (field, changed) in changes {
                if changed && !patch.expect.contains_key(field) {
                    errors.push(format!("{}: missing expect.{field}", patch.key));
                    mismatched = true;
                }
            }
            for (field, expected) in &patch.expect {
                let actual = before.get(field).cloned().unwrap_or(Value::String("absent".into()));
                if &actual != expected {
                    errors.push(format!("{}: expect.{field} mismatch: expected {expected}, got {actual}", patch.key));
                    mismatched = true;
                }
            }
            if mismatched {
                errors.push(format!("{}: patch needs a note and matching expect guards", patch.key));
                continue;
            }
            if let Some(name) = &patch.name {
                op.name = name.clone();
            }
            if let Some(body) = &patch.body {
                op.body = Some(body.clone());
            }
            if let Some(response) = &patch.response {
                op.response = response.clone();
            }
            if let Some(safety) = patch.safety {
                op.safety = safety;
            }
            if let Some(completion) = &patch.completion {
                op.completion = Some(completion.clone());
            }
            if let Some(pagination) = &patch.pagination {
                op.pagination = Some(pagination.clone());
            }
            if let Some(retry) = &patch.retry {
                op.retry = Some(retry.clone());
            }
            op.patches.push(patch.note.clone());
        }
        errors
    }
}
