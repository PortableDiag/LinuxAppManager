//! A local HTTP API that can do everything the window can: list apps, install,
//! update, reinstall, uninstall, open, add/edit/remove sources, follow users,
//! import/export, self-install, and change the API's own port and key.
//!
//! Same shape as Gapless's control API, deliberately: HTTP + JSON that anything
//! can drive with `curl`, a listener on its own thread, and every request
//! handed to the GTK main loop over a channel so it runs in the same place a
//! button click would. That is what keeps an API install and a clicked install
//! of the same app from overlapping — they share the one in-flight guard.
//!
//! ## Security
//!
//! - Bound to **127.0.0.1** only. Off by default; switched on in the ▾ menu →
//!   Local API.
//! - Every request must carry the key, as `Authorization: Bearer <key>` or
//!   `X-API-Key: <key>`. There is no unauthenticated endpoint.
//! - The key is 32 random bytes, hex, in `~/.config/linux-app-manager/api-key`
//!   at mode 0600. The user can replace it with their own or regenerate it.
//! - Comparison is constant-time.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The port the API listens on unless `settings.json` says otherwise. Next to
/// Gapless's 8421, clear of the usual development ports (3000, 5000, 8000, 8080).
pub const DEFAULT_PORT: u16 = 8422;

/// A hung or malicious client must not hold a connection thread forever.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Room for an imported config document, small enough that nothing can be used
/// to exhaust memory.
const MAX_BODY: usize = 1024 * 1024;

/// One request, already parsed and authenticated, on its way to the GTK thread.
pub struct ApiRequest {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// The JSON body, or `Value::Null` if there wasn't one. Malformed JSON is
    /// rejected before it gets this far.
    pub body: Value,
    pub reply: async_channel::Sender<ApiResponse>,
}

pub struct ApiResponse {
    pub status: u16,
    pub body: Value,
    /// Set for the one route that is not JSON. `/api/docs` serves the reference
    /// as Markdown, because the whole point of it is that a person or an agent
    /// reads it — and a 30 KB document escaped into a JSON string is readable by
    /// neither without a second tool.
    pub raw: Option<(&'static str, String)>,
}

impl ApiResponse {
    pub fn ok(body: Value) -> Self {
        ApiResponse { status: 200, body, raw: None }
    }

    pub fn text(content_type: &'static str, text: String) -> Self {
        ApiResponse {
            status: 200,
            body: Value::Null,
            raw: Some((content_type, text)),
        }
    }

    pub fn error(status: u16, message: &str) -> Self {
        ApiResponse {
            status,
            body: json!({ "ok": false, "error": message }),
            raw: None,
        }
    }
}

impl ApiRequest {
    /// `body["x"]` as a string, falling back to `?x=` in the query. Having both
    /// means `curl -X POST '.../volume?volume=0.5'` works without a JSON body,
    /// which is the difference between a one-liner and a quoting exercise.
    pub fn string(&self, key: &str) -> Option<String> {
        if let Some(s) = self.body.get(key).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        self.query.get(key).cloned()
    }

    pub fn u64(&self, key: &str) -> Option<u64> {
        if let Some(n) = self.body.get(key).and_then(|v| v.as_u64()) {
            return Some(n);
        }
        self.query.get(key).and_then(|s| s.parse().ok())
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        if let Some(b) = self.body.get(key).and_then(|v| v.as_bool()) {
            return Some(b);
        }
        match self.query.get(key).map(|s| s.as_str()) {
            Some("true" | "1" | "yes" | "on") => Some(true),
            Some("false" | "0" | "no" | "off") => Some(false),
            _ => None,
        }
    }
}

/// A running listener. Dropping this stops it — and **waits for it to have
/// stopped**, which is the part that matters.
pub struct Server {
    port: u16,
    running: Arc<AtomicBool>,
    /// The accept loop. Joined on drop so the listening socket is provably
    /// closed before anything tries to bind that port again.
    accept: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        // `accept` is blocking, so clearing the flag is not enough on its own —
        // the thread is parked inside the syscall and will not look at it until
        // something arrives. One connection to ourselves wakes it up so it can
        // see the flag and exit. Failure here is fine: it means nothing is
        // listening any more, which is the state we were trying to reach.
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from(([127, 0, 0, 1], self.port)),
            Duration::from_secs(1),
        );

