//! Pairing credentials for other NucleOS machines. The session never reaches the
//! webview: it lives in the operating-system vault next to the canonical origin it
//! was issued for, and every request to a paired machine is made from Rust, only
//! ever to that origin.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue, COOKIE, ORIGIN, SET_COOKIE};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

const VAULT_SERVICE: &str = "nucleos";
const SESSION_COOKIE: &str = "__Host-nucleos_session";
const CSRF_HEADER: &str = "X-Nucleos-Csrf";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Serialize)]
pub struct MachinePair {
    id: String,
    origin: String,
    name: String,
}

#[derive(Serialize)]
struct PairRequest<'a> {
    code: &'a str,
}

#[derive(Deserialize)]
struct PairResponse {
    name: Option<String>,
}

/// What the vault keeps per machine: the canonical origin travels with the session,
/// so an id can only ever reach the origin it was paired with.
#[derive(Serialize, Deserialize)]
struct Stored {
    origin: String,
    session: String,
}

trait Vault: Sync {
    fn put(&self, id: &str, stored: &Stored) -> Result<(), String>;
    fn get(&self, id: &str) -> Result<Stored, String>;
    fn remove(&self, id: &str) -> Result<(), String>;
}

struct OsVault;

impl OsVault {
    fn entry(id: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(VAULT_SERVICE, id).map_err(|error| error.to_string())
    }
}

impl Vault for OsVault {
    fn put(&self, id: &str, stored: &Stored) -> Result<(), String> {
        let value = serde_json::to_string(stored).map_err(|error| error.to_string())?;
        Self::entry(id)?
            .set_password(&value)
            .map_err(|error| format!("could not store the machine session: {error}"))
    }

    fn get(&self, id: &str) -> Result<Stored, String> {
        let value = Self::entry(id)?
            .get_password()
            .map_err(|error| format!("could not read the machine session: {error}"))?;
        serde_json::from_str(&value).map_err(|_| {
            "the stored machine session is unreadable; pair the machine again".to_owned()
        })
    }

    fn remove(&self, id: &str) -> Result<(), String> {
        Self::entry(id)?
            .delete_credential()
            .map_err(|error| format!("could not forget the machine session: {error}"))
    }
}

/// Reduce what the person typed to `https://host[:port]`, or refuse it. The result is
/// what is stored, what the id derives from and what the webview is given back.
fn canonical_origin(origin: &str) -> Result<String, String> {
    const REFUSED: &str =
        "the gateway address must be a plain https:// origin, such as https://tower.example";
    let url = reqwest::Url::parse(origin.trim()).map_err(|_| REFUSED.to_owned())?;
    let host = url.host_str().ok_or_else(|| REFUSED.to_owned())?;
    // Plain http to loopback exists only so the unit tests can talk to a local responder.
    #[cfg(test)]
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && host == "127.0.0.1");
    #[cfg(not(test))]
    let scheme_ok = url.scheme() == "https";
    let bare = url.username().is_empty()
        && url.password().is_none()
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none();
    if !scheme_ok || !bare || host.is_empty() {
        return Err(REFUSED.to_owned());
    }
    Ok(url.origin().ascii_serialization())
}

/// A vault account derived directly from the canonical origin, never from insertion order.
fn machine_id(origin: &str) -> String {
    let mut id = String::from("machine-");
    for byte in origin.bytes() {
        use std::fmt::Write;
        let _ = write!(&mut id, "{byte:02x}");
    }
    id
}

fn session_from_headers(headers: &HeaderMap) -> Option<String> {
    let prefix = format!("{SESSION_COOKIE}=");
    headers.get_all(SET_COOKIE).iter().find_map(|value| {
        value
            .to_str()
            .ok()?
            .split(';')
            .next()?
            .trim()
            .strip_prefix(prefix.as_str())
            .filter(|session| !session.is_empty())
            .map(ToOwned::to_owned)
    })
}

/// One client shape for the whole module: a redirect is never followed, because a
/// followed one could carry the session to an origin it was not issued for.
fn client() -> Result<reqwest::Client, String> {
    // reqwest is built with `rustls-no-provider`; `ring` is already in the build for the updater.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|error| format!("could not start the network client: {error}"))
}

