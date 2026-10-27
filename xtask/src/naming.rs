use std::collections::{BTreeMap, BTreeSet};

use updraft_registry::path::{Template, Word};
use updraft_registry::{Method, Op, Registry, Server};

use crate::build::{Collision, Report};

pub fn assign(ops: &mut [Op], previous: Option<&Registry>, report: &mut Report) {
    let mut taken: BTreeSet<String> = BTreeSet::new();
    if let Some(previous) = previous {
        let by_key: BTreeMap<&str, &str> = previous.ops.iter().map(|op| (op.key.as_str(), op.name.as_str())).collect();
        let by_id: BTreeMap<&str, &str> =
            previous.ops.iter().filter_map(|op| op.operation_id.as_deref().map(|id| (id, op.name.as_str()))).collect();
        for op in ops.iter_mut().filter(|op| op.name.is_empty()) {
            let kept = by_key.get(op.key.as_str()).or_else(|| op.operation_id.as_deref().and_then(|id| by_id.get(id)));
            if let Some(name) = kept.filter(|name| !taken.contains(**name)) {
                op.name = (*name).to_owned();
                taken.insert(op.name.clone());
                report.kept_names += 1;
            }
        }
    }
    for op in ops.iter().filter(|op| !op.name.is_empty()) {
        taken.insert(op.name.clone());
    }

    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, op) in ops.iter().enumerate().filter(|(_, op)| op.name.is_empty()) {
        groups.entry(derive(op)).or_default().push(index);
    }
    for (derived, mut members) in groups {
        members.sort_by(|a, b| ops[*a].key.cmp(&ops[*b].key));
        if members.len() == 1 && !taken.contains(&derived) {
            ops[members[0]].name = derived.clone();
            taken.insert(derived);
            continue;
        }
        let names = disambiguate(ops, &members, &derived, &taken);
        let mut assigned = Vec::new();
        for (index, name) in members.iter().zip(names) {
            taken.insert(name.clone());
            assigned.push((ops[*index].key.clone(), name.clone()));
            ops[*index].name = name;
        }
        report.name_collisions.push(Collision { derived, assigned });
    }
}

fn disambiguate(ops: &[Op], members: &[usize], derived: &str, taken: &BTreeSet<String>) -> Vec<String> {
    let methods: BTreeSet<Method> = members.iter().map(|index| ops[*index].method).collect();
    let lowest = members.iter().map(|index| version(&ops[*index])).min().unwrap_or(0);
    let mut names: Vec<String> = members
        .iter()
        .map(|index| {
            let op = &ops[*index];
            match version(op) {
                _ if methods.len() > 1 => format!("{derived}-{}", method_word(op.method)),
                v if v > lowest => format!("{derived}-v{v}"),
                _ => derived.to_owned(),
            }
        })
        .collect();
    let colliding = |names: &[String], at: usize| {
        taken.contains(&names[at]) || names.iter().enumerate().any(|(other, name)| other != at && *name == names[at])
    };
    let by_param: Vec<String> = (0..names.len())
        .map(|at| {
            if !colliding(&names, at) {
                return names[at].clone();
            }
            let template = Template::parse(&ops[members[at]].path);
            match template.param_names().last() {
                Some(param) => format!("{}-by-{}", names[at], kebab(param)),
                None => names[at].clone(),
            }
        })
        .collect();
    names = by_param;
    let mut counter = 1;
    for at in 0..names.len() {
        while colliding(&names, at) {
            counter += 1;
            names[at] = format!("{derived}-{counter}");
        }
    }
    names
}

pub fn derive(op: &Op) -> String {
    let template = Template::parse(&op.path);
    format!("{}.{}", api(&op.path, op.server), action(op.method, &template, op.pagination.is_some()))
}

fn api(path: &str, server: Server) -> String {
    if server == Server::Thumbnails {
        return "thumbnails".to_owned();
    }
    let first = path.trim_start_matches('/').split('/').next().unwrap_or_default();
    let mut name = first.strip_prefix("legacy-").unwrap_or(first);
    for suffix in ["-public-api", "-http-service", "-api", "-service"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped;
            break;
        }
    }
    name.to_owned()
}

fn action(method: Method, template: &Template, paginated: bool) -> String {
    let relative = template.without_root(template.root_len());
    let words = relative.words();
    let mut nouns: Vec<String> = Vec::new();
    let mut custom = None;
    for (at, word) in words.iter().enumerate() {
        match word {
            Word::Noun(noun) => {
                let followed_by_param = matches!(words.get(at + 1), Some(Word::Param));
                let noun = if followed_by_param { singular(noun) } else { kebab(noun) };
                if nouns.last() != Some(&noun) {
                    nouns.push(noun);
                }
            }
            Word::Verb(verb) => custom = Some(kebab(verb)),
            Word::Param => {}
        }
    }
    let ends_in_param = matches!(words.last(), Some(Word::Param));
    let verb = match (custom, method) {
        (Some(verb), _) => verb,
        (None, Method::Get) if ends_in_param || !paginated => "get".to_owned(),
        (None, Method::Get) => "list".to_owned(),
        (None, Method::Post) => {
            if !ends_in_param && let Some(last) = nouns.last_mut() {
                *last = singular(last);
            }
            "create".to_owned()
        }
        (None, Method::Put) => "set".to_owned(),
        (None, Method::Patch) => "update".to_owned(),
        (None, Method::Delete) => "delete".to_owned(),
    };
    if nouns.is_empty() { verb } else { format!("{verb}-{}", nouns.join("-")) }
}

fn method_word(method: Method) -> &'static str {
    match method {
        Method::Get => "get",
        Method::Post => "post",
        Method::Put => "put",
        Method::Patch => "patch",
        Method::Delete => "delete",
    }
}

fn version(op: &Op) -> u32 {
    let template = Template::parse(&op.path);
    let root = template.root_len();
    op.path
        .trim_start_matches('/')
        .split('/')
        .nth(root.saturating_sub(1))
        .and_then(|segment| segment.strip_prefix('v'))
        .and_then(|digits| digits.parse().ok())
        .unwrap_or(0)
}

pub fn kebab(text: &str) -> String {
    let mut out = String::new();
    let mut previous: Option<char> = None;
    for c in text.chars() {
        if c == '_' || c == ' ' || c == '-' || c == '.' {
            if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
        } else if c.is_ascii_uppercase() {
            if previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit()) && !out.ends_with('-') {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
        previous = Some(c);
    }
    out.trim_matches('-').to_owned()
}

fn singular(noun: &str) -> String {
    let noun = kebab(noun);
    let (head, last) = match noun.rsplit_once('-') {
        Some((head, last)) => (Some(head), last),
        None => (None, noun.as_str()),
    };
    let last = if let Some(stem) = last.strip_suffix("ies") {
        format!("{stem}y")
    } else if last.ends_with("sses") || last.ends_with("uses") {
        last[..last.len() - 2].to_owned()
    } else if last.ends_with('s') && !last.ends_with("ss") && !last.ends_with("us") && !last.ends_with("is") {
        last[..last.len() - 1].to_owned()
    } else {
        last.to_owned()
    };
    match head {
        Some(head) => format!("{head}-{last}"),
        None => last,
    }
}
