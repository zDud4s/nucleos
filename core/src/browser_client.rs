//! Typed HTTP client for the browser sidecar's loopback API.
//!
//! The same separation `web_client.rs` has, for the same reason: `browser.rs` owns the domain and
//! must not learn that the sidecar speaks HTTP. It matters more here than there. This client is how
//! the núcleo drives a process that holds the owner's logged-in profiles, so every call carries the
//! daemon token and every call goes to loopback — there is no configuration that points it anywhere
//! else.
//!
//! # The one thing this module must not turn into an error
//!
//! A fence refusal (spec §6.2) arrives as HTTP 200 with `outcome: "refused"`, and stays a value all
//! the way up. It is an ANSWER: the agent asked for something with a consequence and was told so.
//! Mapping it to an error would make it indistinguishable from a crashed browser, and the agent's
//! natural response to a crash — retry — is the one thing it must not do with a refusal.
//!
//! # Why the placement is built here and not there
//!
//! The sidecar refuses an `/open` that does not say which profile it belongs to, because it has no
//! way to guess: one guess loses the person's logins and the other hands them to a stranger's page.
//! So `Placement` is a required argument, produced from `browser_policy::decide`, and there is no
//! constructor on it that fills in a default.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Which profile a session runs in. The wire form of `browser_policy::Profile`, kept apart from it
/// because one is a decision and the other is a directory on another process's disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRef {
    /// `"project"` or `"ephemeral"` — `browser_policy::Profile::as_str`.
    pub kind: String,
    /// The project id or the run id. The sidecar turns this into a directory name and validates it
    /// there; it is lowercased on the way out because the sidecar refuses uppercase, which it does
    /// because Windows would fold `Acme` and `acme` into one profile holding one set of cookies.
    pub id: String,
}

/// Where a session runs, and what it may load: the núcleo's decision, travelling as one value.
///
/// The two fields go together because they are one decision. A profile without its site list is a
/// browser holding the owner's logins and no rule about where they may be sent; a site list without
/// its profile is a rule nothing enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub profile: ProfileRef,
    /// Empty for an ephemeral profile, and it must be: there are no logins in it to protect, and the
    /// sidecar's fence refuses a list it would have to ignore rather than accepting a security
    /// control that does nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origins: Vec<String>,
    /// Which of those origins this profile may also SUBMIT A FORM to.
    ///
    /// A second list rather than a flag on the first, and the separation is the same argument the
    /// fence makes: reading a site and acting as the person on it are different permissions, wanted
    /// in different combinations — read the Jira and open no tickets, read the inbox and answer
    /// nothing. A person grants the second at the login, next to the first and separately from it.
    ///
    /// Empty for an ephemeral profile for a reason narrower than `origins`': a throwaway has no
    /// login in it, so there is nobody for a form to be submitted AS.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writable: Vec<String>,
}

impl Placement {
    /// The placement for a project profile and the sites it admits, none of which it may write to.
    ///
    /// Read-only by default, and every caller that means otherwise says so with
    /// [`Placement::writing_to`]. The permissive spelling is the one that has to be typed out: a
    /// constructor whose default granted writing would put the whole of this permission behind
    /// somebody remembering to pass an empty vector.
    pub fn project(project_id: &str, origins: Vec<String>) -> Self {
        Self {
            profile: ProfileRef {
                kind: crate::browser_policy::Profile::Project.as_str().to_string(),
                id: slug(project_id),
            },
            origins,
            writable: Vec::new(),
        }
    }

    /// The same placement, naming which of its origins may be submitted to.
    pub fn writing_to(mut self, writable: Vec<String>) -> Self {
        self.writable = writable;
        self
    }

    /// The placement for a throwaway. No origin list, by construction rather than by discipline.
    pub fn ephemeral(run_id: &str) -> Self {
        Self {
            profile: ProfileRef {
                kind: crate::browser_policy::Profile::Ephemeral
                    .as_str()
                    .to_string(),
                id: slug(run_id),
            },
            origins: Vec::new(),
            writable: Vec::new(),
        }
    }
}

