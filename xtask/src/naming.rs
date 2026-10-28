use std::collections::{BTreeMap, BTreeSet};

use updraft_registry::path::{Template, Word};
use updraft_registry::{Method, Op, Registry, Server};

use crate::build::{Collision, Report};
use crate::overlay::Vocabulary;

pub fn assign(
    ops: &mut [Op],
    previous: Option<&Registry>,
    vocabulary: &BTreeMap<String, Vocabulary>,
    report: &mut Report,
) {
    for op in ops.iter().filter(|op| op.path.starts_with("/cloud/v2/")) {
        let template = Template::parse(&op.path);
        let relative = template.without_root(template.root_len());
        let family = cloud_family(&relative.words());
        if cloud_alias(family.collection).is_none() {
            report.unaliased_cloud_collections.insert(family.collection.to_owned());
        }
    }
    let mut taken: BTreeSet<String> = BTreeSet::new();
    let mut kept = BTreeSet::new();
    let mut versions: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for op in ops.iter() {
        let name = if op.name.is_empty() { derive(op, vocabulary) } else { op.name.clone() };
        versions.entry(name).or_default().insert(version(op));
    }
    if let Some(previous) = previous {
        let surviving: BTreeSet<&str> = ops.iter().map(|op| op.key.as_str()).collect();
        let by_key: BTreeMap<&str, &str> = previous.ops.iter().map(|op| (op.key.as_str(), op.name.as_str())).collect();
        let by_id: BTreeMap<&str, &str> = previous
            .ops
            .iter()
            .filter(|op| !surviving.contains(op.key.as_str()))
            .filter_map(|op| op.operation_id.as_deref().map(|id| (id, op.name.as_str())))
            .collect();
        for (index, op) in ops.iter_mut().enumerate().filter(|(_, op)| op.name.is_empty()) {
            if versions.get(&derive(op, vocabulary)).is_some_and(|versions| versions.len() > 1) {
                continue;
            }
            let prior = by_key.get(op.key.as_str()).or_else(|| op.operation_id.as_deref().and_then(|id| by_id.get(id)));
            if let Some(name) = prior.filter(|name| !taken.contains(**name)) {
                op.name = (*name).to_owned();
                taken.insert(op.name.clone());
                report.kept_names += 1;
                kept.insert(index);
            }
        }
    }
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, op) in ops.iter().enumerate().filter(|(index, _)| !kept.contains(index)) {
        let name = if op.name.is_empty() { derive(op, vocabulary) } else { op.name.clone() };
        groups.entry(name).or_default().push(index);
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
    let newest = members.iter().map(|index| version(&ops[*index])).max().unwrap_or(0);
    let versions: BTreeSet<u32> = members.iter().map(|index| version(&ops[*index])).collect();
    let mut names: Vec<String> = members
        .iter()
        .map(|index| {
            let v = version(&ops[*index]);
            if versions.len() > 1 && v < newest { format!("{derived}-v{v}") } else { derived.to_owned() }
        })
        .collect();
    if versions.len() == 1 && members.len() > 1 {
        let nouns: Vec<Vec<String>> = members
            .iter()
            .map(|index| {
                let template = Template::parse(&ops[*index].path);
                template
                    .without_root(template.root_len())
                    .words()
                    .iter()
                    .filter_map(|word| if let Word::Noun(noun) = word { Some(singular(noun)) } else { None })
                    .collect()
            })
            .collect();
        if let Some(at) = (0..nouns.iter().map(Vec::len).min().unwrap_or(0))
            .find(|at| nouns.iter().map(|nouns| &nouns[*at]).collect::<BTreeSet<_>>().len() == members.len())
        {
            for (index, name) in names.iter_mut().enumerate() {
                *name = format!("{derived}-{}", nouns[index][at]);
            }
        }
    }
    if taken.contains(derived) {
        for (at, name) in names.iter_mut().enumerate() {
            *name = format!("{derived}-v{}", version(&ops[members[at]]));
        }
    }
    names
}

pub fn derive(op: &Op, vocabulary: &BTreeMap<String, Vocabulary>) -> String {
    let raw = derive_path(op);
    let (namespace, action) = raw.split_once('.').unwrap();
    let default = Vocabulary::default();
    let vocabulary = vocabulary.get(namespace).unwrap_or(&default);
    if vocabulary.exceptions.iter().any(|exception| exception.name == raw && !exception.reason.trim().is_empty()) {
        return raw;
    }
    let (verb, nouns) = action_parts(op, action);
    let mut nouns: Vec<String> = nouns.split('-').filter(|word| !word.is_empty()).map(str::to_owned).collect();
    for rename in &vocabulary.rename {
        replace_noun(&mut nouns, &rename.from, Some(&rename.to));
    }
    for noun in vocabulary.drop.iter().map(String::as_str).chain([namespace.trim_end_matches("-v1")]) {
        replace_noun(&mut nouns, noun, None);
    }
    if nouns.is_empty() { format!("{namespace}.{verb}") } else { format!("{namespace}.{verb}-{}", nouns.join("-")) }
}

