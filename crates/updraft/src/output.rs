use serde_json::Value;
use std::io::Write;

pub struct Output {
    secrets: Vec<Credential>,
    starts: [bool; 256],
    verbose: bool,
}
struct Credential {
    literal: String,
    characters: Vec<EncodedChar>,
}
struct EncodedChar {
    literal: char,
    forms: Vec<String>,
}
impl Credential {
    fn new(literal: String) -> Self {
        let characters = literal
            .chars()
            .map(|c| {
                let unicode: String = c.encode_utf16(&mut [0; 2]).iter().map(|unit| format!("\\u{unit:04x}")).collect();
                let json = serde_json::to_string(&c.to_string()).expect("a character serializes");
                let percent: String = c.to_string().bytes().map(|byte| format!("%{byte:02X}")).collect();
                let mut forms = vec![unicode, json[1..json.len() - 1].into(), percent];
                if c == '/' {
                    forms.push("\\/".into());
                }
                forms.retain(|form| form.starts_with(['\\', '%']));
                EncodedChar { literal: c, forms }
            })
            .collect();
        Self { literal, characters }
    }
}
impl Output {
    pub fn new(verbose: bool) -> Self {
        Self { secrets: vec![], starts: [false; 256], verbose }
    }
    pub fn redact_also(&mut self, secret: &crate::auth::Secret) {
        let key = secret.expose();
        if key.is_empty() {
            return;
        }
        let form = url::form_urlencoded::Serializer::new(String::new()).append_pair("", key).finish();
        for literal in [key, &form[1..]] {
            let credential = Credential::new(literal.into());
            self.starts[usize::from(literal.as_bytes()[0])] = true;
            for encoded in &credential.characters[0].forms {
                self.starts[usize::from(encoded.as_bytes()[0])] = true;
            }
            self.secrets.push(credential);
        }
        self.secrets.sort_by_key(|s| std::cmp::Reverse(s.literal.len()));
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
        let mut result = String::with_capacity(text.len());
        let mut remaining = text;
        while !remaining.is_empty() {
            let matched = self.starts[usize::from(remaining.as_bytes()[0])]
                .then(|| self.secrets.iter().find_map(|key| encoded_secret_len(remaining, key)))
                .flatten();
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
fn encoded_secret_len(text: &str, key: &Credential) -> Option<usize> {
    if text.starts_with(&key.literal) {
        return Some(key.literal.len());
    }
    let mut at = 0;
    for c in &key.characters {
        let rest = &text[at..];
        if !rest.starts_with(['\\', '%']) {
            if !rest.starts_with(c.literal) {
                return None;
            }
            at += c.literal.len_utf8();
            continue;
        }
        let encoded = c
            .forms
            .iter()
            .filter(|form| rest.get(..form.len()).is_some_and(|prefix| prefix.eq_ignore_ascii_case(form)))
            .map(String::len)
            .max();
        at += if let Some(length) = encoded {
            length
        } else if rest.starts_with(c.literal) {
            c.literal.len_utf8()
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
    fn redaction_handles_five_megabytes_of_prefixes() {
        let key = "sentinel-Key_+/%=NeverPrint";
        let mut output = Output::new(false);
        output.redact_also(&crate::auth::Secret::from_string(key.into()));
        let body = key[..1].repeat(5_000_000);
        let start = std::time::Instant::now();
        let redacted = output.redact(&body);
        let elapsed = start.elapsed();
        assert_eq!(redacted, body, "all five megabytes must be scanned without losing text");
        eprintln!("redacted 5000000 prefix bytes in {elapsed:?}");
        if !cfg!(debug_assertions) {
            assert!(elapsed < std::time::Duration::from_millis(500), "redaction must finish well under a second");
        }
    }

    #[test]
    fn literal_credentials_win_over_ambiguous_escape_prefixes() {
        for key in [r"Key\\tail", "Key Space", "Key\"\\+/=", "\u{00e9}\u{1f511}", "Key\tline\n"] {
            let mut output = Output::new(false);
            output.redact_also(&crate::auth::Secret::from_string(key.into()));
            let json = serde_json::to_string(key).unwrap();
            let percent = percent_encoding::utf8_percent_encode(key, percent_encoding::NON_ALPHANUMERIC).to_string();
            let form = url::form_urlencoded::Serializer::new(String::new()).append_pair("", key).finish();
            let unicode: String = key.encode_utf16().map(|unit| format!("\\u{unit:04X}")).collect();
            let fully_encoded: String = key.bytes().map(|byte| format!("%{byte:02X}")).collect();
            for encoded in [
                key,
                &json[1..json.len() - 1],
                &percent,
                &form[1..],
                &unicode,
                &unicode.to_ascii_lowercase(),
                &fully_encoded,
                &fully_encoded.to_ascii_lowercase(),
            ] {
                assert_eq!(
                    output.redact(encoded),
                    "<redacted>",
                    "literal credentials and their JSON, Unicode, percent, and form encodings must be redacted"
                );
            }
        }
    }

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