/// Reduce an id to what the sidecar accepts as a directory name: lowercase, and `[a-z0-9_-]`.
///
/// The sidecar validates this again and refuses what it does not like — this is not a substitute for
/// that check, and must not become one. It is here so that the ordinary case (a numeric id, a uuid)
/// does not fail at the door over a capital letter, while anything genuinely strange still arrives
/// as something the far side will name and reject rather than something it will quietly accept.
fn slug(id: &str) -> String {
    id.to_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

/// Why the fence stopped something. A closed vocabulary, mirroring the sidecar's — the núcleo has to
/// tell these apart without reading prose.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Refusal {
    pub consequence: String,
    #[serde(default)]
    pub detail: String,
}

/// One browsing session.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Session {
    pub id: String,
    pub mode: String,
    pub requested_url: String,
    /// Where it actually landed. Reported separately from `requested_url` because the profile
    /// decision is a conjunction over the two (spec §5.3), and a redirect is exactly the case the
    /// allowlist exists for.
    pub final_url: String,
    #[serde(default)]
    pub title: String,
    /// Non-null when the fence stopped the navigation this session was opened for. The session
    /// exists and is addressable; it is simply empty.
    #[serde(default)]
    pub refusal: Option<Refusal>,
    /// The page had not finished arriving. Opening waits for it, bounded; this is what the sidecar
    /// says when the bound was reached, and it is the difference between a page with nothing on it
    /// and a page that had not got there yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub still_loading: bool,
    /// The HTTP status the page came back with, and 0 when nothing said.
    ///
    /// A 404 is a page: heading, sentence, search box, and every other signal saying it is fine. An
    /// agent sent to find something reads it correctly and concludes the thing is not there, when
    /// what happened is that the request failed. 0 means nothing said — a document from the
    /// back-forward cache never produces a response — and never "fine".
    #[serde(default, skip_serializing_if = "is_zero")]
    pub status: i64,
}

/// One thing on the page the agent may refer to.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Element {
    /// A handle minted by the driver ("e5"), never a CSS selector: a ref can only name something a
    /// snapshot actually showed.
    ///
    /// Defaulted because prose has none. A snapshot carries the page's words as `role: "text"`
    /// entries, and nothing in the action set does anything to a paragraph — so they arrive without
    /// a ref, and a struct that required one would fail to decode the whole snapshot rather than the
    /// one field.
    #[serde(rename = "ref", default, skip_serializing_if = "String::is_empty")]
    pub element_ref: String,
    pub role: String,
    pub name: String,
    /// What is IN it — the characters in a textbox, the number on a slider. Without it an agent that
    /// types cannot read back what it typed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    /// The accessibility properties that change what an act would MEAN: `checked`/`unchecked`,
    /// `disabled`, `expanded`/`collapsed`, `selected`, `required`, `focused`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<String>,
    /// Where a link goes — a path when it points at the page's own origin, the whole address
    /// otherwise. Without it two links called "Details" are one link, and a link that opens in a
    /// window (which the fence refuses) has no way onward, because `goto` needs an address.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
}

/// One kind of thing on the page that the accessibility tree does not carry.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Unread {
    pub kind: String,
    pub count: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Snapshot {
    pub session_id: String,
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub elements: Vec<Element>,
    /// The page continues past the last element here — the text budget ran out. Carried rather than
    /// dropped: an agent that cannot tell a short page from a cut-off one concludes the rest does not
    /// exist, which is a worse failure than being told to scroll.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Where the prose stopped, to be passed back as `text_from` to read on. Present only when
    /// `truncated` is, so its presence is the offer.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub text_next: i64,
    /// The same offer for the actionable elements, which have a budget of their own: a listing with
    /// two thousand links used to come back whole and unannounced, because the prose had fit.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub controls_next: i64,
    /// What the page tried to do for itself and the fence stopped, since this document loaded.
    ///
    /// The fence's third layer, the injected CSP, is enforced inside the renderer: no request is
    /// ever made, so the interception has nothing to pause and nothing to report. A page whose
    /// content arrives by fetch renders a shell, and a shell is a correct reading of an empty page.
    /// This is what stops the agent concluding the page is blank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<Blocked>,
    /// What is ON the page that the accessibility tree cannot express: a canvas, a video, an
    /// undescribed drawing or image.
    ///
    /// A page drawn into a canvas — a chart, a map, a PDF viewer — loads perfectly and leaves
    /// nothing in the tree, so the reading comes back short and with nothing to doubt. This does not
    /// make the drawing readable; it makes the absence legible, which is the difference between an
    /// agent concluding the answer is not there and knowing to ask a person.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unread: Vec<Unread>,
    /// The page had not finished arriving when this reading was taken.
    ///
    /// It is here because otherwise the flag could be raised and never lowered: opening said it,
    /// acting said it, and the only thing an agent can do about it — take another reading — said
    /// nothing at all. There is no `wait` verb on purpose, so the reading has to carry it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub still_loading: bool,
    /// Questions the page put to a PERSON, and the answers it was given instead.
    ///
    /// `alert`, `confirm`, `prompt` and `beforeunload` freeze the renderer until the attached
    /// debugger answers them, and the sidecar answers no — accepting would be a decision taken on
    /// somebody's behalf, on a surface the page controls. Carried here because otherwise the agent
    /// reads a page where its click did nothing and concludes the button is broken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dialogs: Vec<Dialog>,
    /// The HTTP status of the page being read, and 0 when nothing said. Same meaning as on
    /// `Session`, and here because a click or a goto replaces the document without producing a new
    /// session — so the reading is the only place the current page's status can arrive.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub status: i64,
}

