use crate::outcome::Failure;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use updraft_registry::path::{Template, fold};
use updraft_registry::{BodyMedia, Location, Op, PartKind, ValueKind};

const SEGMENT: &percent_encoding::AsciiSet =
    &percent_encoding::NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');

pub struct Call<'r> {
    op: &'r Op,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Option<BodyInput>,
    values: Vec<(String, String)>,
    until: Option<std::time::Instant>,
}
#[derive(Clone)]
pub enum BodyInput {
    Json { content_type: String, bytes: Vec<u8> },
    Binary { content_type: String, bytes: Vec<u8> },
    Multipart { parts: Vec<PartInput> },
}
#[derive(Clone)]
pub struct PartInput {
    pub name: String,
    pub content: PartContent,
}
#[derive(Clone)]
pub enum PartContent {
    Json(Vec<u8>),
    Text(String),
    File { file_name: String, content_type: String, bytes: Vec<u8> },
}
#[derive(Clone)]
pub enum Source {
    Inline(String),
    File(PathBuf),
    Stdin,
}
impl Source {
    pub fn parse(text: &str) -> Self {
        match text {
            "@-" => Self::Stdin,
            _ if text.starts_with("@@") => Self::Inline(text[1..].into()),
            _ => text.strip_prefix('@').map_or_else(|| Self::Inline(text.into()), |p| Self::File(p.into())),
        }
    }
    pub fn read(&self) -> Result<Vec<u8>, Failure> {
        match self {
            Self::Inline(s) => Ok(s.as_bytes().to_vec()),
            Self::File(p) => std::fs::read(p).map_err(|_| Failure::usage("cannot read the input file")),
            Self::Stdin => {
                let mut b = vec![];
                std::io::stdin().read_to_end(&mut b).map_err(|_| Failure::usage("cannot read stdin"))?;
                Ok(b)
            }
        }
    }
}
impl<'r> Call<'r> {
    pub fn new(
        op: &'r Op,
        preset: Vec<(String, String)>,
        pairs: Vec<(String, String)>,
        body: Option<BodyInput>,
    ) -> Result<Self, Failure> {
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let explicit: std::collections::BTreeSet<_> = pairs.iter().map(|(k, _)| fold(k)).collect();
        for (name, value) in preset.into_iter().filter(|(name, _)| !explicit.contains(&fold(name))).chain(pairs) {
            let param = op.params.iter().find(|p| fold(&p.name) == fold(&name)).ok_or_else(|| {
                Failure::usage(format!(
                    "unknown parameter; expected {}",
                    op.params.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
                ))
            })?;
            validate(&param.kind, &value).map_err(|_| Failure::usage(format!("invalid value for {}", param.name)))?;
            let entries = values.entry(param.name.clone()).or_default();
            if !entries.is_empty() && !matches!(param.kind, ValueKind::Array { .. }) {
                return Err(Failure::usage(format!("duplicate parameter {}", param.name)));
            }
            entries.push(value);
        }
        for p in &op.params {
            if p.required && !values.contains_key(&p.name) {
                return Err(Failure::usage(format!("missing parameter {}", p.name)));
            }
        }
        validate_body(op, body.as_ref())?;
        let path = Template::parse(&op.path)
            .render(|name| values.get(name).map(|v| percent_encoding::utf8_percent_encode(&v[0], SEGMENT).to_string()))
            .map_err(|_| Failure::usage("missing path parameter"))?;
        let mut query = vec![];
        let mut headers = vec![];
        for p in &op.params {
            if let Some(v) = values.get(&p.name) {
                if p.location == Location::Path && v.iter().any(|v| v == "." || v == "..") {
                    return Err(Failure::usage("dot path segments are not allowed"));
                }
                match p.location {
                    Location::Query => {
                        for value in v {
                            query.push((p.name.clone(), value.clone()));
                        }
                    }
                    Location::Header => {
                        let text = v.join(",");
                        ureq::http::HeaderName::from_bytes(p.name.as_bytes())
                            .map_err(|_| Failure::usage("invalid header name"))?;
                        ureq::http::HeaderValue::from_str(&text)
                            .map_err(|_| Failure::usage(format!("invalid header value for {}", p.name)))?;
                        if p.name.eq_ignore_ascii_case("x-api-key") || p.name.eq_ignore_ascii_case("authorization") {
                            return Err(Failure::usage("credentials must come from the selected key source"));
                        }
                        headers.push((p.name.clone(), text));
                    }
                    Location::Path => {}
                }
            }
        }
        Ok(Self {
            op,
            path,
            query,
            headers,
            body,
            until: None,
            values: values.into_iter().flat_map(|(k, vs)| vs.into_iter().map(move |v| (k.clone(), v))).collect(),
        })
    }
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.until = Some(std::time::Instant::now() + timeout);
        self
    }
    pub fn timeout(&self) -> Option<std::time::Duration> {
        self.until.map(|until| until.saturating_duration_since(std::time::Instant::now()))
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn limit_page(&self, size: usize) -> Result<Self, Failure> {
        let p = self.op.pagination.as_ref().ok_or_else(|| Failure::usage("operation does not declare pagination"))?;
        let token = self.values.iter().find(|(k, _)| k == &p.token_param).map_or("", |(_, v)| v.as_str());
        self.next_page(token, Some(size))
    }
    pub fn page_size(&self) -> Option<usize> {
        let name = self.op.pagination.as_ref()?.size_param.as_ref()?;
        self.values
            .iter()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.parse().ok())
            .filter(|size| *size > 0)
    }
    pub fn op(&self) -> &'r Op {
        self.op
    }
    pub fn has_binary_body(&self) -> bool {
        match &self.body {
            Some(BodyInput::Binary { .. }) => true,
            Some(BodyInput::Multipart { parts }) => {
                parts.iter().any(|part| matches!(part.content, PartContent::File { .. }))
            }
            _ => false,
        }
    }
    pub fn request(&self, origin: &str) -> Result<ureq::http::Request<Vec<u8>>, Failure> {
        let mut url = format!("{}{}", origin.trim_end_matches('/'), self.path);
        if !self.query.is_empty() {
            url.push('?');
            url.push_str(&url::form_urlencoded::Serializer::new(String::new()).extend_pairs(&self.query).finish());
        }
        let mut request = ureq::http::Request::builder()
            .method(format!("{:?}", self.op.method).to_ascii_uppercase().as_str())
            .uri(url);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        let bytes = match &self.body {
            None => vec![],
            Some(BodyInput::Json { content_type, bytes } | BodyInput::Binary { content_type, bytes }) => {
                request = request.header("Content-Type", content_type);
                bytes.clone()
            }
            Some(BodyInput::Multipart { parts }) => {
                let boundary = format!(
                    "updraft-{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("system clock after epoch")
                        .as_nanos()
                );
                let mut bytes = vec![];
                for part in parts {
                    bytes.extend_from_slice(
                        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"", quoted(&part.name))
                            .as_bytes(),
                    );
                    let (media, content) = match &part.content {
                        PartContent::Json(b) => (Some("application/json"), b.clone()),
                        PartContent::Text(s) => (None, s.as_bytes().to_vec()),
                        PartContent::File { file_name, content_type, bytes: content } => {
                            bytes.extend_from_slice(format!("; filename=\"{}\"", quoted(file_name)).as_bytes());
                            (Some(content_type.as_str()), content.clone())
                        }
                    };
                    if let Some(media) = media {
                        bytes.extend_from_slice(format!("\r\nContent-Type: {media}").as_bytes());
                    }
                    bytes.extend_from_slice(b"\r\n\r\n");
                    bytes.extend_from_slice(&content);
                    bytes.extend_from_slice(b"\r\n");
                }
                bytes.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
                request = request.header("Content-Type", format!("multipart/form-data; boundary={boundary}"));
                bytes
            }
        };
        request.body(bytes).map_err(|_| Failure::usage("cannot construct the HTTP request"))
    }
    pub fn next_page(&self, token: &str, size: Option<usize>) -> Result<Self, Failure> {
        let p = self.op.pagination.as_ref().ok_or_else(|| Failure::usage("operation does not declare pagination"))?;
        let mut pairs = self.values.clone();
        pairs.retain(|(k, _)| k != &p.token_param && !(size.is_some() && p.size_param.as_ref() == Some(k)));
        if !token.is_empty() {
            pairs.push((p.token_param.clone(), token.into()));
        }
        if let (Some(name), Some(size)) = (&p.size_param, size) {
            let original = self.page_size();
            pairs.push((name.clone(), size.min(original.unwrap_or(size)).to_string()));
        }
        Self::new(self.op, vec![], pairs, self.body.clone())
    }
}
fn quoted(s: &str) -> String {
    s.replace(['\r', '\n'], "").replace('\\', "\\\\").replace('"', "\\\"")
}
fn validate(kind: &ValueKind, text: &str) -> Result<(), ()> {
    let valid = match kind {
        ValueKind::String => true,
        ValueKind::Integer => text.parse::<i64>().is_ok() || text.parse::<u64>().is_ok(),
        ValueKind::Number => text.parse::<f64>().is_ok_and(f64::is_finite),
        ValueKind::Boolean => matches!(text, "true" | "false"),
        ValueKind::Enum { values } => values.iter().any(|v| v == text),
        ValueKind::Array { items } => validate(items, text).is_ok(),
    };
    if valid { Ok(()) } else { Err(()) }
}
fn validate_body(op: &Op, body: Option<&BodyInput>) -> Result<(), Failure> {
    if matches!(op.auth, updraft_registry::Auth::BodyKey { .. }) {
        return if body.is_none() {
            Ok(())
        } else {
            Err(Failure::usage("the selected key supplies this operation's body; omit --body and --part"))
        };
    }
    match (&op.body, body) {
        (None, Some(_)) => Err(Failure::usage("operation declares no request body")),
        (Some(schema), None) if schema.required => Err(Failure::usage("operation requires a body")),
        (_, None) => Ok(()),
        (Some(schema), Some(input)) => {
            match (&schema.media, input) {
                (BodyMedia::Json { content_type, .. }, BodyInput::Json { content_type: actual, bytes })
                    if content_type == actual =>
                {
                    serde_json::from_slice::<serde_json::Value>(bytes)
                        .map_err(|_| Failure::usage("body is not well-formed JSON"))?;
                }
                (BodyMedia::Binary { content_type }, BodyInput::Binary { content_type: actual, .. })
                    if content_type == actual => {}
                (BodyMedia::Multipart { parts: expected }, BodyInput::Multipart { parts }) => {
                    let mut seen = std::collections::BTreeSet::new();
                    for part in parts {
                        if !seen.insert(&part.name) {
                            return Err(Failure::usage("duplicate multipart part"));
                        }
                        let schema = expected
                            .iter()
                            .find(|p| p.name == part.name)
                            .ok_or_else(|| Failure::usage("unknown multipart part"))?;
                        match (&schema.kind, &part.content) {
                            (PartKind::File, PartContent::File { content_type, .. }) => {
                                ureq::http::HeaderValue::from_str(content_type)
                                    .map_err(|_| Failure::usage("invalid part content type"))?;
                            }
                            (PartKind::Json { .. }, PartContent::Json(bytes)) => {
                                serde_json::from_slice::<serde_json::Value>(bytes)
                                    .map_err(|_| Failure::usage("multipart JSON is malformed"))?;
                            }
                            (PartKind::Text, PartContent::Text(_)) => {}
                            _ => return Err(Failure::usage("multipart part has the wrong kind")),
                        }
                    }
                    for p in expected {
                        if p.required && !seen.contains(&p.name) {
                            return Err(Failure::usage(format!("missing multipart part {}", p.name)));
                        }
                    }
                }
                _ => return Err(Failure::usage("body media does not match the operation")),
            }
            Ok(())
        }
    }
}
pub fn parse_pairs(args: &[String]) -> Result<Vec<(String, String)>, Failure> {
    args.iter()
        .map(|s| {
            s.split_once('=')
                .map(|(k, v)| (k.into(), v.into()))
                .ok_or_else(|| Failure::usage("expected name=value parameters"))
        })
        .collect()
}
pub fn read_body(op: &Op, body: Option<Source>, parts: Vec<(String, Source)>) -> Result<Option<BodyInput>, Failure> {
    if matches!(op.auth, updraft_registry::Auth::BodyKey { .. }) && (body.is_some() || !parts.is_empty()) {
        return Err(Failure::usage("the selected key supplies this operation's body; omit --body and --part"));
    }
    let inputs = body.iter().chain(parts.iter().map(|(_, v)| v));
    if inputs.filter(|s| matches!(s, Source::Stdin)).count() > 1 {
        return Err(Failure::usage("stdin can be consumed only once"));
    }
    if body.is_none() && parts.is_empty() {
        return Ok(None);
    }
    let schema = op.body.as_ref().ok_or_else(|| Failure::usage("operation declares no request body"))?;
    if body.is_some() && !parts.is_empty() {
        return Err(Failure::usage("--body and --part conflict"));
    }
    match (&schema.media, body) {
        (BodyMedia::Json { content_type, .. }, Some(source)) => {
            Ok(Some(BodyInput::Json { content_type: content_type.clone(), bytes: source.read()? }))
        }
        (BodyMedia::Binary { content_type }, Some(source)) => {
            Ok(Some(BodyInput::Binary { content_type: content_type.clone(), bytes: source.read()? }))
        }
        (BodyMedia::Multipart { parts: expected }, None) => {
            let mut inputs = vec![];
            for (name, source) in parts {
                let part =
                    expected.iter().find(|p| p.name == name).ok_or_else(|| Failure::usage("unknown multipart part"))?;
                let content = match &part.kind {
                    PartKind::Json { .. } => PartContent::Json(source.read()?),
                    PartKind::Text => PartContent::Text(
                        String::from_utf8(source.read()?).map_err(|_| Failure::usage("text part is not UTF-8"))?,
                    ),
                    PartKind::File => {
                        let Source::File(mut path) = source else {
                            return Err(Failure::usage("file parts require @file"));
                        };
                        let mut media = None;
                        if let Some((file, kind)) = path.to_string_lossy().split_once(";type=") {
                            media = Some(kind.to_owned());
                            path = file.into();
                        }
                        let bytes = std::fs::read(&path).map_err(|_| Failure::usage("cannot read multipart file"))?;
                        let media = media.unwrap_or_else(|| mime(&path).into());
                        PartContent::File {
                            file_name: path.file_name().unwrap_or_default().to_string_lossy().into(),
                            content_type: media,
                            bytes,
                        }
                    }
                };
                inputs.push(PartInput { name, content });
            }
            Ok(Some(BodyInput::Multipart { parts: inputs }))
        }
        _ => Err(Failure::usage("use --body for JSON or binary and --part for multipart")),
    }
}
pub fn mime(path: &std::path::Path) -> &'static str {
    crate::flows::upload::media(path).map_or("application/octet-stream", |(_, media)| media)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn registry() -> updraft_registry::Registry {
        serde_json::from_str(include_str!("../../../spec/registry.json")).unwrap()
    }
    #[test]
    fn binding_checks_and_encodes_at_the_boundary() {
        let r = registry();
        let op = r.op("assets.get").unwrap();
        let c =
            Call::new(op, vec![("assetId".into(), "old".into())], vec![("asset-id".into(), "new/value".into())], None)
                .unwrap();
        assert_eq!(c.path(), "/assets/v1/assets/new%2Fvalue");
        assert!(Call::new(op, vec![], vec![], None).is_err());
        assert!(Call::new(op, vec![], vec![("assetId".into(), "..".into())], None).is_err());
    }
    #[test]
    fn scalar_kinds_and_array_items_are_validated() {
        for (kind, value) in [
            (ValueKind::Integer, "1.5"),
            (ValueKind::Number, "NaN"),
            (ValueKind::Boolean, "yes"),
            (ValueKind::Enum { values: vec!["Saved".into()] }, "published"),
            (ValueKind::Array { items: Box::new(ValueKind::Integer) }, "1,x"),
        ] {
            assert!(validate(&kind, value).is_err(), "invalid wire values fail before a Call exists");
        }
        assert!(validate(&ValueKind::Integer, "12345678901234567890").is_ok());
    }
}
