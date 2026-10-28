use serde_json::Value;
use std::io::Write;
const URL_COMPONENT: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');

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
        let escaped = serde_json::to_string(key).expect("a string serializes");
        self.secrets.push(escaped[1..escaped.len() - 1].into());
        let sets = [percent_encoding::NON_ALPHANUMERIC, URL_COMPONENT];
        for set in sets {
            let encoded = percent_encoding::utf8_percent_encode(key, set).to_string();
            let mut lower = encoded.clone().into_bytes();
            for i in 0..lower.len().saturating_sub(2) {
                if lower[i] == b'%' {
                    lower[i + 1].make_ascii_lowercase();
                    lower[i + 2].make_ascii_lowercase();
                }
            }
            self.secrets.push(encoded);
            self.secrets.push(String::from_utf8(lower).expect("percent encoding is ASCII"));
        }
        let form = url::form_urlencoded::Serializer::new(String::new()).append_pair("", key).finish();
        self.secrets.push(form[1..].into());
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
        self.secrets.iter().fold(text.to_owned(), |text, secret| text.replace(secret, "<redacted>"))
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
                let safe = if let Some((_, host)) = safe[scheme.len()..].split_once('@') {
                    format!("{scheme}{host}")
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
