//! The judge's post and its occupant (spec `2026-09-26-autopilot-modo-juiz-design.md` D1, D3,
//! D10): one POST to TypeSafe per call, the answers read by question name, and every failure an
//! error, never an answer.

use std::collections::BTreeMap;
#[cfg(any(test, feature = "testkit"))]
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use super::Question;

/// D3: TypeSafe's endpoint and the model that measured best (0.875 against `jev-preview`'s
/// 0.812). `model` is mandatory: without it the answer is a 422, and the bare name `jev` is
/// refused.
pub const JEV_BASE_URL: &str = "https://api.typesafe.ai/v1";
pub const JUDGE_MODEL: &str = "jev-latest";
/// The key's name in the credential store, spelled like the rest of `machine_config::SECRETS`
/// (the spec wrote `typesafe_api_key`; every key there is kebab-case). `SECRETS` uses this constant.
pub const TYPESAFE_KEY: &str = "typesafe-api-key";
/// The client's own timeout. The judge's whole budget in the hook is `judge::JUDGE_DEADLINE`,
/// which bounds this call and everything around it.
pub const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);
/// $0.042 per million input tokens; the output is free.
pub const PRICE_PER_MILLION_INPUT_TOKENS_USD: f64 = 0.042;

/// D1: the post. Handed a `state` and the questions, it answers each question's probability by
/// the question's `key`, or says why it could not. `JevJudge` is the only occupant; the trait is
/// what lets another be seated, and spec B add questions, without touching the autopilot.
#[async_trait::async_trait]
pub trait Judge: Send + Sync {
    /// Recorded on a verdict when the answer does not name its own model.
    fn model(&self) -> &str;
    async fn ask(&self, state: &str, questions: &[Question]) -> Result<Answers, JudgeError>;
}

/// Every question's probability, by the question's `key`. A question with no answer is never
/// here: its absence is `JudgeError::Missing` (D10).
#[derive(Debug, Clone, PartialEq)]
pub struct Answers {
    pub probabilities: BTreeMap<&'static str, f64>,
    pub input_tokens: Option<i64>,
    pub model: Option<String>,
}

/// D10: each of these leaves the classifier to decide alone, and the reason goes into `error`.
#[derive(Debug, Clone, PartialEq)]
pub enum JudgeError {
    /// No key in the credential store, or the store could not be read.
    NoKey(String),
    /// D11: every permit is in flight; the call was never made.
    Busy,
    Timeout,
    Http(u16),
    Transport(String),
    InvalidJson,
    /// A missing probability is an error and never an approval.
    Missing(&'static str),
    OutOfRange(&'static str),
}

impl std::fmt::Display for JudgeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoKey(reason) => write!(formatter, "no key: {reason}"),
            Self::Busy => write!(formatter, "busy: every permit is in flight"),
            Self::Timeout => write!(formatter, "timeout"),
            Self::Http(status) => write!(formatter, "http {status}"),
            Self::Transport(reason) => write!(formatter, "transport: {reason}"),
            Self::InvalidJson => write!(formatter, "invalid json"),
            Self::Missing(key) => write!(formatter, "missing {key}"),
            Self::OutOfRange(key) => write!(formatter, "out of range {key}"),
        }
    }
}

impl JudgeError {
    /// Whether the request may have reached TypeSafe, and may be billed. A missing key and a
    /// missing permit are certain not to have; every other failure happened on or after the wire.
    pub fn may_have_been_billed(&self) -> bool {
        !matches!(self, Self::NoKey(_) | Self::Busy)
    }
}

/// The body of the one POST.
fn request_body(state: &str, questions: &[Question]) -> Value {
    let asked: serde_json::Map<String, Value> = questions
        .iter()
        .map(|question| {
            (
                question.key.to_owned(),
                serde_json::json!({"type": "noul", "instructions": question.instructions}),
            )
        })
        .collect();
    serde_json::json!({"model": JUDGE_MODEL, "state": state, "questions": asked})
}

