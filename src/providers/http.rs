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
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout_connect(CONNECT)
            .timeout(TOTAL)
            .user_agent("hark-launcher/0.1")
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

/// POST JSON body; returns response bytes.
pub fn post_json(url: &str, body: &str) -> Result<Vec<u8>, String> {
    let resp = agent()
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
