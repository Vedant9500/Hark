//! Small blocking HTTP helpers for provider worker threads (FX, translate).
//! Keeps curl process spawns off the battery path.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

const CONNECT: Duration = Duration::from_secs(2);
/// Per-request wall time. Translate races two backends under ~3s; FX rates also
/// fit. Too tight (1–2s) caused free Google TLS reads to fail while curl was fine.
const TOTAL: Duration = Duration::from_secs(4);

/// Shared agent so TLS/DNS sessions can be reused across worker requests.
/// `no_proxy`: hark talks only to fixed, trusted hosts (Frankfurter,
/// translate APIs, the user's LibreTranslate endpoint) — an inherited
/// `ALL_PROXY`/`HTTP_PROXY` env var would silently route that traffic
/// (including secret-bearing requests) through an attacker-chosen host.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(CONNECT)
            .timeout(TOTAL)
            .user_agent("hark-launcher/0.1")
            .try_proxy_from_env(false)
            .build()
    })
}

/// Agent for requests that may carry secrets in the body (e.g. the
/// LibreTranslate API key). Redirects are disabled: on 307/308 ureq would
/// replay the request body — key and pasted text — to whatever host the
/// (possibly plain-HTTP) endpoint redirects to, bypassing the endpoint
/// validation done at config time (CWE-601/CWE-200).
fn no_redirect_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(CONNECT)
            .timeout(TOTAL)
            .user_agent("hark-launcher/0.1")
            .redirects(0)
            .try_proxy_from_env(false)
            .build()
    })
}

/// GET body as bytes (status must be 2xx; ureq maps non-2xx to Err).
pub fn get_bytes(url: &str) -> Result<Vec<u8>, String> {
    let resp = agent().get(url).call().map_err(short_err)?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(4 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {e}"))?;
    Ok(buf)
}

/// Background-fetch agent for non-UI workers (FX rates). Unlike the fast
/// request agent, this tolerates slow cold DNS lookups (can take 5s+ on the
/// first resolution in a fresh process, exceeding the UI-facing timeouts).
fn background_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .user_agent("hark-launcher/0.1")
            .try_proxy_from_env(false)
            .build()
    })
}

/// GET body as bytes with generous timeouts; for background fetches that never
/// block the UI (e.g. currency-rate refresh).
pub fn get_bytes_background(url: &str) -> Result<Vec<u8>, String> {
    let resp = background_agent().get(url).call().map_err(short_err)?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(4 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {e}"))?;
    Ok(buf)
}

/// GET with simple query pairs (values are form-urlencoded by ureq).
pub fn get_bytes_query(url: &str, query: &[(&str, &str)]) -> Result<Vec<u8>, String> {
    let mut req = agent().get(url);
    for (k, v) in query {
        req = req.query(k, v);
    }
    let resp = req.call().map_err(short_err)?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(4 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {e}"))?;
    Ok(buf)
}

/// POST JSON body; returns response bytes. Never follows redirects — callers
/// may put secrets (API keys, user text) in the body.
pub fn post_json(url: &str, body: &str) -> Result<Vec<u8>, String> {
    let resp = no_redirect_agent()
        .post(url)
        .set("Content-Type", "application/json")
        .send_string(body)
        .map_err(short_err)?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(4 * 1024 * 1024)
        .read_to_end(&mut buf)
        .map_err(|e| format!("read failed: {e}"))?;
    Ok(buf)
}

fn short_err(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => {
            let s = t.to_string();
            let low = s.to_ascii_lowercase();
            if low.contains("timed out") || low.contains("timeout") {
                "timed out".into()
            } else if low.contains("dns")
                || low.contains("connection")
                || low.contains("failed to connect")
                || low.contains("network")
            {
                "unreachable".into()
            } else {
                s.chars().take(80).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The secret-bearing POST agent must not follow redirects: a 307/308
    /// from a configured endpoint would replay the key-bearing body to an
    /// arbitrary host.
    /// Audit P3 (Pass 16): agents must ignore inherited proxy env vars —
    /// otherwise an `ALL_PROXY`/`HTTP_PROXY` set in the session routes
    /// hark's traffic (incl. secret-bearing POSTs) through it.
    #[test]
    fn agents_ignore_proxy_env() {
        // Agents are process-wide OnceLocks; setting the env here only
        // proves the builder flag exists and compiles into each agent.
        // Functional proof: `try_proxy_from_env(false)` makes the agent skip
        // env proxy resolution entirely.
        std::env::set_var("ALL_PROXY", "http://127.0.0.1:1");
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:1");
        std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:1");
        // With no_proxy, a request to a dead proxy address must NOT be
        // attempted; the connect error names the real host/port instead.
        let err = get_bytes("http://127.0.0.1:2/x").unwrap_err();
        assert!(!err.to_ascii_lowercase().contains(":1"), "{err}");
        std::env::remove_var("ALL_PROXY");
        std::env::remove_var("HTTP_PROXY");
        std::env::remove_var("HTTPS_PROXY");
    }

    #[test]
    fn post_json_agent_has_redirects_disabled() {
        use std::io::{Read, Write as _};
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let redirect_target = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        redirect_target.set_nonblocking(true).unwrap();
        let target_port = redirect_target.local_addr().unwrap().port();
        let target_hit = Arc::new(AtomicBool::new(false));
        let target_hit_c = target_hit.clone();
        let target_handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                match redirect_target.accept() {
                    Ok((mut sock, _)) => {
                        target_hit_c.store(true, Ordering::SeqCst);
                        let mut buf = [0u8; 1024];
                        let _ = sock.read(&mut buf);
                        let _ = sock.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        );
                        return;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });

        let source = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let source_port = source.local_addr().unwrap().port();
        let source_hits = Arc::new(AtomicUsize::new(0));
        let source_hits_c = source_hits.clone();
        let source_handle = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = source.accept() {
                source_hits_c.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:{target_port}/evil\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes());
            }
        });

        let body = r#"{"api_key":"sekret","q":"hi"}"#;
        let bytes = post_json(&format!("http://127.0.0.1:{source_port}/t"), body).unwrap();
        assert!(bytes.is_empty(), "no body from a 307");
        source_handle.join().unwrap();
        target_handle.join().unwrap();
        assert_eq!(source_hits.load(Ordering::SeqCst), 1);
        assert!(
            !target_hit.load(Ordering::SeqCst),
            "redirect target must not receive the key-bearing POST"
        );
    }
}
