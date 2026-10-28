use std::collections::{BTreeMap, BTreeSet};

use updraft_registry::path::{Template, Word};
use updraft_registry::{Method, Op, Registry, Server};

use crate::build::{Collision, Report};

pub fn assign(ops: &mut [Op], previous: Option<&Registry>, report: &mut Report) {
    for op in ops.iter().filter(|op| op.path.starts_with("/cloud/v2/")) {
        let template = Template::parse(&op.path);
        let relative = template.without_root(template.root_len());
        let family = cloud_family(&relative.words());
        if cloud_alias(family.collection).is_none() {
            report.unaliased_cloud_collections.insert(family.collection.to_owned());
        }
    }
    let mut taken: BTreeSet<String> = BTreeSet::new();
    if let Some(previous) = previous {
        let surviving: BTreeSet<&str> = ops.iter().map(|op| op.key.as_str()).collect();
        let by_key: BTreeMap<&str, &str> = previous.ops.iter().map(|op| (op.key.as_str(), op.name.as_str())).collect();
        let by_id: BTreeMap<&str, &str> = previous
            .ops
            .iter()
            .filter(|op| !surviving.contains(op.key.as_str()))
            .filter_map(|op| op.operation_id.as_deref().map(|id| (id, op.name.as_str())))
            .collect();
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
    let relative = template.without_root(template.root_len());
    let words = relative.words();
    if op.path.starts_with("/cloud/v2/") {
        let family = cloud_family(&words);
        let namespace = cloud_alias(family.collection).unwrap_or(family.collection);
        let mut below = words[family.at + 1..].to_vec();
        if matches!(below.first(), Some(Word::Param)) {
            below.remove(0);
        }
        if family.collection.starts_with("luau-execution-") {
            below.retain(|word| !matches!(word, Word::Noun("tasks")));
            if family.collection.ends_with("binary-inputs") {
                below.insert(0, Word::Noun("binary-inputs"));
            } else {
                below.insert(0, Word::Noun("tasks"));
                if op.method == Method::Post && words[..family.at].contains(&Word::Noun("versions")) {
                    below.insert(0, Word::Noun("version"));
                }
            }
        } else if family.collection == "instances" {
            below.insert(0, Word::Noun("instance"));
        } else if family.collection == "creator-store-products" {
            let noun = if words.get(family.at + 1) == Some(&Word::Param) { "product" } else { "products" };
            below.insert(0, Word::Noun(noun));
        }
        if family.collection == "user-restrictions" && words[..family.at].contains(&Word::Noun("places")) {
            below.insert(0, Word::Noun("place"));
        }
        if let Some(at) = below.iter().position(|word| *word == Word::Noun("forum-categories")) {
            let last = below
                .iter()
                .filter_map(|word| match word {
                    Word::Noun(noun) => Some(*noun),
                    _ => None,
                })
                .next_back()
                .unwrap();
            let noun = match last {
                "posts" => "forum-posts",
                "comments" => "forum-comments",
                _ => "forum-categories",
            };
            below.truncate(at);
            below.push(Word::Noun(noun));
        }
        let mut action = action(op.method, &below, op.pagination.is_some());
        if action == "assign-role-membership" || action == "unassign-role-membership" {
            action = action.replace("-membership", "");
        }
        return format!("{namespace}.{action}");
    }
    let namespace = api(&op.path, op.server);
    let mut words = words;
    if namespace == "data-stores-v1" {
        let at = words.iter().position(|word| *word == Word::Noun("entries"));
        words = at.map_or_else(Vec::new, |at| words[at..].to_vec());
        words = words
            .iter()
            .enumerate()
            .filter_map(|(at, word)| {
                let redundant = matches!(
                    (word, words.get(at + 1)),
                    (Word::Noun("entries"), Some(Word::Noun("entry")))
                        | (Word::Noun("versions"), Some(Word::Noun("version")))
                );
                (!redundant).then_some(*word)
            })
            .collect();
    } else if namespace == "ordered-data-stores-v1" {
        let at = words.iter().position(|word| *word == Word::Noun("scopes")).unwrap_or(words.len());
        words = words[at..].to_vec();
    } else {
        words = compact(&words, &namespace);
    }
    format!("{namespace}.{}", action(op.method, &words, op.pagination.is_some()))
}

struct Family<'a> {
    collection: &'a str,
    at: usize,
}

fn cloud_family<'a>(words: &[Word<'a>]) -> Family<'a> {
    let mut at = 0;
    while matches!(words.get(at), Some(Word::Noun("universes" | "places")))
        && matches!(words.get(at + 1), Some(Word::Param))
        && matches!(words.get(at + 2), Some(Word::Noun(_)))
    {
        at += 2;
    }
    if words.get(at) == Some(&Word::Noun("versions"))
        && matches!(words.get(at + 2), Some(Word::Noun(noun)) if noun.starts_with("luau-execution-"))
    {
        at += 2;
    }
    if words.get(at) == Some(&Word::Noun("subscription-products"))
        && words.get(at + 2) == Some(&Word::Noun("subscriptions"))
    {
        at += 2;
    }
    let collection = match words[at] {
        Word::Noun(noun) => noun,
        _ => unreachable!("a resource begins with a collection"),
    };
    Family { collection, at }
}

fn cloud_alias(collection: &str) -> Option<&'static str> {
    match collection {
        "instances" => Some("places"),
        name if name.starts_with("luau-execution-") => Some("luau"),
        "creator-store-products" => Some("creator-store"),
        "subscriptions" => Some("subscriptions"),
        _ => None,
    }
}

fn compact<'a>(words: &[Word<'a>], namespace: &str) -> Vec<Word<'a>> {
    if action(Method::Get, words, false).split('-').count() <= 4 {
        return words.to_vec();
    }
    words
        .iter()
        .enumerate()
        .filter_map(|(at, word)| {
            let Word::Noun(noun) = word else { return Some(*word) };
            let scope = matches!(*noun, "universes" | "universe" | "places" | "games" | "language-codes")
                && matches!(words.get(at + 1), Some(Word::Param));
            let repeated = namespace.contains(kebab(noun).as_str()) || namespace.contains(&singular(noun));
            (!(scope || repeated)).then_some(*word)
        })
        .collect()
}

fn api(path: &str, server: Server) -> String {
    if server == Server::Thumbnails {
        return "thumbnails".to_owned();
    }
    let first = path.trim_start_matches('/').split('/').next().unwrap_or_default();
    if first == "datastores" {
        return "data-stores-v1".to_owned();
    }
    if first == "ordered-data-stores" && path.starts_with("/ordered-data-stores/v1/") {
        return "ordered-data-stores-v1".to_owned();
    }
    if matches!(first, "legacy-followings" | "legacy-develop") && path.contains("/v1/") {
        return format!("{}-v1", first.trim_start_matches("legacy-"));
    }
    let mut name = first.strip_prefix("legacy-").unwrap_or(first);
    for suffix in ["-public-api", "-http-service", "-api", "-service"] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped;
            break;
        }
    }
    name.to_owned()
}

fn action(method: Method, words: &[Word<'_>], paginated: bool) -> String {
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
        (Some(verb), _) if verb.starts_with("list-") => {
            let (verb, rest) = verb.split_once('-').unwrap_or((&verb, ""));
            if !rest.is_empty() {
                nouns.push(rest.to_owned());
            }
            verb.to_owned()
        }
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
    } else if matches!(last, "aliases" | "buses" | "statuses")
        || ["sses", "xes", "ches", "shes", "zzes"].iter().any(|suffix| last.ends_with(suffix))
    {
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