        // THEN WAIT FOR IT. Returning here without joining leaves the old
        // listening socket open for a moment after `drop` has returned, so a
        // rebind to the same port — which is exactly what the Regenerate-key
        // button does — fails with "Address already in use" and the control API
        // stays **dead** until the app is restarted. That shipped in v0.4.0 and
        // took the operator's API down the first time they pressed the button.
        // SO_REUSEADDR does not help: the old socket is still *live*, not in
        // TIME_WAIT.
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// Starts the listener. Returns an error the UI can show — a port already in use
/// is the common one, and silently failing to start a control API is a great way
/// to waste somebody's afternoon.
pub fn start(port: u16, key: String, tx: async_channel::Sender<ApiRequest>) -> std::io::Result<Server> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr)?;
    let running = Arc::new(AtomicBool::new(true));

    let accept = std::thread::spawn({
        let running = running.clone();
        move || {
            for stream in listener.incoming() {
                if !running.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let key = key.clone();
                let tx = tx.clone();
                // A thread per connection. This is a local control socket with a
                // handful of callers, and it keeps one slow client from blocking
                // the next request.
                std::thread::spawn(move || {
                    let _ = handle(stream, &key, &tx);
                });
            }
        }
    });

    Ok(Server {
        port,
        running,
        accept: Some(accept),
    })
}

fn handle(mut stream: TcpStream, key: &str, tx: &async_channel::Sender<ApiRequest>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let mut reader = BufReader::new(stream.try_clone()?);

    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/").to_string();

    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((name, value)) = h.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    // Authenticate before reading the body: an unauthorised caller does not get
    // to make us allocate for it.
    let presented = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
        .map(|s| s.to_string())
        .or_else(|| headers.get("x-api-key").cloned())
        .unwrap_or_default();

    if !constant_time_eq(presented.as_bytes(), key.as_bytes()) {
        return respond(
            &mut stream,
            &ApiResponse::error(401, "missing or incorrect API key — send it as 'Authorization: Bearer <key>' or 'X-API-Key: <key>'"),
        );
    }

    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return respond(&mut stream, &ApiResponse::error(413, "request body too large"));
    }
    let mut raw = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut raw)?;
    }

    let body = if raw.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice::<Value>(&raw) {
            Ok(v) => v,
            Err(e) => {
                return respond(
                    &mut stream,
                    &ApiResponse::error(400, &format!("body is not valid JSON: {e}")),
                )
            }
        }
    };

    let (path, query) = split_target(&target);

    // Hand it to the GTK main loop and wait for the answer. `bounded(1)` is a
    // one-shot: exactly one reply, and the handler cannot block on sending it.
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    let request = ApiRequest {
        method,
        path,
        query,
        body,
        reply: reply_tx,
    };

    if tx.send_blocking(request).is_err() {
        return respond(&mut stream, &ApiResponse::error(503, "App Manager is shutting down"));
    }

    let response = reply_rx
        .recv_blocking()
        .unwrap_or_else(|_| ApiResponse::error(500, "no reply from App Manager"));

    respond(&mut stream, &response)
}

fn split_target(target: &str) -> (String, HashMap<String, String>) {
    let (path, raw_query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let mut query = HashMap::new();
    for pair in raw_query.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(k), percent_decode(v));
    }
    // Trailing slashes are a classic source of "why does /api/play/ 404" — treat
    // them as the same route.
    let path = path.trim_end_matches('/');
    let path = if path.is_empty() { "/" } else { path };
    (path.to_string(), query)
}

/// Enough of percent-decoding for query strings: `%XX` and `+` for space. Paths
/// with spaces in them are the reason this is here at all.
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn respond(stream: &mut TcpStream, response: &ApiResponse) -> std::io::Result<()> {
    let (content_type, body) = match &response.raw {
        Some((ct, text)) => (*ct, text.as_bytes().to_vec()),
        None => (
            "application/json",
            serde_json::to_vec(&response.body).unwrap_or_else(|_| b"{}".to_vec()),
        ),
    };
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {} {}\r\n\
         Content-Type: {}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\r\n",
        response.status,
        reason,
        content_type,
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()?;
    let _ = stream.shutdown(Shutdown::Both);
    Ok(())
}