fn redirect_sentence(status: reqwest::StatusCode) -> String {
    format!("the gateway answered with a redirect ({status}), which is never followed")
}

/// Only the statuses that point at another location are refused; 300 and 304 are
/// ordinary answers (304 is the reply to a caller's `If-None-Match`).
fn is_refused_redirect(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
}

async fn pair_with(vault: &dyn Vault, origin: &str, code: &str) -> Result<MachinePair, String> {
    let response = client()?
        .post(format!("{origin}/auth/pair"))
        .header(ORIGIN, origin)
        .header(CSRF_HEADER, "1")
        .json(&PairRequest { code: code.trim() })
        .send()
        .await
        .map_err(|error| format!("could not reach the gateway: {error}"))?;
    let status = response.status();
    if status.is_redirection() {
        return Err(redirect_sentence(status));
    }
    if !status.is_success() {
        return Err(format!("the gateway refused pairing ({status})"));
    }
    let session = session_from_headers(response.headers())
        .ok_or_else(|| "the gateway did not return a session".to_owned())?;
    let pair = response
        .json::<PairResponse>()
        .await
        .map_err(|error| format!("the gateway returned an invalid pairing response: {error}"))?;
    let id = machine_id(origin);
    vault.put(
        &id,
        &Stored {
            origin: origin.to_owned(),
            session,
        },
    )?;
    let name = pair
        .name
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| origin.to_owned());
    Ok(MachinePair {
        id,
        origin: origin.to_owned(),
        name,
    })
}

/// Pair with a gateway, keep its `__Host-` session in the OS vault, and return only
/// the stable local id, canonical origin and human-readable name to the webview.
#[tauri::command]
pub async fn machine_pair(origin: String, code: String) -> Result<MachinePair, String> {
    let origin = canonical_origin(&origin)?;
    pair_with(&OsVault, &origin, &code).await
}

#[tauri::command]
pub fn machine_forget(id: String) -> Result<(), String> {
    OsVault.remove(&id)
}

/// The request URL: the stored origin plus a path that cannot change that origin.
fn machine_url(origin: &str, path: &str) -> Result<reqwest::Url, String> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return Err(
            "the request path must start with a single / and stay on the paired machine".to_owned(),
        );
    }
    let url = reqwest::Url::parse(&format!("{origin}{path}"))
        .map_err(|_| "the request path is not valid".to_owned())?;
    if url.origin().ascii_serialization() != origin {
        return Err("the request path would leave the paired machine".to_owned());
    }
    Ok(url)
}

/// Caller headers minus everything only Rust may set, then Rust's own `Origin`, CSRF
/// marker and `Cookie`.
fn machine_headers(
    origin: &str,
    session: &str,
    caller: &HashMap<String, String>,
) -> Result<HeaderMap, String> {
    let mut headers = HeaderMap::new();
    for (name, value) in caller {
        let lower = name.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "origin" | "cookie" | "host" | "x-nucleos-csrf"
        ) || lower.starts_with("x-nucleos-device-")
        {
            continue;
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| format!("`{name}` is not a valid header name"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| format!("the `{name}` header value is not valid"))?;
        headers.append(name, value);
    }
    headers.insert(
        ORIGIN,
        HeaderValue::from_str(origin).map_err(|_| "the paired origin is not valid".to_owned())?,
    );
    headers.insert(CSRF_HEADER, HeaderValue::from_static("1"));
    let mut cookie =
        HeaderValue::from_str(&format!("{SESSION_COOKIE}={session}")).map_err(|_| {
            "the stored machine session is unreadable; pair the machine again".to_owned()
        })?;
    cookie.set_sensitive(true);
    headers.insert(COOKIE, cookie);
    Ok(headers)
}

#[derive(Serialize)]
pub struct FetchHead {
    status: u16,
    headers: Vec<(String, String)>,
}

/// What the response body channel carries. `End` or `Error` always closes it.
#[derive(Serialize)]
#[serde(tag = "event", content = "data", rename_all = "camelCase")]
pub enum FetchEvent {
    Chunk(Vec<u8>),
    End,
    Error(String),
}

