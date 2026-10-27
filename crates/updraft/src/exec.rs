use crate::auth::Secret;
use crate::call::Call;
use crate::outcome::{Attempt, Failure, History, Verdict, judge};
use crate::output::Output;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use updraft_registry::{Auth, Completion, Method, Op, Payload, Registry};
use ureq::RequestExt;

pub struct Executor<'a> {
    pub registry: &'static Registry,
    agent: ureq::Agent,
    origin: Option<String>,
    key: Option<&'a Secret>,
    pub timing: Timing,
    out: &'a Output,
}
#[derive(Clone, Copy)]
pub struct Timing {
    pub poll_interval: Duration,
    pub deadline: Duration,
    pub retry_delay: Option<Duration>,
}
pub enum Reply {
    Json(Value),
    Text(String),
    Bytes(Vec<u8>),
    Empty,
}
impl Reply {
    pub fn json(self) -> Result<Value, Failure> {
        match self {
            Self::Json(v) => Ok(v),
            Self::Empty => Ok(Value::Null),
            _ => Err(Failure::unavailable("expected a JSON response")),
        }
    }
    pub fn document(self, out: Option<&std::path::Path>) -> Result<Value, Failure> {
        match self {
            Self::Json(v) => Ok(v),
            Self::Empty => Ok(Value::Null),
            Self::Text(s) => Ok(json!({"text":s})),
            Self::Bytes(bytes) => {
                let path = out.ok_or_else(|| Failure::unavailable("binary response requires --out"))?;
                std::fs::write(path, &bytes).map_err(|_| Failure::usage("cannot write the output file"))?;
                Ok(json!({"file":path,"bytes":bytes.len()}))
            }
        }
    }
}
impl<'a> Executor<'a> {
    pub fn new(
        registry: &'static Registry,
        origin: Option<String>,
        key: Option<&'a Secret>,
        timing: Timing,
        out: &'a Output,
    ) -> Result<Self, Failure> {
        if let Some(origin) = &origin {
            let url = url::Url::parse(origin).map_err(|_| Failure::usage("UPDRAFT_ORIGIN must be an HTTP origin"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.path() != "/"
            {
                return Err(Failure::usage("UPDRAFT_ORIGIN must be an HTTP origin"));
            }
            out.note("Warning: UPDRAFT_ORIGIN overrides Roblox servers.");
        }
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        Ok(Self { registry, agent, origin, key, timing, out })
    }
    pub fn send(&self, call: &Call<'_>) -> Result<Reply, Failure> {
        let op = call.op();
        let mut history = History::default();
        loop {
            if call.timeout().is_some_and(|remaining| remaining.is_zero()) {
                return Err(Failure::unavailable("request deadline passed"));
            }
            let mut request = call.request(self.origin.as_deref().unwrap_or(op.server.origin()))?;
            if matches!(op.auth, Auth::ApiKey { .. }) {
                let key = self.key.ok_or_else(|| Failure::new(3, "auth", "no API key selected"))?;
                request.headers_mut().insert(
                    "x-api-key",
                    ureq::http::HeaderValue::from_str(key.expose())
                        .map_err(|_| Failure::new(3, "auth", "selected key is not a valid header value"))?,
                );
            }
            self.out.trace(&format!("{} {}", request.method(), op.path));
            let timeout =
                call.timeout().unwrap_or_else(|| Duration::from_secs(if op.method == Method::Get { 30 } else { 120 }));
            let attempt = match request.with_agent(&self.agent).configure().timeout_global(Some(timeout)).run() {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    self.out.trace(&format!("HTTP {status}"));
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|s| s.to_str().ok())
                        .and_then(|s| s.parse().ok());
                    match response.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec() {
                        Ok(body) => {
                            if (200..300).contains(&status)
                                && matches!(op.response, Payload::Json { .. })
                                && status != 204
                                && serde_json::from_slice::<Value>(&body).is_err()
                            {
                                Attempt::Transport {
                                    sent: true,
                                    message: format!("invalid JSON: {}", String::from_utf8_lossy(&body)),
                                }
                            } else {
                                Attempt::Response { status, retry_after, body }
                            }
                        }
                        Err(error) => Attempt::Transport { sent: true, message: error.to_string() },
                    }
                }
                Err(error) => {
                    let not_sent = matches!(
                        error,
                        ureq::Error::HostNotFound
                            | ureq::Error::ConnectionFailed
                            | ureq::Error::Timeout(ureq::Timeout::Connect | ureq::Timeout::Resolve)
                    ) || matches!(&error,ureq::Error::Io(e) if matches!(e.kind(),std::io::ErrorKind::ConnectionRefused|std::io::ErrorKind::AddrNotAvailable|std::io::ErrorKind::NetworkUnreachable|std::io::ErrorKind::HostUnreachable));
                    Attempt::Transport { sent: !not_sent, message: error.to_string() }
                }
            };
            let reason = match &attempt {
                Attempt::Response { status, .. } => format!("HTTP {status}"),
                Attempt::Transport { .. } => "request failed".into(),
            };
            match judge(op, attempt, history) {
                Verdict::Success(body) => {
                    return match &op.response {
                        Payload::Json { .. } if !body.is_empty() => serde_json::from_slice(&body)
                            .map(Reply::Json)
                            .map_err(|_| Failure::unavailable("invalid JSON response")),
                        Payload::Json { .. } | Payload::Empty => Ok(Reply::Empty),
                        Payload::Text => String::from_utf8(body)
                            .map(Reply::Text)
                            .map_err(|_| Failure::unavailable("response text is not UTF-8")),
                        Payload::Binary => Ok(Reply::Bytes(body)),
                    };
                }
                Verdict::Retry { after_seconds, history: next } => {
                    history = next;
                    let delay = self.timing.retry_delay.unwrap_or(Duration::from_secs(after_seconds.into()));
                    let attempts = op.retry.as_ref().map_or(3, |r| r.attempts);
                    self.out.note(&format!(
                        "{reason}, retrying in {} s (attempt {} of {attempts})",
                        delay.as_secs_f64(),
                        history.attempts + 1
                    ));
                    std::thread::sleep(call.timeout().map_or(delay, |remaining| delay.min(remaining)));
                }
                Verdict::Fail(failure) => {
                    return Err(if failure.code == 3 {
                        let scopes = match &op.auth {
                            Auth::ApiKey { scopes } => scopes.clone(),
                            Auth::None => Some(vec![]),
                        };
                        let metadata = if scopes.is_some() { "declared" } else { "unknown" };
                        failure.with("requiredScopes", json!(scopes)).with("scopeMetadata", json!(metadata))
                    } else {
                        failure
                    });
                }
            }
        }
    }
    pub fn send_all(&self, call: Call<'_>, max_items: Option<usize>) -> Result<Value, Failure> {
        let p = call.op().pagination.as_ref().ok_or_else(|| Failure::usage("operation does not declare pagination"))?;
        if max_items == Some(0) {
            return Err(Failure::usage("--max-items must be positive"));
        }
        let mut call = if let Some(limit) = max_items { call.limit_page(limit)? } else { call };
        let mut result = Value::Null;
        let mut items = vec![];
        let mut tokens = BTreeSet::new();
        loop {
            let page = self.send(&call)?.json()?;
            let page_items = page[&p.items_field]
                .as_array()
                .ok_or_else(|| Failure::unavailable(format!("page has no {} array", p.items_field)))?;
            items.extend(page_items.iter().cloned());
            if result.is_null() {
                result = page.clone();
            }
            let token = page[&p.next_field].as_str().unwrap_or_default();
            result[&p.next_field] = json!(token);
            if max_items.is_some_and(|limit| items.len() > limit) {
                return Err(Failure::unavailable(
                    "server exceeded the requested page size; use a smaller page size or omit --max-items",
                ));
            }
            if token.is_empty() || max_items.is_some_and(|limit| items.len() >= limit) {
                break;
            }
            if !tokens.insert(token.to_owned()) {
                return Err(Failure::unavailable("server repeated a pagination token"));
            }
            call = call.next_page(token, max_items.map(|limit| limit - items.len()))?;
        }
        result[&p.items_field] = json!(items);
        Ok(result)
    }
    pub fn finish(&self, op: &Op, first: Value) -> Result<Value, Failure> {
        let Some(completion) = &op.completion else { return Ok(first) };
        let until = Instant::now() + self.timing.deadline;
        let mut value = first;
        let mut path = value["path"].as_str().map(str::to_owned);
        loop {
            match completion {
                Completion::Operation => {
                    if !value["error"].is_null() {
                        return Err(Failure::remote(format!("operation failed: {}", value["error"]), value));
                    }
                    if value["done"] == true {
                        return Ok(value);
                    }
                }
                Completion::State { field, success, failure } => {
                    if let Some(state) = value[field].as_str() {
                        if success.iter().any(|s| s == state) {
                            return Ok(value);
                        }
                        if failure.iter().any(|s| s == state) {
                            return Err(Failure::remote(format!("task {state}"), value));
                        }
                    }
                }
            }
            if let Some(new) = value["path"].as_str() {
                path = Some(new.into());
            }
            let path = path.as_deref().ok_or_else(|| Failure::unavailable("response has no resource path to poll"))?;
            let (reader, captures) = self.registry.reader_of(op, path).ok_or_else(|| {
                Failure::unavailable(format!("no reader matches task path {path}")).with("path", json!(path))
            })?;
            let call = Call::new(reader, decode_captures(captures.0), vec![], None)?;
            let resume = format!("updraft call GET {}", call.path());
            let pending = |detail| {
                Failure::new(
                    8,
                    "pending",
                    format!("resource is not done after {} s", self.timing.deadline.as_secs_f64()),
                )
                .with("resume", json!(resume))
                .with("detail", detail)
            };
            if Instant::now() >= until {
                return Err(pending(value));
            }
            std::thread::sleep(self.timing.poll_interval.min(until.saturating_duration_since(Instant::now())));
            if Instant::now() >= until {
                continue;
            }
            let call = call.with_timeout(until.saturating_duration_since(Instant::now()));
            value = match self.send(&call) {
                Ok(reply) => reply.json()?,
                Err(_) if Instant::now() >= until => return Err(pending(value)),
                Err(failure) => return Err(failure),
            };
        }
    }
}
pub fn decode_captures(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    pairs
        .into_iter()
        .map(|(k, v)| (k, percent_encoding::percent_decode_str(&v).decode_utf8_lossy().into_owned()))
        .collect()
}