/// One question the page asked a person, and the answer given on their behalf.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dialog {
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    pub answer: String,
}

/// What the injected CSP stopped: how many, and the most recent one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blocked {
    pub count: i64,
    pub consequence: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

/// The answer to an action: done, or refused with a named consequence.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActResult {
    pub outcome: String,
    #[serde(default)]
    pub refusal: Option<Refusal>,
    /// The act replaced the document, so every ref from before it names something that is gone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub navigated: bool,
    /// Where the page ended up, filled only when the act moved it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// The page it moved to had not finished arriving. Same meaning as on [`Session`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub still_loading: bool,
    /// The form submissions this act actually SENT — the ones the fence let through.
    ///
    /// Only what left. A submission the fence stopped is a refusal and not a write, and conflating
    /// the two would make the record of what an agent did as the person contain things it did not
    /// do. `browser::post_act` files these in `browser_writes` before answering the caller.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<Write>,
}

/// One annotated picture of a page: what a person would see, with the agent's own refs drawn on it.
///
/// The labels ARE the refs. Nothing here is a coordinate, and there is no verb that takes one — see
/// the sidecar's `browser.LookResult` for why that is the whole design rather than a limitation of
/// it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LookResult {
    /// The picture, base64, as it arrived. Never decoded on this side: it is passed to the model as
    /// an image block, and decoding it here would only be re-encoding it a line later.
    pub image: String,
    pub mime: String,
    /// The refs actually drawn, which is fewer than the session knows: what is scrolled out of the
    /// viewport gets no label.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
}

/// One form submission that left this machine, as much of it as is safe to keep.
///
/// The names of the fields and how many there were. Never the values — see the sidecar's
/// `browser.Write` and migration 0097 for the argument, which is the same one in both places: a form
/// carries passwords, tokens and private text, and a record of what was submitted would turn this
/// database into where every credential an agent ever types comes to rest.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Write {
    /// Where it went, in the shape `browser_sites.origin` is written in — so the join a person makes
    /// by eye is the join the database would make.
    pub origin: String,
    /// The form's action with its query removed, and the method it went with.
    pub action: String,
    pub method: String,
    /// The NAMES of the fields submitted, in document order, and how many there were in total. Two
    /// numbers on purpose: a long form is truncated to a readable list of names while the count
    /// stays true.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub field_count: i64,
    /// The act that caused it — the ref from the snapshot and the verb. The write rule's fifth
    /// condition written down rather than asserted.
    #[serde(default)]
    pub r#ref: String,
    #[serde(default)]
    pub verb: String,
}

impl ActResult {
    pub fn refused(&self) -> bool {
        self.outcome == "refused"
    }
}

/// The driver's half of asking for the wheel: the session is ready to be shown to a person.
///
/// It does not hand anything over. Spec §4.4 rule 3 puts the request in `proposals.rs` because the
/// daemon runs without a shell, and what this marks in the sidecar is the other half of rule 1 — from
/// here on the agent's actions are refused rather than queued.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandoffTicket {
    pub session_id: String,
    pub mode: String,
    pub url: String,
    #[serde(default)]
    pub reason: String,
}