/// Compares every byte regardless of where the first difference is. A plain `==`
/// returns as soon as two bytes differ, and the time that takes is measurable
/// over enough requests — which is how a key gets recovered one byte at a time.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---- the reference --------------------------------------------------------

/// The API reference, compiled into the binary. `include_str!` rather than
/// reading `docs/API.md` off disk at run time: an installed AppImage has no repo
/// beside it, and a document that ships *with the build* cannot describe a
/// different build than the one answering you.
pub const REFERENCE: &str = include_str!("../docs/API.md");

/// `?section=` narrows the reference to one `##` heading — the reference is
/// ~20 KB and an agent asking about one endpoint should not have to take all of
/// it. Matching is case-insensitive and by prefix, so `?section=rating` finds
/// "Ratings". Returns `None` if nothing matches, so the caller can say which
/// sections exist rather than serving an empty document.
pub fn reference_section(name: &str) -> Option<String> {
    let want = name.trim().to_ascii_lowercase();
    let mut out = String::new();
    let mut in_section = false;
    // A `##` heading inside a fenced block is code, not a heading.
    let mut in_fence = false;
    for line in REFERENCE.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if !in_fence && line.starts_with("## ") {
            let heading = line[3..].trim().to_ascii_lowercase();
            in_section = heading.starts_with(&want) || heading.contains(&want);
            if in_section {
                out.push_str(line);
                out.push('\n');
                continue;
            }
        }
        if in_section {
            out.push_str(line);
            out.push('\n');
        }
    }
    (!out.trim().is_empty()).then_some(out)
}

/// Every `##` heading, for the "no such section" answer.
pub fn reference_sections() -> Vec<String> {
    let mut in_fence = false;
    let mut out = Vec::new();
    for line in REFERENCE.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if !in_fence && line.starts_with("## ") {
            out.push(line[3..].trim().to_string());
        }
    }
    out
}

// ---- the key ------------------------------------------------------------

fn key_path() -> Option<PathBuf> {
    let dir = crate::config::config_dir();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("api-key"))
}

/// Reads the key, creating one on first use. Returns `None` only if the config
/// directory itself is unusable.
pub fn load_or_create_key() -> Option<String> {
    let path = key_path()?;
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let trimmed = existing.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    let key = generate_key()?;
    write_key(&path, &key)?;
    Some(key)
}

/// Throws the old key away and issues a new one. Anything holding the old key
/// stops working immediately, which is the point.
pub fn regenerate_key() -> Option<String> {
    let path = key_path()?;
    let key = generate_key()?;
    write_key(&path, &key)?;
    Some(key)
}

/// Replace the key with one the user chose. Refuses anything short or with
/// whitespace in it: a short key is guessable, and whitespace does not survive
/// being pasted into a header.
pub fn set_key(key: &str) -> Result<String, String> {
    let key = validate_key(key)?;
    let path = key_path().ok_or("the config directory is not writable")?;
    write_key(&path, key).ok_or("could not write the key file")?;
    Ok(key.to_string())
}

fn validate_key(key: &str) -> Result<&str, String> {
    let key = key.trim();
    if key.len() < 16 {
        return Err("the key must be at least 16 characters".into());
    }
    if !key.chars().all(|c| c.is_ascii_graphic()) {
        return Err("the key may only contain printable ASCII, no spaces".into());
    }
    Ok(key)
}

fn write_key(path: &std::path::Path, key: &str) -> Option<()> {
    std::fs::write(path, format!("{key}\n")).ok()?;
    // 0600 before anyone else gets a chance to read it. `write` creates with the
    // process umask, which on a normal desktop is world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Some(())
}