fn action_parts<'a>(op: &Op, action: &'a str) -> (&'a str, &'a str) {
    let template = Template::parse(&op.path);
    let custom =
        template.words().iter().find_map(|word| if let Word::Verb(verb) = word { Some(kebab(verb)) } else { None });
    if let Some(custom) = custom.filter(|verb| !verb.starts_with("list-")) {
        if action == custom {
            return (action, "");
        }
        if let Some(nouns) = action.strip_prefix(&format!("{custom}-")) {
            return (&action[..custom.len()], nouns);
        }
    }
    action.split_once('-').unwrap_or((action, ""))
}

fn replace_noun(words: &mut Vec<String>, from: &str, to: Option<&str>) {
    let count = from.split('-').count();
    let mut at = 0;
    while at + count <= words.len() {
        let phrase = words[at..at + count].join("-");
        if singular(&phrase) != singular(from) {
            at += 1;
            continue;
        }
        let replacement = to.map(|to| if phrase == singular(&phrase) { singular(to) } else { to.to_owned() });
        let replacement: Vec<String> =
            replacement.into_iter().flat_map(|text| text.split('-').map(str::to_owned).collect::<Vec<_>>()).collect();
        let added = replacement.len();
        words.splice(at..at + count, replacement);
        at += added;
    }
}

pub fn consistency(ops: &[Op], vocabulary: &BTreeMap<String, Vocabulary>) -> Vec<String> {
    let mut errors = Vec::new();
    for op in ops {
        let (namespace, action) = op.name.split_once('.').unwrap();
        if action.split('-').count() > 4 {
            errors.push(format!("{} exceeds four action words", op.name));
        }
        let default = Vocabulary::default();
        let vocabulary = vocabulary.get(namespace).unwrap_or(&default);
        if vocabulary
            .exceptions
            .iter()
            .any(|exception| exception.name == op.name && !exception.reason.trim().is_empty())
        {
            continue;
        }
        let (_, nouns) = action_parts(op, action);
        let nouns = nouns.strip_suffix("-v1").unwrap_or(nouns);
        let words: Vec<_> = nouns.split('-').collect();
        let contains = |term: &str| {
            let count = term.split('-').count();
            words.windows(count).any(|window| singular(&window.join("-")) == singular(term))
        };
        for term in vocabulary.drop.iter().chain(vocabulary.rename.iter().map(|rename| &rename.from)) {
            if contains(term) {
                errors.push(format!("{} contains forbidden vocabulary noun {term}", op.name));
            }
        }
        let own = namespace.trim_end_matches("-v1");
        let count = own.split('-').count();
        if words.len() >= count
            && [words[..count].join("-"), words[words.len() - count..].join("-")]
                .iter()
                .any(|noun| singular(noun) == singular(own))
        {
            errors.push(format!("{} repeats its namespace noun", op.name));
        }
    }
    errors
}

fn derive_path(op: &Op) -> String {
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
        if let Some(collection) = family.collection.strip_prefix("luau-execution-") {
            let resource = collection
                .strip_prefix("session-task-")
                .or_else(|| collection.strip_prefix("session-"))
                .unwrap_or(collection);
            if collection != "sessions" {
                below.insert(0, Word::Noun(resource));
                if words.get(family.at + 1) == Some(&Word::Param) {
                    below.insert(1, Word::Param);
                }
            }
            if op.method == Method::Post && words[..family.at].contains(&Word::Noun("versions")) {
                below.insert(0, Word::Noun("version"));
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
        let mut action = action(op.method, &compact(&below), op.pagination.is_some());
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
        words = compact(&words);
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

fn compact<'a>(words: &[Word<'a>]) -> Vec<Word<'a>> {
    let scopes: BTreeSet<usize> = words
        .iter()
        .enumerate()
        .filter_map(|(at, word)| {
            let Word::Noun(noun) = word else { return None };
            let scoped = matches!(
                *noun,
                "universes"
                    | "universe"
                    | "places"
                    | "games"
                    | "users"
                    | "groups"
                    | "tables"
                    | "versions"
                    | "resource"
                    | "id"
                    | "languages"
            ) && matches!(words.get(at + 1), Some(Word::Param))
                && words[at + 2..].iter().any(|word| matches!(word, Word::Noun(_)));
            scoped.then_some(at)
        })
        .collect();
    words
        .iter()
        .enumerate()
        .filter_map(|(at, word)| (!(scopes.contains(&at) || at > 0 && scopes.contains(&(at - 1)))).then_some(*word))
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
