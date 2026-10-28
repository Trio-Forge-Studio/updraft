use serde_json::{Value, json};
use updraft_registry::{Op, RetryPolicy, Safety};

pub const DEFAULT_ATTEMPTS: u8 = 3;

pub const UNKNOWN_HINT: &str = "Check the resource in Creations before repeating this request.";

#[derive(Default)]
pub struct Acceptance {
    accepted: bool,
    resume: Option<String>,
}
impl Acceptance {
    pub fn record(&mut self) {
        self.accepted = true;
    }
    pub fn resume(&mut self, command: String) {
        self.resume = Some(command);
    }
    pub fn finish<T>(&self, result: Result<T, Failure>) -> Result<T, Failure> {
        result.map_err(|mut failure| {
            if let Some(resume) = &self.resume {
                failure.error["resume"] = json!(resume);
            }
            if !self.accepted || failure.code == 6 {
                return failure;
            }
            if self.resume.is_some() {
                failure.code = 8;
                failure.error["kind"] = json!("pending");
            } else {
                failure.code = 5;
                failure.error["kind"] = json!("outcome_unknown");
                failure
                    .error
                    .as_object_mut()
                    .expect("a failure is an object")
                    .entry("hint")
                    .or_insert(json!(UNKNOWN_HINT));
            }
            failure
        })
    }
}
#[derive(Debug)]
pub struct Failure {
    pub code: u8,
    pub error: Value,
}
impl Failure {
    pub fn new(code: u8, kind: &str, message: impl Into<String>) -> Self {
        Self { code, error: json!({"kind":kind,"message":message.into()}) }
    }
    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(2, "usage", message)
    }
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(7, "unavailable", message)
    }
    pub fn unknown(message: impl Into<String>) -> Self {
        Self::new(5, "outcome_unknown", message).with("hint", json!(UNKNOWN_HINT))
    }
    pub fn remote(message: impl Into<String>, detail: Value) -> Self {
        Self::new(6, "remote_failure", message).with("detail", detail)
    }
    pub fn with(mut self, field: &str, value: Value) -> Self {
        self.error[field] = value;
        self
    }
    pub fn document(&self) -> Value {
        json!({"error":self.error})
    }
}

pub enum Attempt {
    PageOverflow { can_resize: bool },
    Redirect(String),
    Response { status: u16, retry_after: Option<String>, body: Vec<u8> },
    Transport { sent: bool, message: String },
}
#[derive(Clone, Copy, Default)]
pub struct History {
    pub attempts: u8,
    pub uncertain: bool,
}
pub enum Verdict {
    Redirect(String),
    Success(Vec<u8>),
    Retry { after_seconds: u32, history: History },
    Fail(Failure),
}

