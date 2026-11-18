//! The greetd IPC protocol: JSON messages over the Unix socket in
//! $GREETD_SOCK, each prefixed with its length (u32, native byte order).
//! See greetd-ipc(7). Only the four requests a greeter needs.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

#[derive(Debug, PartialEq)]
pub enum Response {
    Success,
    Error(String),
    /// (kind: "visible" | "secret" | "info" | "error", message)
    AuthMessage(String, String),
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub fn create_session(user: &str) -> String {
    format!(r#"{{"type":"create_session","username":"{}"}}"#, escape(user))
}

pub fn auth_response(response: &str) -> String {
    format!(r#"{{"type":"post_auth_message_response","response":"{}"}}"#, escape(response))
}

pub fn start_session(cmd: &[&str]) -> String {
    let args: Vec<String> = cmd.iter().map(|a| format!("\"{}\"", escape(a))).collect();
    format!(r#"{{"type":"start_session","cmd":[{}],"env":[]}}"#, args.join(","))
}

pub fn cancel_session() -> String {
    r#"{"type":"cancel_session"}"#.to_string()
}

/// A string field from a flat JSON object (enough for greetd's replies).
fn field(json: &str, key: &str) -> Option<String> {
    // The key is the occurrence followed by ':' (the same text can also
    // appear as a value, e.g. "type":"auth_message").
    let pat = format!("\"{key}\"");
    let mut from = 0;
    let rest = loop {
        let at = from + json[from..].find(&pat)? + pat.len();
        if let Some(r) = json[at..].trim_start().strip_prefix(':') {
            break r.trim_start();
        }
        from = at;
    };
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?).unwrap_or('?'));
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

pub fn parse(json: &str) -> Option<Response> {
    match field(json, "type")?.as_str() {
        "success" => Some(Response::Success),
        "error" => Some(Response::Error(field(json, "description").unwrap_or_else(|| "Login failed".into()))),
        "auth_message" => Some(Response::AuthMessage(field(json, "auth_message_type")?, field(json, "auth_message").unwrap_or_default())),
        _ => None,
    }
}

pub struct Greetd {
    sock: UnixStream,
}

impl Greetd {
    /// Connect to the greetd running us (None when there's no greetd:
    /// then the greeter runs in demo mode).
    pub fn connect() -> Option<Self> {
        let path = std::env::var_os("GREETD_SOCK")?;
        UnixStream::connect(path).ok().map(|sock| Self { sock })
    }

    pub fn request(&mut self, json: &str) -> Result<Response, String> {
        let body = json.as_bytes();
        self.sock.write_all(&(body.len() as u32).to_ne_bytes()).map_err(|e| e.to_string())?;
        self.sock.write_all(body).map_err(|e| e.to_string())?;
        let mut len = [0u8; 4];
        self.sock.read_exact(&mut len).map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; u32::from_ne_bytes(len) as usize];
        self.sock.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&buf);
        parse(&text).ok_or_else(|| format!("unexpected reply from greetd: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_json() {
        assert_eq!(create_session("ana"), r#"{"type":"create_session","username":"ana"}"#);
        assert_eq!(auth_response("pa\"ss\\"), r#"{"type":"post_auth_message_response","response":"pa\"ss\\"}"#);
        assert_eq!(start_session(&["pancake-session"]), r#"{"type":"start_session","cmd":["pancake-session"],"env":[]}"#);
    }

    #[test]
    fn replies_parse() {
        assert_eq!(parse(r#"{"type":"success"}"#), Some(Response::Success));
        assert_eq!(
            parse(r#"{"type":"auth_message","auth_message_type":"secret","auth_message":"Password: "}"#),
            Some(Response::AuthMessage("secret".into(), "Password: ".into()))
        );
        assert_eq!(
            parse(r#"{"type": "error", "error_type": "auth_error", "description": "pam: \"bad\" password"}"#),
            Some(Response::Error("pam: \"bad\" password".into()))
        );
        assert_eq!(parse(r#"{"type":"nope"}"#), None);
    }
}