static CANCELS: LazyLock<Mutex<HashMap<String, Arc<Notify>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn cancels() -> std::sync::MutexGuard<'static, HashMap<String, Arc<Notify>>> {
    CANCELS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Send the request and return the response with its head already read.
async fn start_fetch(
    id: &str,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: Option<String>,
    cancel: &Notify,
) -> Result<(FetchHead, reqwest::Response), String> {
    let stored = OsVault.get(id)?;
    let method = match method.to_ascii_uppercase().as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        "PATCH" => reqwest::Method::PATCH,
        "DELETE" => reqwest::Method::DELETE,
        "HEAD" => reqwest::Method::HEAD,
        _ => return Err("this request method is not allowed on a paired machine".to_owned()),
    };
    let url = machine_url(&stored.origin, path)?;
    let mut request = client()?.request(method, url).headers(machine_headers(
        &stored.origin,
        &stored.session,
        headers,
    )?);
    if let Some(body) = body {
        request = request.body(body);
    }
    let response = tokio::select! {
        () = cancel.notified() => return Err("the request was cancelled".to_owned()),
        sent = request.send() => sent.map_err(|error| format!("could not reach the machine: {error}"))?,
    };
    let status = response.status();
    if is_refused_redirect(status) {
        return Err(redirect_sentence(status));
    }
    let headers = response
        .headers()
        .iter()
        .filter(|(name, _)| !matches!(name.as_str(), "set-cookie" | "set-cookie2"))
        .map(|(name, value)| {
            (
                name.to_string(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    Ok((
        FetchHead {
            status: status.as_u16(),
            headers,
        },
        response,
    ))
}

/// Request a path on a paired machine. Returns the status and headers once they
/// arrive; the body follows on `on_chunk` until an `end` or `error` event.
#[tauri::command]
pub async fn machine_fetch(
    id: String,
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Option<String>,
    on_chunk: tauri::ipc::Channel<FetchEvent>,
    request_id: String,
) -> Result<FetchHead, String> {
    let cancel = Arc::new(Notify::new());
    cancels().insert(request_id.clone(), cancel.clone());
    let started = start_fetch(&id, &method, &path, &headers, body, &cancel).await;
    let (head, mut response) = match started {
        Ok(started) => started,
        Err(error) => {
            cancels().remove(&request_id);
            return Err(error);
        }
    };
    tauri::async_runtime::spawn(async move {
        let closing = loop {
            tokio::select! {
                () = cancel.notified() => break FetchEvent::Error("the request was cancelled".to_owned()),
                chunk = response.chunk() => match chunk {
                    Ok(Some(bytes)) => {
                        if on_chunk.send(FetchEvent::Chunk(bytes.to_vec())).is_err() {
                            break FetchEvent::End;
                        }
                    }
                    Ok(None) => break FetchEvent::End,
                    Err(error) => break FetchEvent::Error(format!("the machine stopped answering: {error}")),
                },
            }
        };
        cancels().remove(&request_id);
        let _ = on_chunk.send(closing);
    });
    Ok(head)
}

/// Stop a request started by `machine_fetch`. An unknown id is not an error: the request may
/// simply have finished first.
#[tauri::command]
pub fn machine_fetch_cancel(request_id: String) {
    if let Some(cancel) = cancels().get(&request_id) {
        cancel.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn a_not_modified_answer_is_not_a_redirect() {
        assert!(!is_refused_redirect(reqwest::StatusCode::NOT_MODIFIED));
        assert!(!is_refused_redirect(reqwest::StatusCode::MULTIPLE_CHOICES));
        for code in [301u16, 302, 303, 307, 308] {
            assert!(
                is_refused_redirect(reqwest::StatusCode::from_u16(code).unwrap()),
                "{code}"
            );
        }
    }

    #[derive(Default)]
    struct MemVault(Mutex<HashMap<String, String>>);

    impl Vault for MemVault {
        fn put(&self, id: &str, stored: &Stored) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .insert(id.to_owned(), serde_json::to_string(stored).unwrap());
            Ok(())
        }
        fn get(&self, id: &str) -> Result<Stored, String> {
            let map = self.0.lock().unwrap();
            serde_json::from_str(map.get(id).ok_or("missing")?).map_err(|error| error.to_string())
        }
        fn remove(&self, id: &str) -> Result<(), String> {
            self.0.lock().unwrap().remove(id);
            Ok(())
        }
    }

    /// Answer exactly one connection with `response`, then close. Returns the loopback origin.
    fn serve_once(response: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    fn set_cookie(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(SET_COOKIE, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn the_session_cookie_is_read_from_set_cookie() {
        let headers = set_cookie(&[
            "other=1; Path=/",
            "__Host-nucleos_session=abc123; Path=/; Secure; HttpOnly",
        ]);
        assert_eq!(session_from_headers(&headers).as_deref(), Some("abc123"));
    }

    #[test]
    fn an_empty_session_cookie_is_a_failed_pairing() {
        assert_eq!(
            session_from_headers(&set_cookie(&["__Host-nucleos_session=; Max-Age=0"])),
            None
        );
        let origin = serve_once("HTTP/1.1 200 OK\r\nSet-Cookie: __Host-nucleos_session=; Path=/\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
        let vault = MemVault::default();
        let result = tauri::async_runtime::block_on(pair_with(&vault, &origin, "ABCDE"));
        assert!(result.err().unwrap().contains("did not return a session"));
        assert!(vault.0.lock().unwrap().is_empty());
    }

    #[test]
    fn an_origin_is_canonicalized_before_it_is_stored() {
        assert_eq!(
            canonical_origin("  HTTPS://Tower.Example/ ").unwrap(),
            "https://tower.example"
        );
        assert_eq!(
            canonical_origin("https://tower.example:443").unwrap(),
            "https://tower.example"
        );
        assert_eq!(
            canonical_origin("https://Tower.Example:8443/").unwrap(),
            "https://tower.example:8443"
        );
    }

    #[test]
    fn an_origin_with_a_path_or_plain_http_is_refused() {
        for refused in [
            "http://tower.example",
            "https://tower.example/app",
            "https://tower.example/?x=1",
            "https://tower.example/#top",
            "https://user:pw@tower.example",
            "ftp://tower.example",
            "tower.example",
            "",
        ] {
            assert!(
                canonical_origin(refused).is_err(),
                "{refused:?} should be refused"
            );
        }
    }

    #[test]
    fn a_path_cannot_leave_the_paired_origin() {
        let origin = "https://tower.example";
        assert_eq!(
            machine_url(origin, "/api/projects?x=1").unwrap().as_str(),
            "https://tower.example/api/projects?x=1"
        );
        for refused in [
            "",
            "api",
            "//evil.example/x",
            "/\\evil.example",
            "/a\\b",
            "@evil.example",
            ":443/x",
            "/a\nb",
        ] {
            assert!(
                machine_url(origin, refused).is_err(),
                "{refused:?} should be refused"
            );
        }
    }

    #[test]
    fn caller_headers_cannot_set_origin_or_cookie() {
        let caller: HashMap<String, String> = [
            ("Origin", "https://evil.example"),
            ("cookie", "stolen=1"),
            ("HOST", "evil.example"),
            ("x-nucleos-csrf", "0"),
            ("X-Nucleos-Device-Id", "forged"),
            ("Accept", "text/event-stream"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect();
        let headers = machine_headers("https://tower.example", "s3cret", &caller).unwrap();
        assert_eq!(headers.get(ORIGIN).unwrap(), "https://tower.example");
        assert_eq!(
            headers.get(COOKIE).unwrap(),
            "__Host-nucleos_session=s3cret"
        );
        assert_eq!(headers.get(CSRF_HEADER).unwrap(), "1");
        assert_eq!(headers.get("accept").unwrap(), "text/event-stream");
        assert_eq!(headers.get_all(ORIGIN).iter().count(), 1);
        assert!(headers.get("host").is_none());
        assert!(headers.get("x-nucleos-device-id").is_none());
    }

    #[test]
    fn a_redirect_is_not_followed_during_pairing() {
        let origin = serve_once(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/elsewhere\r\nSet-Cookie: __Host-nucleos_session=abc; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let vault = MemVault::default();
        let error = tauri::async_runtime::block_on(pair_with(&vault, &origin, "ABCDE"))
            .err()
            .unwrap();
        assert!(error.contains("redirect"), "{error}");
        assert!(vault.0.lock().unwrap().is_empty());
    }
}