pub fn judge(op: &Op, attempt: Attempt, history: History) -> Verdict {
    judge_policy(op.safety, op.retry.as_ref(), attempt, history)
}
pub fn judge_read(attempt: Attempt, history: History) -> Verdict {
    judge_policy(Safety::Read, None, attempt, history)
}
fn judge_policy(safety: Safety, retry: Option<&RetryPolicy>, attempt: Attempt, history: History) -> Verdict {
    let unsafe_write = safety == Safety::Unsafe;
    let (failure, retryable, uncertain, retry_after) = match attempt {
        Attempt::PageOverflow { can_resize } => {
            if unsafe_write {
                return Verdict::Fail(Failure::unknown(
                    "the page exceeds the requested limit; the request will not be repeated",
                ));
            }
            if can_resize && history.attempts == 0 {
                return Verdict::Retry { after_seconds: 0, history: History { attempts: 1, uncertain: false } };
            }
            return Verdict::Fail(Failure::usage(
                "the page cannot fit the requested item limit; use a smaller page size or omit --max-items",
            ));
        }
        Attempt::Redirect(location) => return Verdict::Redirect(location),
        Attempt::Response { status, retry_after, body } => {
            if (200..300).contains(&status) {
                return Verdict::Success(body);
            }
            let message = format!("HTTP {status}");
            let detail =
                serde_json::from_slice::<Value>(&body).unwrap_or_else(|_| json!(String::from_utf8_lossy(&body)));
            let failure = match status {
                401 | 403 => Failure::new(3, "auth", message),
                429 => Failure::unavailable(message),
                400..500 => Failure::new(4, "rejected", message),
                _ if unsafe_write => Failure::unknown(message),
                _ => Failure::unavailable(message),
            }
            .with("status", json!(status))
            .with("detail", detail);
            let retryable = retry.map_or(!unsafe_write && (status == 429 || status >= 500), |policy| {
                policy.statuses.iter().any(|class| class.matches(status))
            });
            (failure, retryable, unsafe_write && status >= 500, retry_after)
        }
        Attempt::Transport { sent, message } => {
            let failure = if sent && unsafe_write { Failure::unknown(message) } else { Failure::unavailable(message) };
            (failure, !unsafe_write, unsafe_write && sent, None)
        }
    };
    let uncertain = history.uncertain || uncertain;
    let requested =
        retry_after.as_deref().filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()));
    let seconds = requested.and_then(|value| value.parse::<u64>().ok());
    let capped = requested.is_some() && seconds.is_none_or(|wait| wait > 300);
    let failure = if capped {
        failure.with("retryAfter", seconds.map_or_else(|| json!(requested), |seconds| json!(seconds)))
    } else {
        failure
    };
    let attempts = retry.map_or(DEFAULT_ATTEMPTS, |policy| policy.attempts);
    if retryable && !capped && history.attempts + 1 < attempts {
        let delay = retry.map_or(if history.attempts == 0 { 1 } else { 4 }, |policy| policy.delay_seconds);
        return Verdict::Retry {
            after_seconds: seconds
                .map_or(delay, |wait| u32::try_from(wait).expect("retry waits are capped at 300 seconds")),
            history: History { attempts: history.attempts + 1, uncertain },
        };
    }
    if uncertain && failure.code != 5 {
        let mut failure = failure;
        failure.code = 5;
        failure.error["kind"] = json!("outcome_unknown");
        failure.error["hint"] = json!(UNKNOWN_HINT);
        Verdict::Fail(failure)
    } else {
        Verdict::Fail(failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use updraft_registry::Registry;
    fn op(safety: Safety) -> Op {
        let r: Registry = serde_json::from_str(include_str!("../../../spec/registry.json")).unwrap();
        let mut op = r.op("assets.get").unwrap().clone();
        op.safety = safety;
        op
    }
    fn response(status: u16) -> Attempt {
        Attempt::Response { status, retry_after: None, body: b"reply".to_vec() }
    }
    fn code(verdict: Verdict) -> u8 {
        match verdict {
            Verdict::Fail(f) => f.code,
            _ => panic!("expected a terminal failure"),
        }
    }
    #[test]
    fn recovery_uses_the_latest_explicit_command() {
        let mut acceptance = Acceptance::default();
        acceptance.record();
        acceptance.resume("old command".into());
        acceptance.resume("new command".into());
        let failure = acceptance.finish::<()>(Err(Failure::unavailable("poll failed"))).unwrap_err();
        assert_eq!(failure.code, 8);
        assert_eq!(failure.error["resume"], "new command", "recovery has one explicit owner");
    }
    #[test]
    fn success_row() {
        for s in [Safety::Read, Safety::Idempotent, Safety::Unsafe] {
            assert!(matches!(judge(&op(s), response(200), History::default()), Verdict::Success(_)));
        }
    }
    #[test]
    fn redirect_row() {
        assert!(
            matches!(judge(&op(Safety::Read),Attempt::Redirect("/next".into()),History::default()),Verdict::Redirect(location) if location=="/next")
        );
    }
    #[test]
    fn page_overflow_row() {
        assert!(matches!(
            judge(&op(Safety::Read), Attempt::PageOverflow { can_resize: true }, History::default()),
            Verdict::Retry { after_seconds: 0, .. }
        ));
        assert_eq!(code(judge(&op(Safety::Unsafe), Attempt::PageOverflow { can_resize: true }, History::default())), 5);
        assert_eq!(code(judge(&op(Safety::Read), Attempt::PageOverflow { can_resize: false }, History::default())), 2);
    }
    #[test]
    fn auth_row() {
        for status in [401, 403] {
            assert_eq!(code(judge(&op(Safety::Unsafe), response(status), History::default())), 3);
        }
    }
    #[test]
    fn rejection_row() {
        assert_eq!(code(judge(&op(Safety::Unsafe), response(400), History::default())), 4);
    }
    #[test]
    fn unsafe_429_row() {
        assert_eq!(code(judge(&op(Safety::Unsafe), response(429), History::default())), 7);
    }
    #[test]
    fn unsafe_not_sent_row() {
        assert_eq!(
            code(judge(
                &op(Safety::Unsafe),
                Attempt::Transport { sent: false, message: "refused".into() },
                History::default()
            )),
            7
        );
    }
    #[test]
    fn unsafe_lost_row() {
        assert_eq!(
            code(judge(
                &op(Safety::Unsafe),
                Attempt::Transport { sent: true, message: "reset".into() },
                History::default()
            )),
            5
        );
    }
    #[test]
    fn unsafe_server_error_row() {
        assert_eq!(code(judge(&op(Safety::Unsafe), response(503), History::default())), 5);
    }
    #[test]
    fn read_retry_row() {
        for status in [429, 503] {
            assert!(matches!(
                judge(&op(Safety::Read), response(status), History::default()),
                Verdict::Retry { after_seconds: 1, .. }
            ));
        }
    }
    #[test]
    fn read_transport_retry_row() {
        assert!(matches!(
            judge(
                &op(Safety::Read),
                Attempt::Transport { sent: true, message: "reset".into() },
                History { attempts: 1, uncertain: false }
            ),
            Verdict::Retry { after_seconds: 4, .. }
        ));
    }
    #[test]
    fn exhaustion_row() {
        assert_eq!(code(judge(&op(Safety::Read), response(503), History { attempts: 2, uncertain: false })), 7);
    }
    #[test]
    fn retry_after_row() {
        assert!(matches!(
            judge(
                &op(Safety::Read),
                Attempt::Response { status: 429, retry_after: Some("9".into()), body: vec![] },
                History::default()
            ),
            Verdict::Retry { after_seconds: 9, .. }
        ));
    }
    #[test]
    fn publish_uncertainty_row() {
        let r: Registry = serde_json::from_str(include_str!("../../../spec/registry.json")).unwrap();
        let p = r.op("universes.create-place-version").unwrap();
        let Verdict::Retry { history, .. } = judge(p, response(503), History::default()) else {
            panic!("publish retries 5xx")
        };
        assert_eq!(code(judge(p, response(400), history)), 5);
        assert!(matches!(judge(p, response(200), history), Verdict::Success(_)));
        assert_eq!(code(judge(p, response(409), History { attempts: 3, uncertain: false })), 4);
    }
}
