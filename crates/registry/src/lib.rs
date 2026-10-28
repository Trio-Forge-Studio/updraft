pub mod model;
pub mod path;

pub use model::*;
use path::{Captures, Template};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpRef {
    Name(String),
    Url { method: Method, path: String, query: Vec<(String, String)> },
}

#[derive(Debug)]
pub struct Resolved<'r> {
    pub op: &'r Op,
    pub preset: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    Unknown { suggestions: Vec<String> },
    Ambiguous { names: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Hit {
    pub name: String,
    pub method: Method,
    pub path: String,
    pub summary: String,
    pub score: u32,
}

impl Registry {
    pub fn op(&self, name: &str) -> Option<&Op> {
        self.ops.binary_search_by(|op| op.name.as_str().cmp(name)).ok().map(|index| &self.ops[index])
    }

    pub fn by_key(&self, key: &str) -> Option<&Op> {
        self.ops.iter().find(|op| op.key == key)
    }

    pub fn resolve(&self, reference: &OpRef) -> Result<Resolved<'_>, ResolveError> {
        match reference {
            OpRef::Name(name) => self
                .op(name)
                .map(|op| Resolved { op, preset: Vec::new() })
                .ok_or_else(|| ResolveError::Unknown { suggestions: self.suggest(name) }),
            OpRef::Url { method, path, query } => {
                let mut best: Vec<(&Op, Captures, (usize, usize))> = Vec::new();
                for op in self.ops.iter().filter(|op| op.method == *method) {
                    let template = Template::parse(&op.path);
                    if let Some(captures) = template.captures(path) {
                        best.push((op, captures, template.specificity()));
                    }
                }
                let Some(top) = best.iter().map(|(_, _, specificity)| *specificity).max() else {
                    return Err(ResolveError::Unknown { suggestions: Vec::new() });
                };
                best.retain(|(_, _, specificity)| *specificity == top);
                match best.as_slice() {
                    [(op, captures, _)] => {
                        Ok(Resolved { op, preset: captures.0.iter().cloned().chain(query.iter().cloned()).collect() })
                    }
                    _ => {
                        Err(ResolveError::Ambiguous { names: best.iter().map(|(op, _, _)| op.name.clone()).collect() })
                    }
                }
            }
        }
    }

    pub fn reader_of(&self, from: &Op, resource_path: &str) -> Option<(&Op, Captures)> {
        let root = Template::parse(&from.path);
        let root_prefix: Vec<&str> = from.path.trim_matches('/').split('/').take(root.root_len()).collect();
        self.ops
            .iter()
            .filter(|op| op.method == Method::Get)
            .filter(|op| op.path.trim_matches('/').split('/').take(root_prefix.len()).eq(root_prefix.iter().copied()))
            .filter_map(|op| {
                let template = Template::parse(&op.path);
                let relative = template.without_root(template.root_len());
                relative.captures(resource_path).map(|captures| (op, captures, relative.specificity()))
            })
            .max_by_key(|(_, _, specificity)| *specificity)
            .map(|(op, captures, _)| (op, captures))
    }

    pub fn find(&self, query: &str, limit: usize) -> Vec<Hit> {
        let query = tokens(query);
        let mut hits: Vec<Hit> = self
            .ops
            .iter()
            .filter_map(|op| {
                let scopes = match &op.auth {
                    Auth::ApiKey { scopes: Some(scopes) } => {
                        scopes.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(" ")
                    }
                    _ => String::new(),
                };
                let fields = [
                    (tokens(&op.name), 8),
                    (tokens(&op.path), 6),
                    (tokens(op.operation_id.as_deref().unwrap_or_default()), 5),
                    (tokens(&scopes), 4),
                    (tokens(&op.tags.join(" ")), 3),
                    (tokens(&op.summary), 2),
                    (tokens(&op.description), 1),
                ];
                let mut score = 0;
                for word in &query {
                    let weight = fields
                        .iter()
                        .filter(|(words, _)| words.iter().any(|w| w.contains(word)))
                        .map(|(_, weight)| *weight)
                        .max()?;
                    score += weight;
                }
                Some(Hit {
                    name: op.name.clone(),
                    method: op.method,
                    path: op.path.clone(),
                    summary: op.summary.clone(),
                    score,
                })
            })
            .collect();
        hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
        hits.truncate(limit);
        hits
    }

    fn suggest(&self, name: &str) -> Vec<String> {
        let mut ranked: Vec<_> = self.ops.iter().map(|op| (distance(name, &op.name), &op.name)).collect();
        ranked.sort();
        ranked.into_iter().take(3).map(|(_, name)| name.clone()).collect()
    }
}

fn tokens(text: &str) -> Vec<String> {
    let mut split = String::new();
    let mut lower = false;
    for c in text.chars() {
        if !c.is_ascii_alphanumeric() {
            split.push(' ');
        } else {
            if lower && c.is_ascii_uppercase() {
                split.push(' ');
            }
            split.push(c.to_ascii_lowercase());
        }
        lower = c.is_ascii_lowercase() || c.is_ascii_digit();
    }
    split.split_whitespace().map(str::to_owned).collect()
}

fn distance(a: &str, b: &str) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.bytes().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.bytes().enumerate() {
            let previous = row[j + 1];
            row[j + 1] = (row[j] + 1).min(previous + 1).min(diagonal + usize::from(x != y));
            diagonal = previous;
        }
    }
    row[b.len()]
}