/// 32 bytes from the kernel CSPRNG, hex-encoded.
fn generate_key() -> Option<String> {
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut buf)
        .ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_still_compares_correctly() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc123", b"abc12"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
        // The case that matters: differing in the *first* byte must be no more
        // conclusive than differing in the last.
        assert!(!constant_time_eq(b"zbc123", b"abc123"));
    }

    #[test]
    fn target_splits_into_path_and_query() {
        let (path, query) = split_target("/api/rating?index=3&stars=5");
        assert_eq!(path, "/api/rating");
        assert_eq!(query.get("index").unwrap(), "3");
        assert_eq!(query.get("stars").unwrap(), "5");

        let (path, query) = split_target("/api/status");
        assert_eq!(path, "/api/status");
        assert!(query.is_empty());
    }

    /// `/api/play/` and `/api/play` must be the same route. Getting this wrong
    /// produces a 404 that looks like the endpoint does not exist.
    #[test]
    fn trailing_slashes_do_not_make_a_different_route() {
        assert_eq!(split_target("/api/play/").0, "/api/play");
        assert_eq!(split_target("/api/play").0, "/api/play");
        assert_eq!(split_target("/").0, "/");
        assert_eq!(split_target("").0, "/");
    }

    #[test]
    fn query_values_are_percent_decoded() {
        let (_, query) = split_target("/api/open?path=%2Fhome%2Fme%2FMy+Music%2Falbum");
        assert_eq!(query.get("path").unwrap(), "/home/me/My Music/album");
    }

    /// A malformed escape must not eat the rest of the string or panic.
    #[test]
    fn broken_percent_escapes_are_left_alone() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("a%2"), "a%2");
    }

    #[test]
    fn request_reads_values_from_body_or_query() {
        let (tx, _rx) = async_channel::bounded(1);
        let mut query = HashMap::new();
        query.insert("stars".to_string(), "4".to_string());
        query.insert("on".to_string(), "yes".to_string());
        let req = ApiRequest {
            method: "POST".into(),
            path: "/api/apps".into(),
            query,
            body: json!({ "index": 7, "mode": "favorites" }),
            reply: tx,
        };
        assert_eq!(req.u64("index"), Some(7));
        assert_eq!(req.string("mode").as_deref(), Some("favorites"));
        // Not in the body — must fall back to the query string.
        assert_eq!(req.u64("stars"), Some(4));
        assert_eq!(req.bool("on"), Some(true));
        assert_eq!(req.u64("nothing"), None);
    }

    /// The body wins when a key appears in both, so a caller that sends JSON is
    /// never surprised by a stale query parameter.
    #[test]
    fn body_takes_precedence_over_query() {
        let (tx, _rx) = async_channel::bounded(1);
        let mut query = HashMap::new();
        query.insert("stars".to_string(), "1".to_string());
        let req = ApiRequest {
            method: "POST".into(),
            path: "/api/apps".into(),
            query,
            body: json!({ "stars": 5 }),
            reply: tx,
        };
        assert_eq!(req.u64("stars"), Some(5));
    }

    /// The reference is compiled in, so an empty or missing one is a build-time
    /// problem and this test is the thing that notices it.
    #[test]
    fn the_reference_ships_with_the_binary() {
        assert!(REFERENCE.len() > 2000, "docs/API.md looks empty");
        assert!(REFERENCE.contains("/api/status"));
        assert!(!reference_sections().is_empty());
    }

    #[test]
    fn a_section_can_be_asked_for_by_name() {
        let sections = reference_sections();
        let first = sections.first().expect("at least one ## heading");
        let body = reference_section(first).expect("its own heading must match");
        assert!(body.starts_with("## "));
        assert!(body.to_lowercase().contains(&first.to_lowercase()));
        // Case and partial names work, so an agent guessing "rating" finds
        // "Ratings".
        assert!(reference_section(&first.to_uppercase()).is_some());
        assert!(reference_section("no such section anywhere").is_none());
    }

    /// A `##` inside a fenced code block is a shell comment or a Markdown
    /// example, not a heading, and listing it would send a caller after a
    /// section that does not exist.
    #[test]
    fn fenced_code_does_not_produce_headings() {
        for s in reference_sections() {
            assert!(!s.starts_with('#'), "{s:?} came out of a code fence");
        }
    }

    #[test]
    fn a_chosen_key_must_be_long_and_pasteable() {
        assert!(validate_key("short").is_err());
        assert!(validate_key("has a space in the middle").is_err());
        assert_eq!(validate_key("  0123456789abcdefXYZ \n").unwrap(), "0123456789abcdefXYZ");
    }

    #[test]
    fn generated_keys_are_long_and_not_repeated() {
        let a = generate_key().expect("/dev/urandom must be readable");
        let b = generate_key().unwrap();
        assert_eq!(a.len(), 64, "32 bytes, hex");
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
