use serde_json::Value;
use std::io::Write;

pub struct Output {
    secrets: Vec<String>,
    verbose: bool,
}
impl Output {
    pub fn new(verbose: bool) -> Self {
        Self { secrets: vec![], verbose }
    }
    pub fn redact_also(&mut self, secret: &crate::auth::Secret) {
        let key = secret.expose();
        if key.is_empty() {
            return;
        }
        self.secrets.push(key.into());
        self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }
    pub fn document(&self, value: &Value, error: bool) -> std::io::Result<()> {
        fn clean(output: &Output, value: &Value, error: bool) -> Value {
            match value {
                Value::String(s) => Value::String(if error { output.redact_error(s) } else { output.redact(s) }),
                Value::Array(a) => Value::Array(a.iter().map(|v| clean(output, v, error)).collect()),
                Value::Object(o) => {
                    Value::Object(o.iter().map(|(k, v)| (output.redact(k), clean(output, v, error))).collect())
                }
                v => v.clone(),
            }
        }
        writeln!(std::io::stdout().lock(), "{}", clean(self, value, error))
    }
    pub fn text(&self, text: &str) -> std::io::Result<()> {
        write!(std::io::stdout().lock(), "{}", self.redact(text))
    }
    pub fn note(&self, line: &str) {
        let _ = writeln!(std::io::stderr().lock(), "{}", self.redact_error(line));
    }
    pub fn trace(&self, line: &str) {
        if self.verbose {
            self.note(line);
        }
    }
    pub fn redact(&self, text: &str) -> String {
        let mut result = String::new();
        let mut remaining = text;
        while !remaining.is_empty() {
            let matched = self.secrets.iter().find_map(|key| encoded_secret_len(remaining, key));
            if let Some(length) = matched {
                result.push_str("<redacted>");
                remaining = &remaining[length..];
            } else {
                let c = remaining.chars().next().expect("remaining text is nonempty");
                result.push(c);
                remaining = &remaining[c.len_utf8()..];
            }
        }
        result
    }
    fn redact_error(&self, text: &str) -> String {
        let mut text = self.redact(text);
        for scheme in ["https://", "http://"] {
            let mut start = 0;
            while let Some(at) = text[start..].to_ascii_lowercase().find(scheme) {
                let at = start + at;
                let end = text[at..]
                    .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>'))
                    .map_or(text.len(), |i| at + i);
                let token = &text[at..end];
                let safe = token.split(['?', '#']).next().unwrap_or(token);
                let authority_end = safe[scheme.len()..].find('/').map_or(safe.len(), |i| scheme.len() + i);
                let safe = if let Some((_, host)) = safe[scheme.len()..authority_end].rsplit_once('@') {
                    format!("{scheme}{host}{}", &safe[authority_end..])
                } else {
                    safe.into()
                };
                text.replace_range(at..end, &safe);
                start = at + safe.len();
            }
        }
        text
    }
}
fn encoded_secret_len(text: &str, key: &str) -> Option<usize> {
    let mut at = 0;
    for c in key.chars() {
        let rest = &text[at..];
        let unicode: String = c.encode_utf16(&mut [0; 2]).iter().map(|unit| format!("\\u{unit:04x}")).collect();
        let json = serde_json::to_string(&c.to_string()).expect("a character serializes");
        let json = &json[1..json.len() - 1];
        let percent =
            percent_encoding::utf8_percent_encode(&c.to_string(), percent_encoding::NON_ALPHANUMERIC).to_string();
        let encoded = [unicode.as_str(), json, if c == '/' { "\\/" } else { "" }, percent.as_str()]
            .into_iter()
            .filter(|form| !form.is_empty() && (form.starts_with('\\') || form.starts_with('%')))
            .filter(|form| rest.get(..form.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(form)))
            .map(str::len)
            .max();
        at += if let Some(length) = encoded {
            length
        } else if rest.starts_with(c) {
            c.len_utf8()
        } else {
            return None;
        };
    }
    Some(at)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keys_and_signed_urls_are_redacted() {
        let mut output = Output::new(false);
        output.redact_also(&crate::auth::Secret::from_string("Key\"\\+/=".into()));
        assert_eq!(output.redact("Key\"\\+/="), "<redacted>");
        assert_eq!(output.redact("Key\\\"\\\\+/="), "<redacted>");
        assert_eq!(output.redact("Key%22%5C%2B%2F%3D"), "<redacted>");
        assert_eq!(output.redact_error("https://user:pass@cdn.test/file?signature=private"), "https://cdn.test/file");
    }
}