/// PURE: every question's answer, or the D10 error naming the first question whose answer was
/// missing or out of `[0, 1]`.
fn parse_answers(payload: &Value, questions: &[Question]) -> Result<Answers, JudgeError> {
    let mut probabilities = BTreeMap::new();
    for question in questions {
        let p = payload
            .get("answers")
            .and_then(|answers| answers.get(question.key))
            .and_then(|answer| answer.get("noul"))
            .and_then(Value::as_f64)
            .ok_or(JudgeError::Missing(question.key))?;
        if !(p.is_finite() && (0.0..=1.0).contains(&p)) {
            return Err(JudgeError::OutOfRange(question.key));
        }
        probabilities.insert(question.key, p);
    }
    Ok(Answers {
        probabilities,
        input_tokens: payload
            .get("usage")
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(Value::as_i64),
        model: payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

pub enum KeySource {
    /// D3: the keyring, read per call with `spawn_blocking` (the keyring is synchronous), the way
    /// `github.rs` reads its token. `secrets.rs` refuses files and environment variables.
    Keyring,
    #[cfg(any(test, feature = "testkit"))]
    Fixed(Option<String>),
}

/// D1/D3: the Jev, called directly: one POST, so no llm-router process has to be alive for every
/// tool call.
pub struct JevJudge {
    /// Built on first use, not at startup: a TLS or proxy misconfiguration is then a `Transport`
    /// error on a call the classifier survives, instead of a panic that stops the daemon for a
    /// feature most projects never switch on.
    client: OnceLock<Result<reqwest::Client, String>>,
    timeout: Duration,
    base_url: String,
    key: KeySource,
}

impl JevJudge {
    pub fn new(base_url: &str) -> Self {
        Self::with(base_url, KeySource::Keyring, CLIENT_TIMEOUT)
    }

    pub fn with(base_url: &str, key: KeySource, timeout: Duration) -> Self {
        Self {
            client: OnceLock::new(),
            timeout,
            base_url: base_url.trim_end_matches('/').to_owned(),
            key,
        }
    }

    #[cfg(any(test, feature = "testkit"))]
    pub fn for_tests(base_url: &str, key: Option<&str>, timeout: Duration) -> Self {
        Self::with(base_url, KeySource::Fixed(key.map(str::to_owned)), timeout)
    }

    fn client(&self) -> Result<&reqwest::Client, JudgeError> {
        self.client
            .get_or_init(|| {
                // Spec A D1: the state goes to the Jev and nowhere else. A redirect would re-POST it
                // to whatever host the response names, so none is followed.
                reqwest::Client::builder()
                    .timeout(self.timeout)
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| JudgeError::Transport(error.clone()))
    }

    async fn key(&self) -> Result<String, JudgeError> {
        match &self.key {
            KeySource::Keyring => {
                match tokio::task::spawn_blocking(|| crate::secrets::load_secret(TYPESAFE_KEY))
                    .await
                {
                    Ok(Ok(Some(key))) if !key.is_empty() => Ok(key),
                    Ok(Ok(_)) => Err(JudgeError::NoKey("no TypeSafe key is stored".to_owned())),
                    Ok(Err(error)) => Err(JudgeError::NoKey(error.to_string())),
                    Err(error) => Err(JudgeError::NoKey(error.to_string())),
                }
            }
            #[cfg(any(test, feature = "testkit"))]
            KeySource::Fixed(key) => key
                .clone()
                .ok_or_else(|| JudgeError::NoKey("no TypeSafe key is stored".to_owned())),
        }
    }
}

#[async_trait::async_trait]
impl Judge for JevJudge {
    fn model(&self) -> &str {
        JUDGE_MODEL
    }

    async fn ask(&self, state: &str, questions: &[Question]) -> Result<Answers, JudgeError> {
        let key = self.key().await?;
        let response = self
            .client()?
            .post(format!("{}/systemone", self.base_url))
            .bearer_auth(key)
            .json(&request_body(state, questions))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    JudgeError::Timeout
                } else {
                    JudgeError::Transport(error.to_string())
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(JudgeError::Http(status.as_u16()));
        }
        let payload: Value = response.json().await.map_err(|error| {
            if error.is_timeout() {
                JudgeError::Timeout
            } else {
                JudgeError::InvalidJson
            }
        })?;
        parse_answers(&payload, questions)
    }
}

/// A judge that answers what it was told to, counts its calls and keeps the last `state` it saw.
/// No network: this is the D1 seam the hook's tests use; `JevJudge`'s own tests use a fake HTTP
/// server instead.
#[cfg(any(test, feature = "testkit"))]
pub struct ScriptedJudge {
    /// Every key's answer; a key not here is answered 0.5, as before.
    reply: Result<BTreeMap<&'static str, f64>, JudgeError>,
    delay: Option<Duration>,
    calls: std::sync::atomic::AtomicUsize,
    pub last_state: std::sync::Mutex<Option<String>>,
    /// Spec B: the keys of each call, in order - how a test proves a question was NOT asked.
    asked: std::sync::Mutex<Vec<Vec<&'static str>>>,
}

#[cfg(any(test, feature = "testkit"))]
impl ScriptedJudge {
    fn build(
        reply: Result<BTreeMap<&'static str, f64>, JudgeError>,
        delay: Option<Duration>,
    ) -> Arc<Self> {
        Arc::new(Self {
            reply,
            delay,
            calls: Default::default(),
            last_state: Default::default(),
            asked: Default::default(),
        })
    }

    /// Answers A's two questions; any other question is answered 0.5.
    pub fn answering(p_in_scope: f64, p_safe: f64) -> Arc<Self> {
        Self::answering_keys(&[(super::IN_SCOPE, p_in_scope), (super::SAFE, p_safe)])
    }

    pub fn answering_keys(answers: &[(&'static str, f64)]) -> Arc<Self> {
        Self::build(Ok(answers.iter().copied().collect()), None)
    }

    pub fn answering_keys_slowly(delay: Duration, answers: &[(&'static str, f64)]) -> Arc<Self> {
        Self::build(Ok(answers.iter().copied().collect()), Some(delay))
    }

    pub fn failing(error: JudgeError) -> Arc<Self> {
        Self::build(Err(error), None)
    }

    pub fn slow(delay: Duration) -> Arc<Self> {
        Self::answering_keys_slowly(delay, &[(super::IN_SCOPE, 0.99), (super::SAFE, 0.99)])
    }

    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn asked_keys(&self) -> Vec<Vec<&'static str>> {
        self.asked.lock().unwrap().clone()
    }
}

#[cfg(any(test, feature = "testkit"))]
#[async_trait::async_trait]
impl Judge for ScriptedJudge {
    fn model(&self) -> &str {
        "scripted"
    }

    async fn ask(&self, state: &str, questions: &[Question]) -> Result<Answers, JudgeError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.last_state.lock().unwrap() = Some(state.to_owned());
        self.asked
            .lock()
            .unwrap()
            .push(questions.iter().map(|q| q.key).collect());
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        let map = self.reply.clone()?;
        Ok(Answers {
            probabilities: questions
                .iter()
                .map(|q| (q.key, *map.get(q.key).unwrap_or(&0.5)))
                .collect(),
            input_tokens: Some(700),
            model: Some(JUDGE_MODEL.to_owned()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Spec B's tests need answers by key, a delay, and the keys each call asked.
    #[tokio::test]
    async fn the_scripted_judge_answers_by_key_and_remembers_what_it_was_asked() {
        let judge = ScriptedJudge::answering_keys(&[("off_task", 0.9), ("needed", 0.2)]);
        let questions = [
            super::super::Question {
                key: "off_task",
                instructions: "x",
            },
            super::super::Question {
                key: "needed",
                instructions: "y",
            },
            super::super::Question {
                key: "avoidable",
                instructions: "z",
            },
        ];
        let answers = judge.ask("s", &questions).await.unwrap();
        assert_eq!(answers.probabilities["off_task"], 0.9);
        assert_eq!(
            answers.probabilities["avoidable"], 0.5,
            "unscripted keys answer 0.5, as A's do"
        );
        assert_eq!(
            judge.asked_keys(),
            vec![vec!["off_task", "needed", "avoidable"]]
        );
        let a = ScriptedJudge::answering(0.97, 0.91);
        let answers = a.ask("s", crate::judge::JUDGE_QUESTIONS).await.unwrap();
        assert_eq!(
            (
                answers.probabilities["in_scope"],
                answers.probabilities["safe"]
            ),
            (0.97, 0.91),
            "A's constructor is unchanged"
        );
    }

    async fn serve(handler: axum::routing::MethodRouter) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = axum::Router::new().route("/v1/systemone", handler);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{address}/v1")
    }

    fn answers(in_scope: Value, safe: Value) -> Value {
        json!({
            "answers": {"in_scope": {"noul": in_scope}, "safe": {"noul": safe}},
            "usage": {"input_tokens": 700, "output_tokens": 0},
            "model": "jev-latest"
        })
    }

    /// D3/D8: one POST with the model, every question asked as `noul`, and the key as a bearer;
    /// the answers come back keyed by the question's name.
    #[tokio::test]
    async fn the_jev_is_asked_every_question_and_answers_by_name() {
        let seen = Arc::new(Mutex::new(None));
        let recorder = seen.clone();
        let url = serve(post(
            move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                let recorder = recorder.clone();
                async move {
                    let bearer = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_owned();
                    *recorder.lock().unwrap() = Some((bearer, body));
                    axum::Json(answers(json!(0.97), json!(0.91)))
                }
            },
        ))
        .await;
        let judge = JevJudge::for_tests(&url, Some("k-test"), Duration::from_secs(2));

        let answer = judge
            .ask("TASK:\nx\n", crate::judge::JUDGE_QUESTIONS)
            .await
            .unwrap();

        assert_eq!(answer.probabilities.get("in_scope"), Some(&0.97));
        assert_eq!(answer.probabilities.get("safe"), Some(&0.91));
        assert_eq!(answer.input_tokens, Some(700));
        let (bearer, body) = seen.lock().unwrap().clone().unwrap();
        assert_eq!(bearer, "Bearer k-test");
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["state"], "TASK:\nx\n");
        assert_eq!(body["questions"]["in_scope"]["type"], "noul");
        assert_eq!(
            body["questions"]["in_scope"]["instructions"],
            crate::judge::QUESTION_IN_SCOPE
        );
        assert_eq!(
            body["questions"]["safe"]["instructions"],
            crate::judge::QUESTION_SAFE
        );
    }

    /// D10: every way the call can fail is an error, and none of them is an answer.
    #[tokio::test]
    async fn every_failure_of_the_call_is_an_error_and_never_an_answer() {
        let refused = serve(post(|| async {
            (
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "model is required",
            )
        }))
        .await;
        let garbled = serve(post(|| async { "not json" })).await;
        let half = serve(post(|| async {
            axum::Json(json!({"answers": {"in_scope": {"noul": 0.9}}}))
        }))
        .await;
        let wild = serve(post(|| async {
            axum::Json(answers(json!(0.9), json!(1.7)))
        }))
        .await;
        let slow = serve(post(|| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            axum::Json(answers(json!(0.9), json!(0.9)))
        }))
        .await;
        // Two seconds for every server that answers at once (a short client timeout on a busy
        // test machine fails a test for the wrong reason); 300 ms only for the slow one.
        let ask = |url: String, timeout: Duration| async move {
            JevJudge::for_tests(&url, Some("k"), timeout)
                .ask("s", crate::judge::JUDGE_QUESTIONS)
                .await
                .unwrap_err()
        };
        let quick = Duration::from_secs(2);
        assert_eq!(ask(refused, quick).await, JudgeError::Http(422));
        assert_eq!(ask(garbled, quick).await, JudgeError::InvalidJson);
        assert_eq!(ask(half, quick).await, JudgeError::Missing("safe"));
        assert_eq!(ask(wild, quick).await, JudgeError::OutOfRange("safe"));
        assert_eq!(
            ask(slow, Duration::from_millis(300)).await,
            JudgeError::Timeout
        );
    }

    /// D3/D10: no key, no request — nothing leaves the machine without one.
    #[tokio::test]
    async fn without_a_key_nothing_is_sent() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let url = serve(post(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { axum::Json(answers(json!(0.9), json!(0.9))) }
        }))
        .await;
        let error = JevJudge::for_tests(&url, None, Duration::from_secs(2))
            .ask("s", crate::judge::JUDGE_QUESTIONS)
            .await
            .unwrap_err();
        assert!(matches!(error, JudgeError::NoKey(_)));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
