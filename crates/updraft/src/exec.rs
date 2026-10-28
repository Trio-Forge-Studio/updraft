use crate::auth::Secret;
use crate::call::Call;
use crate::outcome::{Acceptance, Attempt, DEFAULT_ATTEMPTS, Failure, History, Verdict, judge, judge_read};
use crate::output::Output;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use updraft_registry::{Auth, Completion, Method, Op, Payload, Registry};
use ureq::RequestExt;

pub struct Executor<'a> {
    pub registry: &'static Registry,
    acceptance: &'a std::cell::RefCell<Acceptance>,
    agent: ureq::Agent,
    downloads: Downloads,
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
    Empty,
}
impl Reply {
    pub fn json(self) -> Result<Value, Failure> {
        match self {
            Self::Json(v) => Ok(v),
            Self::Empty => Ok(Value::Null),
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
        acceptance: &'a std::cell::RefCell<Acceptance>,
    ) -> Result<Self, Failure> {
        if let Some(origin) = &origin {
            let url = url::Url::parse(origin).map_err(|_| Failure::usage("UPDRAFT_ORIGIN must be an HTTP origin"))?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
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
            .timeout_global(Some(Duration::from_secs(600)))
            .build()
            .into();
        let downloads = Downloads {
            agent: ureq::Agent::config_builder()
                .max_redirects(0)
                .max_redirects_will_error(false)
                .http_status_as_error(false)
                .timeout_connect(Some(Duration::from_secs(30)))
                .timeout_recv_response(Some(Duration::from_secs(30)))
                .timeout_per_read(Some(Duration::from_secs(30)))
                .timeout_global(None)
                .build()
                .into(),
            allow_loopback: origin.is_some(),
            retry_delay: timing.retry_delay,
            retry_budget: timing.deadline,
        };
        Ok(Self { registry, acceptance, agent, downloads, origin, key, timing, out })
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
            if let Auth::BodyKey { field } = &op.auth {
                let key = self.key.ok_or_else(|| Failure::new(3, "auth", "no API key selected"))?;
                *request.body_mut() =
                    serde_json::to_vec(&json!({field:key.expose()})).expect("a JSON value serializes");
                request.headers_mut().insert("content-type", ureq::http::HeaderValue::from_static("application/json"));
            }
            self.out.trace(&format!("{} {}", request.method(), op.path));
            let request_timeout = request_timeout(call);
            let timeout = call.timeout().map_or(request_timeout, |remaining| remaining.min(request_timeout));
            let attempt = match request.with_agent(&self.agent).configure().timeout_global(Some(timeout)).run() {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if (200..300).contains(&status) && op.safety == updraft_registry::Safety::Unsafe {
                        self.acceptance.borrow_mut().record();
                    }
                    self.out.trace(&format!("HTTP {status}"));
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|s| s.to_str().ok())
                        .map(|s| s.trim().to_owned());
                    match response.body_mut().with_config().limit(64 * 1024 * 1024).read_to_vec() {
                        Ok(body) => {
                            if (200..300).contains(&status)
                                && matches!(op.response, Payload::Json { .. })
                                && status != 204
                                && !body.is_empty()
                                && serde_json::from_slice::<Value>(&body).is_err()
                            {
                                if op.safety == updraft_registry::Safety::Unsafe {
                                    return Err(Failure::unknown("invalid JSON response")
                                        .with("detail", json!(String::from_utf8_lossy(&body))));
                                }
                                Attempt::Transport {
                                    sent: true,
                                    message: format!("invalid JSON: {}", String::from_utf8_lossy(&body)),
                                }
                            } else {
                                Attempt::Response { status, retry_after, body }
                            }
                        }
                        Err(error) if !(200..300).contains(&status) => {
                            Attempt::Response { status, retry_after, body: error.to_string().into_bytes() }
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
                Attempt::Redirect(_) => "redirect".into(),
                Attempt::PageOverflow { .. } => "page exceeds limit".into(),
            };
            match judge(op, attempt, history) {
                Verdict::Redirect(_) => unreachable!("API requests do not follow redirects"),
                Verdict::Success(body) => {
                    if body.is_empty() {
                        return Ok(Reply::Empty);
                    }
                    return match &op.response {
                        Payload::Json { .. } if !body.is_empty() => serde_json::from_slice(&body)
                            .map(Reply::Json)
                            .map_err(|_| Failure::unavailable("invalid JSON response")),
                        Payload::Json { .. } | Payload::Empty => Ok(Reply::Empty),
                        Payload::Text | Payload::Binary => {
                            Err(Failure::unavailable("unsupported registry response payload"))
                        }
                    };
                }
                Verdict::Retry { after_seconds, history: next } => {
                    history = next;
                    let delay = self.timing.retry_delay.unwrap_or(Duration::from_secs(after_seconds.into()));
                    let attempts = op.retry.as_ref().map_or(DEFAULT_ATTEMPTS, |r| r.attempts);
                    self.out.note(&format!(
                        "{reason}, retrying in {} s (attempt {} of {attempts})",
                        delay.as_secs_f64(),
                        history.attempts + 1
                    ));
                    std::thread::sleep(call.timeout().map_or(delay, |remaining| delay.min(remaining)));
                }
                Verdict::Fail(failure) => return Err(failure),
            }
        }
    }
    pub fn send_all(&self, call: Call<'_>, max_items: Option<usize>, max_pages: u32) -> Result<Value, Failure> {
        let p = call.op().pagination.as_ref().ok_or_else(|| Failure::usage("operation does not declare pagination"))?;
        if max_items == Some(0) {
            return Err(Failure::usage("--max-items must be positive"));
        }
        let mut call = if let Some(limit) = max_items.filter(|_| call.page_size().is_some()) {
            call.limit_page(limit)?
        } else {
            call
        };
        let mut result = Value::Null;
        let mut items = vec![];
        let mut tokens = BTreeSet::new();
        let mut resize_history = History::default();
        let mut pages = 0;
        loop {
            let page = self.send(&call)?.json()?;
            let page_items = page[&p.items_field]
                .as_array()
                .ok_or_else(|| Failure::unavailable(format!("page has no {} array", p.items_field)))?;
            if let Some(remaining) =
                max_items.map(|limit| limit - items.len()).filter(|remaining| page_items.len() > *remaining)
            {
                let can_resize = p.size_param.is_some() && call.page_size().is_none_or(|size| size > remaining);
                match judge(call.op(), Attempt::PageOverflow { can_resize }, resize_history) {
                    Verdict::Retry { after_seconds, history } => {
                        resize_history = history;
                        std::thread::sleep(
                            self.timing.retry_delay.unwrap_or(Duration::from_secs(after_seconds.into())),
                        );
                        call = call.limit_page(remaining)?;
                        continue;
                    }
                    Verdict::Fail(failure) => return Err(failure.with("detail", page)),
                    _ => unreachable!("page overflow returns retry or failure"),
                }
            }
            pages += 1;
            items.extend(page_items.iter().cloned());
            if result.is_null() {
                result = page.clone();
            }
            let token = page[&p.next_field].as_str().unwrap_or_default();
            result[&p.next_field] = json!(token);
            if token.is_empty() || pages >= max_pages || max_items.is_some_and(|limit| items.len() >= limit) {
                break;
            }
            if !tokens.insert(token.to_owned()) {
                return Err(Failure::unavailable("server repeated a pagination token"));
            }
            call = call.next_page(
                token,
                max_items.map(|limit| limit - items.len()).filter(|remaining| *remaining < page_items.len()),
            )?;
            resize_history = History::default();
        }
        result[&p.items_field] = json!(items);
        Ok(result)
    }
    pub fn finish(&self, op: &Op, first: Value) -> Result<Value, Failure> {
        let Some(completion) = &op.completion else { return Ok(first) };
        let until = Instant::now() + self.timing.deadline;
        let mut value = first;
        let mut path = value["path"].as_str().map(str::to_owned);
        let mut polled = false;
        loop {
            if value.is_object()
                && value["path"].is_null()
                && let Some(path) = &path
            {
                value["path"] = json!(path);
            }
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
            let (reader, captures) = self.registry.reader_of(op, path).map_err(|_| {
                Failure::unavailable(format!("no reader matches task path {path}")).with("path", json!(path))
            })?;
            let call = Call::new(reader, decode_captures(captures.0), vec![], None)?;
            let pending = |detail| {
                Failure::new(
                    8,
                    "pending",
                    format!("resource is not done after {} s", self.timing.deadline.as_secs_f64()),
                )
                .with("detail", detail)
            };
            if Instant::now() >= until {
                return Err(pending(value));
            }
            if polled {
                std::thread::sleep(self.timing.poll_interval.min(until.saturating_duration_since(Instant::now())));
            }
            polled = true;
            if Instant::now() >= until {
                continue;
            }
            let call = call.with_timeout(until.saturating_duration_since(Instant::now()));
            value = match self.send(&call) {
                Ok(reply) => reply.json()?,
                Err(mut failure) => {
                    if Instant::now() >= until {
                        failure.code = 8;
                        failure.error["kind"] = json!("pending");
                    }
                    return Err(failure.with("last", value));
                }
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
impl Executor<'_> {
    pub fn resume(&self, command: String) {
        self.acceptance.borrow_mut().resume(command);
    }
    pub fn note(&self, message: &str) {
        self.out.note(message);
    }
    pub fn download(&self, location: &str, to: &std::path::Path, overwrite: bool) -> Result<u64, Failure> {
        self.downloads.save(location, to, overwrite, self.out)
    }
}
fn request_timeout(call: &Call<'_>) -> Duration {
    Duration::from_secs(if call.has_binary_body() {
        600
    } else if call.op().method == Method::Get {
        30
    } else {
        120
    })
}
struct Downloads {
    agent: ureq::Agent,
    allow_loopback: bool,
    retry_delay: Option<Duration>,
    retry_budget: Duration,
}
impl Downloads {
    fn save(&self, location: &str, to: &std::path::Path, overwrite: bool, out: &Output) -> Result<u64, Failure> {
        use std::io::Seek;
        let retry_until = Instant::now() + self.retry_budget;
        let mut url = url::Url::parse(location).map_err(|_| Failure::unavailable("delivery location is not a URL"))?;
        let parent = to.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| std::path::Path::new("."));
        if to.exists() && !overwrite {
            return Err(Failure::usage("output already exists; use --overwrite"));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| Failure::usage("cannot create a temporary output file"))?;
        for redirects in 0..=5 {
            let test_http = self.allow_loopback
                && url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            if (url.scheme() != "https" && !test_http) || !url.username().is_empty() || url.password().is_some() {
                return Err(Failure::unavailable("download requires HTTPS without URL credentials"));
            }
            url.set_fragment(None);
            let mut history = History::default();
            loop {
                temporary
                    .as_file_mut()
                    .set_len(0)
                    .and_then(|()| temporary.rewind())
                    .map_err(|_| Failure::unavailable("cannot reset the temporary download file"))?;
                let mut saved = 0;
                let attempt = match self.agent.get(url.as_str()).call() {
                    Ok(mut response) => {
                        let status = response.status().as_u16();
                        let retry_after = response
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .map(|v| v.trim().to_owned());
                        if (200..300).contains(&status) {
                            match std::io::copy(&mut response.body_mut().as_reader(), temporary.as_file_mut()) {
                                Ok(bytes) => {
                                    saved = bytes;
                                    Attempt::Response { status, retry_after, body: vec![] }
                                }
                                Err(_) => Attempt::Transport {
                                    sent: true,
                                    message: "download body was lost; the output was not replaced".into(),
                                },
                            }
                        } else if matches!(status, 301 | 302 | 303 | 307 | 308) {
                            Attempt::Redirect(
                                response
                                    .headers()
                                    .get("location")
                                    .and_then(|v| v.to_str().ok())
                                    .unwrap_or_default()
                                    .into(),
                            )
                        } else {
                            let body = response
                                .body_mut()
                                .with_config()
                                .limit(64 * 1024 * 1024)
                                .read_to_vec()
                                .unwrap_or_default();
                            Attempt::Response { status, retry_after, body }
                        }
                    }
                    Err(error) => Attempt::Transport { sent: true, message: error.to_string() },
                };
                match judge_read(attempt, history) {
                    Verdict::Success(_) => {
                        temporary.as_file().sync_all().map_err(|_| Failure::usage("cannot flush the output file"))?;
                        let result = if overwrite { temporary.persist(to) } else { temporary.persist_noclobber(to) };
                        result.map_err(|_| Failure::usage("cannot rename the temporary output file"))?;
                        return Ok(saved);
                    }
                    Verdict::Redirect(location) => {
                        if redirects == 5 {
                            return Err(Failure::unavailable("download exceeded five redirects"));
                        }
                        if location.is_empty() {
                            return Err(Failure::unavailable("download redirect has no location"));
                        }
                        url = url
                            .join(&location)
                            .map_err(|_| Failure::unavailable("download redirect has an invalid URL"))?;
                        break;
                    }
                    Verdict::Retry { after_seconds, history: next } => {
                        history = next;
                        let delay = self.retry_delay.unwrap_or(Duration::from_secs(after_seconds.into()));
                        out.note(&format!(
                            "download request failed, retrying in {} s (attempt {} of {DEFAULT_ATTEMPTS})",
                            delay.as_secs_f64(),
                            history.attempts + 1
                        ));
                        let remaining = retry_until.saturating_duration_since(Instant::now());
                        std::thread::sleep(delay.min(remaining));
                        if Instant::now() >= retry_until {
                            return Err(Failure::unavailable("download retry deadline passed"));
                        }
                    }
                    Verdict::Fail(mut failure) => {
                        if failure.code == 3 {
                            failure.code = 4;
                            failure.error["kind"] = json!("rejected");
                        }
                        return Err(failure);
                    }
                }
            }
        }
        unreachable!("the redirect limit returns within the loop")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_success_ignores_every_declared_payload_kind() {
        let server = httpmock::MockServer::start();
        let m = server.mock(|_, t| {
            t.status(200).body("");
        });
        let output = Output::new(false);
        let acceptance = std::cell::RefCell::new(Acceptance::default());
        let ex = Executor::new(
            &crate::REGISTRY,
            Some(server.base_url()),
            None,
            Timing { deadline: Duration::ZERO, poll_interval: Duration::ZERO, retry_delay: Some(Duration::ZERO) },
            &output,
            &acceptance,
        )
        .unwrap();
        for response in
            [Payload::Empty, Payload::Json { shape: updraft_registry::Shape::Any }, Payload::Text, Payload::Binary]
        {
            let mut op = crate::REGISTRY.op("universes.get-place-universe").unwrap().clone();
            op.response = response;
            let call = Call::new(&op, vec![], vec![("placeId".into(), "42".into())], None).unwrap();
            assert!(ex.send(&call).unwrap().json().unwrap().is_null(), "empty 2xx bodies ignore payload metadata");
        }
        m.assert_calls(4);
    }
    #[test]
    fn download_needs_no_registry_operation_or_key() {
        let server = httpmock::MockServer::start();
        let m = server.mock(|w, t| {
            w.path("/file").header_missing("x-api-key");
            t.body("bytes");
        });
        let output = Output::new(false);
        let acceptance = std::cell::RefCell::new(Acceptance::default());
        let ex = Executor::new(
            &crate::REGISTRY,
            Some(server.base_url()),
            None,
            Timing { deadline: Duration::ZERO, poll_interval: Duration::ZERO, retry_delay: Some(Duration::ZERO) },
            &output,
            &acceptance,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asset");
        assert_eq!(ex.download(&format!("{}/file", server.base_url()), &path, false).unwrap(), 5);
        assert_eq!(std::fs::read(&path).unwrap(), b"bytes");
        m.assert_calls(1);
    }
    #[test]
    fn download_reads_have_idle_budgets_and_binary_uploads_have_ten_minutes() {
        let output = Output::new(false);
        let acceptance = std::cell::RefCell::new(Acceptance::default());
        let ex = Executor::new(
            &crate::REGISTRY,
            None,
            None,
            Timing {
                deadline: Duration::from_secs(300),
                poll_interval: Duration::ZERO,
                retry_delay: Some(Duration::ZERO),
            },
            &output,
            &acceptance,
        )
        .unwrap();
        let publish = crate::REGISTRY.op("universes.create-place-version").unwrap();
        let binary = Call::new(
            publish,
            vec![],
            vec![("universeId".into(), "1".into()), ("placeId".into(), "2".into())],
            Some(crate::call::BodyInput::Binary { content_type: "application/octet-stream".into(), bytes: vec![1] }),
        )
        .unwrap();
        assert_eq!(request_timeout(&binary), Duration::from_secs(600), "binary calls select the longer request budget");
        let read = Call::new(
            crate::REGISTRY.op("assets.get-operation").unwrap(),
            vec![],
            vec![("operationId".into(), "x".into())],
            None,
        )
        .unwrap();
        assert_eq!(request_timeout(&read), Duration::from_secs(30), "polls retain short budgets");
        assert!(ex.downloads.agent.config().timeouts().global.is_none(), "active downloads have no total duration cap");
        assert_eq!(
            ex.downloads.agent.config().timeout_per_read(),
            Some(Duration::from_secs(30)),
            "a stalled read still times out"
        );
        assert_eq!(
            ex.agent.config().timeouts().global,
            Some(Duration::from_secs(600)),
            "binary transfers can run for ten minutes"
        );
    }
}