/// The window, open, with a person in front of it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Wheel {
    pub session: String,
    pub mode: String,
    pub url: String,
    /// The agent sessions that were closed to make room. Spec §4.1 allows one browser per profile, so
    /// a handover into a profile that already had one takes it down — and the rows for those sessions
    /// have to be closed here, or the UI offers to hand over a browser that no longer exists.
    #[serde(default)]
    pub displaced: Vec<String>,
}

/// The wheel coming back, carrying the only thing that makes the trip worth recording.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Returned {
    /// The navigation the headful window recorded (spec §5.3a). Candidates, not permissions: nothing
    /// here is granted until a person says so, and what they are shown is exactly this list.
    #[serde(default)]
    pub chain: Vec<String>,
}

/// What went wrong, in the shapes a caller has to tell apart.
#[derive(Debug)]
pub enum BrowserError {
    /// The sidecar is not running or not answering.
    Unreachable(String),
    /// The fence is not attached, so the sidecar refuses to browse (spec §6.2a). Separate from
    /// [`BrowserError::Failed`] because nothing is broken: the correct response is to report why
    /// browsing is unavailable, never to retry until it works.
    FenceDown(String),
    /// No such session — it was closed, or the sidecar restarted and took every session with it.
    NoSuchSession(String),
    /// The request was malformed. In practice this means a placement the sidecar would not accept,
    /// which is a bug on this side rather than anything the person can act on.
    BadRequest(String),
    /// The driver does not support it.
    Unsupported(String),
    /// Anything else.
    Failed(String),
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BrowserError::Unreachable(why) => write!(f, "browser sidecar unreachable: {why}"),
            BrowserError::FenceDown(why) => write!(f, "browsing is fenced off: {why}"),
            BrowserError::NoSuchSession(why) => write!(f, "no such browsing session: {why}"),
            BrowserError::BadRequest(why) => {
                write!(f, "the browser sidecar refused the request: {why}")
            }
            BrowserError::Unsupported(why) => {
                write!(f, "unsupported by this browser driver: {why}")
            }
            BrowserError::Failed(why) => write!(f, "browser request failed: {why}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct BrowserClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

/// The ceiling on one sidecar call. Above the sidecar's own open timeout (30s by default) so that a
/// navigation which times out over there returns a legible error rather than being cut off here —
/// two timeouts racing produce a failure whose message names the wrong side.
const CALL_TIMEOUT: Duration = Duration::from_secs(90);

impl BrowserClient {
    pub fn new(addr: &str, token: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(CALL_TIMEOUT)
                .build()
                .unwrap_or_default(),
            base: format!("http://{addr}"),
            token,
        }
    }

    pub async fn open(&self, url: &str, placement: &Placement) -> Result<Session, BrowserError> {
        self.call(
            "/open",
            &serde_json::json!({ "url": url, "placement": placement }),
        )
        .await
    }

    /// Read the page. `changes_only` asks for what moved since the previous snapshot of this
    /// session rather than the whole page — the same reading, filtered. `text_from` resumes prose
    /// where a truncated snapshot stopped, which is what keeps truncation from being a dead end.
    /// `find` keeps only the lines that say it, which is the difference between reading a
    /// two-thousand-link directory and reading the one link that was wanted.
    pub async fn snapshot(
        &self,
        session_id: &str,
        changes_only: bool,
        text_from: i64,
        controls_from: i64,
        find: &str,
    ) -> Result<Snapshot, BrowserError> {
        self.call(
            "/snapshot",
            &serde_json::json!({
                "session_id": session_id,
                "changes_only": changes_only,
                "text_from": text_from,
                "controls_from": controls_from,
                "find": find,
            }),
        )
        .await
    }

    pub async fn act(
        &self,
        session_id: &str,
        kind: &str,
        element_ref: &str,
        text: &str,
    ) -> Result<ActResult, BrowserError> {
        self.call(
            "/act",
            &serde_json::json!({
                "session_id": session_id,
                "kind": kind,
                "ref": element_ref,
                "text": text,
            }),
        )
        .await
    }

    /// The annotated picture, for the AGENT to look at.
    ///
    /// JSON and not bytes, unlike [`BrowserClient::screenshot`] below, because the labels travel with
    /// the picture: an image body with the refs in a header would split one answer across two places,
    /// and the half that makes the picture actionable is the half that would be dropped first.
    pub async fn look(&self, session_id: &str) -> Result<LookResult, BrowserError> {
        self.call("/look", &serde_json::json!({ "session_id": session_id }))
            .await
    }

    /// Pixels, for a person to look at. Returns PNG bytes rather than JSON, which is why it does not
    /// go through [`BrowserClient::call`].
    pub async fn screenshot(&self, session_id: &str) -> Result<Vec<u8>, BrowserError> {
        let response = self
            .post(
                "/screenshot",
                &serde_json::json!({ "session_id": session_id }),
            )
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.read_error(status, response).await);
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| BrowserError::Failed(error.to_string()))
    }

    pub async fn handoff(
        &self,
        session_id: &str,
        reason: &str,
    ) -> Result<HandoffTicket, BrowserError> {
        self.call(
            "/handoff",
            &serde_json::json!({ "session_id": session_id, "reason": reason }),
        )
        .await
    }

    /// Hand the wheel to a person: close the agent's browser, open a headful one over the project's
    /// profile (spec §4.2, §4.5).
    ///
    /// The placement is sent rather than inferred from the session, and that is the whole of §4.5:
    /// the profile a handover targets is not the one the agent was in. A run that hit a login wall
    /// was almost certainly in a throwaway, and a throwaway is deleted with the run — so a person
    /// asked to log in there would be logging into something that is about to be erased.
    pub async fn take_wheel(
        &self,
        session_id: &str,
        url: &str,
        placement: &Placement,
    ) -> Result<Wheel, BrowserError> {
        self.call(
            "/wheel/take",
            &serde_json::json!({
                "session_id": session_id,
                "url": url,
                "placement": placement,
            }),
        )
        .await
    }

    /// Take the wheel back: close the person's window and read what it recorded.
    pub async fn return_wheel(&self, session_id: &str) -> Result<Returned, BrowserError> {
        self.call(
            "/wheel/return",
            &serde_json::json!({ "session_id": session_id }),
        )
        .await
    }

    /// Delete a project's profile from disk — spec §10's "Esquecer".
    ///
    /// The counterweight to a list that only grows. Everything else in this client either reads a
    /// page or moves a session; this is the one call that destroys something a person made, which is
    /// why it takes a whole [`Placement`] rather than a project id — the profile name is built in one
    /// place, and this is not a second one.
    pub async fn forget(&self, placement: &Placement) -> Result<Vec<String>, BrowserError> {
        #[derive(Deserialize)]
        struct Stopped {
            #[serde(default)]
            stopped: Vec<String>,
        }
        let answer: Stopped = self
            .call(
                "/forget",
                &serde_json::json!({ "profile": placement.profile }),
            )
            .await?;
        Ok(answer.stopped)
    }

    pub async fn close(&self, session_id: &str) -> Result<(), BrowserError> {
        let response = self
            .post("/close", &serde_json::json!({ "session_id": session_id }))
            .await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        Err(self.read_error(status, response).await)
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, BrowserError> {
        let response = self.post(path, body).await?;
        let status = response.status();
        if !status.is_success() {
            return Err(self.read_error(status, response).await);
        }
        response
            .json::<T>()
            .await
            .map_err(|error| BrowserError::Failed(error.to_string()))
    }

    async fn post(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, BrowserError> {
        self.http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .map_err(|error| BrowserError::Unreachable(error.to_string()))
    }

    async fn read_error(
        &self,
        status: reqwest::StatusCode,
        response: reqwest::Response,
    ) -> BrowserError {
        let body = response.text().await.unwrap_or_default();
        classify(status.as_u16(), body.trim())
    }
}

/// Turn the sidecar's status code into the variant a caller can act on.
///
/// A pure function with a table of cases, for `web_client.rs`'s reason: the mapping that decides
/// whether a caller may retry must not be reachable only through a live socket.
fn classify(status: u16, body: &str) -> BrowserError {
    let body = body.to_string();
    match status {
        400 => BrowserError::BadRequest(body),
        401 => BrowserError::Failed(format!("unauthorized: {body}")),
        404 => BrowserError::NoSuchSession(body),
        501 => BrowserError::Unsupported(body),
        503 => BrowserError::FenceDown(body),
        other => BrowserError::Failed(format!("{other}: {body}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_url_is_loopback_http() {
        let client = BrowserClient::new("127.0.0.1:8795", "t".into());
        assert_eq!(client.base, "http://127.0.0.1:8795");
    }

    /// Spec §6.2a. A fence that is not attached must not look like a crash: a crash invites a retry,
    /// and what would be retried is browsing without a fence.
    #[test]
    fn a_fence_that_is_down_is_not_a_crash() {
        assert!(matches!(
            classify(503, "fence is not attached: refusing to browse"),
            BrowserError::FenceDown(_)
        ));
        assert!(matches!(
            classify(502, "open failed"),
            BrowserError::Failed(_)
        ));
    }

    /// A session the sidecar has never heard of is its own case, because the remedy is to open a new
    /// one — not to report a fault, and not to retry the same id.
    #[test]
    fn a_lost_session_is_distinguishable_from_a_broken_sidecar() {
        assert!(matches!(
            classify(404, "no such session"),
            BrowserError::NoSuchSession(_)
        ));
        assert!(matches!(
            classify(400, "placement: profile: no kind"),
            BrowserError::BadRequest(_)
        ));
    }

    /// An unknown status keeps its number, because the one thing a reader needs from a status nobody
    /// anticipated is the status.
    #[test]
    fn an_unexpected_status_is_named_in_the_message() {
        assert!(classify(418, "teapot").to_string().contains("418"));
    }

    /// The wire shape has to match the sidecar's, and the two are written in different languages —
    /// so the field names are asserted here rather than assumed. A rename on either side is a
    /// placement that arrives empty, which the sidecar answers with 400.
    #[test]
    fn the_placement_serialises_the_way_the_sidecar_reads_it() {
        let placement = Placement::project("42", vec!["https://jira.example.org".into()]);
        let json = serde_json::to_value(&placement).expect("serialisable");
        assert_eq!(json["profile"]["kind"], "project");
        assert_eq!(json["profile"]["id"], "42");
        assert_eq!(json["origins"][0], "https://jira.example.org");
    }

    /// An ephemeral placement carries no list at all — not an empty one, not a null. The sidecar's
    /// fence refuses a list under the ephemeral rule, which is what makes a mix-up loud instead of
    /// quiet, and this is the half of that pairing on this side.
    #[test]
    fn an_ephemeral_placement_has_no_origins() {
        let placement = Placement::ephemeral("run-7");
        assert!(placement.origins.is_empty());
        let json = serde_json::to_value(&placement).expect("serialisable");
        assert!(json.get("origins").is_none(), "{json}");
        assert_eq!(json["profile"]["kind"], "ephemeral");
    }

    /// The sidecar turns an id into a directory name and refuses uppercase, because Windows folds
    /// `Acme` and `acme` into one directory — one set of cookies for two identities. Sending it
    /// lowercased means the ordinary case never reaches that refusal.
    #[test]
    fn an_id_is_reduced_to_what_a_directory_name_may_be() {
        assert_eq!(Placement::project("Acme", vec![]).profile.id, "acme");
        assert_eq!(Placement::ephemeral("../escape").profile.id, "---escape");
        assert_eq!(
            Placement::ephemeral("7f3a1c2e-9b4d-4a6f").profile.id,
            "7f3a1c2e-9b4d-4a6f"
        );
    }

    /// A refusal is a value on a 200, so it must survive a round trip as data rather than as an
    /// error. This is the shape `browser.rs` reads to decide what to tell the agent.
    #[test]
    fn a_refusal_arrives_as_a_value() {
        let result: ActResult = serde_json::from_str(
            r#"{"outcome":"refused","refusal":{"consequence":"form-submission","detail":"POST /orders"}}"#,
        )
        .expect("a refusal is a value");
        assert!(result.refused());
        assert_eq!(
            result.refusal.expect("named").consequence,
            "form-submission"
        );

        let done: ActResult = serde_json::from_str(r#"{"outcome":"done"}"#).expect("done");
        assert!(!done.refused());
        assert!(done.refusal.is_none());
    }

    /// An act that moved the page says so, and says where to.
    ///
    /// The fields are optional on the wire and absent on the ordinary act, so the risk is the
    /// reverse of the usual one: a struct that dropped them would decode every payload happily and
    /// leave the agent holding refs into a document that is gone.
    #[test]
    fn an_act_that_navigated_carries_where_it_went() {
        let result: ActResult = serde_json::from_str(
            r#"{"outcome":"done","navigated":true,"url":"https://example.org/next","still_loading":true}"#,
        )
        .expect("a navigation is a value");
        assert!(result.navigated);
        assert_eq!(result.url, "https://example.org/next");
        assert!(result.still_loading);

        let ordinary: ActResult = serde_json::from_str(r#"{"outcome":"done"}"#).expect("done");
        assert!(
            !ordinary.navigated,
            "a click that changed nothing must not claim otherwise"
        );
        assert!(ordinary.url.is_empty());
    }

    /// A cut snapshot offers a way to read on, and a whole one offers none.
    ///
    /// The presence of the offset IS the offer, so a default that invented a zero on a complete
    /// page would send a polite agent back to read the same page again.
    #[test]
    fn a_cut_snapshot_says_where_to_read_on_from() {
        let cut: Snapshot = serde_json::from_str(
            r#"{"session_id":"s1","url":"https://example.org/","truncated":true,"text_next":20000}"#,
        )
        .expect("a truncated snapshot");
        assert!(cut.truncated);
        assert_eq!(cut.text_next, 20000);

        let whole: Snapshot =
            serde_json::from_str(r#"{"session_id":"s1","url":"https://example.org/"}"#)
                .expect("a whole snapshot");
        assert!(!whole.truncated);
        assert_eq!(whole.text_next, 0);
    }

    /// A page the fence left able to render nothing says so on the reading.
    ///
    /// Absent on the ordinary page, so the field has to survive both ways: a struct that defaulted
    /// it to a zero count would report every page as fine, which is the answer this whole path
    /// exists to stop being given silently.
    #[test]
    fn a_page_the_fence_left_empty_says_so() {
        let shell: Snapshot = serde_json::from_str(
            r#"{"session_id":"s1","url":"https://example.org/","blocked":{"count":3,"consequence":"page-request","detail":"the page tried to reach https://example.org/content on its own"}}"#,
        )
        .expect("a blocked reading");
        let blocked = shell.blocked.expect("carried");
        assert_eq!(blocked.count, 3);
        assert_eq!(blocked.consequence, "page-request");
        assert!(blocked.detail.contains("/content"));

        let ordinary: Snapshot =
            serde_json::from_str(r#"{"session_id":"s1","url":"https://example.org/"}"#)
                .expect("an ordinary reading");
        assert!(
            ordinary.blocked.is_none(),
            "a page nothing was refused on must not look refused"
        );
    }

    /// A sidecar that answers the six routes the way the Go one does, so the client can be driven
    /// over a real socket instead of only being reasoned about.
    ///
    /// It records what ARRIVED, which is the half that matters: the shapes on this wire are written
    /// twice, in two languages, and the failure mode is a field that serialises under a name the far
    /// side does not read. That produces a 400 in production and nothing at all in a test that only
    /// checks the reply.
    async fn stub_sidecar() -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use axum::extract::Path;
        use axum::response::IntoResponse as _;
        use axum::routing::post;

        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let app = axum::Router::new().route(
            "/{verb}",
            post(
                move |Path(verb): Path<String>,
                      headers: axum::http::HeaderMap,
                      axum::Json(body): axum::Json<serde_json::Value>| {
                    let recorder = recorder.clone();
                    async move {
                        let authorization = headers
                            .get("authorization")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_string();
                        recorder.lock().unwrap().push(serde_json::json!({
                            "verb": verb,
                            "authorization": authorization,
                            "body": body,
                        }));
                        match verb.as_str() {
                            "open" => axum::Json(serde_json::json!({
                                "id": "s1", "mode": "agent",
                                "requested_url": "https://jira.example.org/",
                                "final_url": "https://jira.example.org/browse",
                                "title": "Jira",
                            }))
                            .into_response(),
                            "snapshot" => axum::Json(serde_json::json!({
                                "session_id": "s1", "url": "https://jira.example.org/", "title": "Jira",
                                "elements": [{"ref": "e5", "role": "button", "name": "Sign in"}],
                            }))
                            .into_response(),
                            "act" => axum::Json(serde_json::json!({
                                "outcome": "refused",
                                "refusal": {"consequence": "form-submission", "detail": "POST /login"},
                            }))
                            .into_response(),
                            "handoff" => axum::Json(serde_json::json!({
                                "session_id": "s1", "mode": "human",
                                "url": "https://jira.example.org/", "reason": "login",
                            }))
                            .into_response(),
                            "screenshot" => (
                                [(axum::http::header::CONTENT_TYPE, "image/png")],
                                b"\x89PNG".to_vec(),
                            )
                                .into_response(),
                            _ => axum::http::StatusCode::NO_CONTENT.into_response(),
                        }
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (address.to_string(), seen)
    }

    /// Every verb, over a socket. What this pins that a serde test cannot: the token goes on every
    /// call, the placement arrives under the name `/open` reads, and a refusal survives the round
    /// trip as a value rather than becoming an error somewhere in the middle.
    #[tokio::test]
    async fn the_six_verbs_reach_the_sidecar_the_way_it_reads_them() {
        let (address, seen) = stub_sidecar().await;
        let client = BrowserClient::new(&address, "tok".into());

        let session = client
            .open(
                "https://jira.example.org/",
                &Placement::project("acme", vec!["https://jira.example.org".into()]),
            )
            .await
            .expect("open");
        assert_eq!(session.id, "s1");
        assert_eq!(session.final_url, "https://jira.example.org/browse");
        assert!(session.refusal.is_none());

        let snapshot = client
            .snapshot("s1", false, 0, 300, "invoices")
            .await
            .expect("snapshot");
        assert_eq!(snapshot.elements[0].element_ref, "e5");

        let result = client.act("s1", "click", "e5", "").await.expect("act");
        assert!(result.refused(), "a refusal must survive as a value");

        let ticket = client.handoff("s1", "login").await.expect("handoff");
        assert_eq!(ticket.mode, "human");

        let image = client.screenshot("s1").await.expect("screenshot");
        assert_eq!(&image[..4], b"\x89PNG");

        client.close("s1").await.expect("close");

        let calls = seen.lock().unwrap();
        let verbs = calls
            .iter()
            .map(|call| call["verb"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            verbs,
            vec!["open", "snapshot", "act", "handoff", "screenshot", "close"]
        );
        for call in calls.iter() {
            assert_eq!(
                call["authorization"], "Bearer tok",
                "every call carries the daemon token: {call}"
            );
        }
        assert_eq!(calls[0]["body"]["placement"]["profile"]["kind"], "project");
        assert_eq!(calls[0]["body"]["placement"]["profile"]["id"], "acme");
        // Every cursor a snapshot can carry, asserted by name. This is the one thing a round trip
        // through serde cannot catch on its own: a field the far side has no home for marshals
        // perfectly and arrives nowhere, which is how `controls_from` was sent, dropped, and
        // answered with the first page of controls for two days without anything reporting a fault.
        assert_eq!(calls[1]["body"]["controls_from"], 300);
        assert_eq!(calls[1]["body"]["find"], "invoices");
        assert_eq!(calls[2]["body"]["ref"], "e5");
        assert_eq!(calls[5]["body"]["session_id"], "s1");
    }

    /// The other half of the round trip: a sidecar that is not there. A caller has to be able to
    /// tell "nothing is listening" from "the browser said no", because only one of them is a fault.
    #[tokio::test]
    async fn a_sidecar_that_is_not_listening_is_unreachable_and_not_a_refusal() {
        // Port 1 on loopback: nothing legitimate binds it, and connecting fails immediately rather
        // than hanging for the call timeout.
        let client = BrowserClient::new("127.0.0.1:1", "tok".into());
        let error = client
            .open("https://example.org/", &Placement::ephemeral("r1"))
            .await
            .expect_err("nothing is listening");
        assert!(matches!(error, BrowserError::Unreachable(_)), "{error}");
    }

    /// `ref` is a keyword here and a field name there. The rename is the kind of thing that compiles
    /// either way and only fails at runtime, on the one call that matters.
    #[test]
    fn an_element_ref_crosses_the_language_boundary() {
        let snapshot: Snapshot = serde_json::from_str(
            r#"{"session_id":"s1","url":"https://example.org/","title":"t",
                "elements":[{"ref":"e5","role":"button","name":"Sign in"}]}"#,
        )
        .expect("a snapshot");
        assert_eq!(snapshot.elements[0].element_ref, "e5");
    }
}
