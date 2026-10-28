#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    segments: Vec<Segment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Param { prefix: String, name: String, suffix: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captures(pub Vec<(String, String)>);

impl Template {
    pub fn parse(template: &str) -> Template {
        let segments = template
            .trim_matches('/')
            .split('/')
            .map(|segment| match (segment.find('{'), segment.find('}')) {
                (Some(open), Some(close)) if open < close => Segment::Param {
                    prefix: segment[..open].to_owned(),
                    name: segment[open + 1..close].to_owned(),
                    suffix: segment[close + 1..].to_owned(),
                },
                _ => Segment::Literal(segment.to_owned()),
            })
            .collect();
        Template { segments }
    }

    pub fn param_names(&self) -> impl Iterator<Item = &str> {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Param { name, .. } => Some(name.as_str()),
            Segment::Literal(_) => None,
        })
    }

    pub fn words(&self) -> Vec<Word<'_>> {
        let mut words = Vec::new();
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => match text.split_once(':') {
                    Some((noun, verb)) => {
                        words.push(Word::Noun(noun));
                        words.push(Word::Verb(verb));
                    }
                    None => words.push(Word::Noun(text)),
                },
                Segment::Param { suffix, .. } => {
                    words.push(Word::Param);
                    if let Some(verb) = suffix.strip_prefix(':') {
                        words.push(Word::Verb(verb));
                    }
                }
            }
        }
        words
    }

    pub fn specificity(&self) -> (usize, usize) {
        let literals = self.segments.iter().filter(|segment| matches!(segment, Segment::Literal(_))).count();
        let constraints = self
            .segments
            .iter()
            .map(|segment| match segment {
                Segment::Param { prefix, suffix, .. } => prefix.len() + suffix.len(),
                _ => 0,
            })
            .sum();
        (literals, constraints)
    }

    pub fn captures(&self, path: &str) -> Option<Captures> {
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        if parts.len() != self.segments.len() {
            return None;
        }
        let mut captures = Vec::new();
        for (segment, part) in self.segments.iter().zip(parts) {
            match segment {
                Segment::Literal(text) if text == part => {}
                Segment::Literal(_) => return None,
                Segment::Param { prefix, name, suffix } => {
                    let value = part.strip_prefix(prefix.as_str())?.strip_suffix(suffix.as_str())?;
                    if value.is_empty() {
                        return None;
                    }
                    captures.push((name.clone(), value.to_owned()));
                }
            }
        }
        Some(Captures(captures))
    }

    pub fn without_root(&self, root_len: usize) -> Template {
        Template { segments: self.segments[root_len.min(self.segments.len())..].to_vec() }
    }

    pub fn root_len(&self) -> usize {
        self.segments
            .iter()
            .position(|segment| {
                matches!(segment, Segment::Literal(text)
                    if text.len() > 1 && text.starts_with('v') && text[1..].bytes().all(|b| b.is_ascii_digit()))
            })
            .map_or(0, |index| index + 1)
    }

    pub fn render(&self, value: impl Fn(&str) -> Option<String>) -> Result<String, String> {
        let mut out = String::new();
        for segment in &self.segments {
            out.push('/');
            match segment {
                Segment::Literal(text) => out.push_str(text),
                Segment::Param { prefix, name, suffix } => {
                    let filled = value(name).ok_or_else(|| name.clone())?;
                    out.push_str(prefix);
                    out.push_str(&filled);
                    out.push_str(suffix);
                }
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Word<'a> {
    Noun(&'a str),
    Verb(&'a str),
    Param,
}

pub fn fold(name: &str) -> String {
    name.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_verbs_and_resource_paths_match() {
        let archive = Template::parse("/assets/v1/assets/{assetId}:archive");
        assert_eq!(
            archive.captures("/assets/v1/assets/42:archive"),
            Some(Captures(vec![("assetId".into(), "42".into())]))
        );
        assert_eq!(archive.captures("/assets/v1/assets/42"), None);

        let poll = Template::parse("/assets/v1/operations/{operationId}");
        let relative = poll.without_root(poll.root_len());
        assert!(relative.captures("operations/abc").is_some());
    }
}
