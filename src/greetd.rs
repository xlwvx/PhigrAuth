//! Minimal greetd IPC client.
//!
//! greetd (the privileged session manager) performs PAM authentication and
//! starts the session.  This is required when the greeter itself runs as an
//! unprivileged user such as `greeter`: local pam_unix cannot read
//! /etc/shadow, and the greeter cannot spawn a session as another user.
//!
//! Protocol framing: a native-endian u32 payload length followed by a JSON
//! object.  See greetd-ipc(7).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

const MAX_MESSAGE: usize = 1024 * 1024;

#[derive(Debug)]
pub struct Session {
    stream: UnixStream,
    started: bool,
}

#[derive(Debug)]
pub enum AuthError {
    /// Wrong credentials / PAM auth_error; show the normal retry state.
    Failure(String),
    /// Configuration or greeting daemon problem; show a toast.
    Error(String),
}

pub fn socket_path() -> Option<String> {
    std::env::var("GREETD_SOCK")
        .ok()
        .filter(|path| !path.is_empty())
}

pub fn available() -> bool {
    socket_path().is_some()
}

fn send_message(stream: &mut UnixStream, message: &Value) -> Result<(), String> {
    let payload = serde_json::to_vec(message).map_err(|e| format!("greetd JSON 编码失败: {e}"))?;
    let len = u32::try_from(payload.len()).map_err(|_| "greetd 消息过长".to_string())?;
    stream
        .write_all(&len.to_ne_bytes())
        .and_then(|_| stream.write_all(&payload))
        .and_then(|_| stream.flush())
        .map_err(|e| format!("greetd 发送失败: {e}"))
}

fn read_message(stream: &mut UnixStream) -> Result<Value, String> {
    let mut len = [0u8; 4];
    stream
        .read_exact(&mut len)
        .map_err(|e| format!("greetd 读取失败: {e}"))?;
    let len = u32::from_ne_bytes(len) as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err(format!("greetd 返回了非法消息长度 {len}"));
    }
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("greetd 读取失败: {e}"))?;
    serde_json::from_slice(&buf).map_err(|e| format!("greetd JSON 解析失败: {e}"))
}

fn response_type(value: &Value) -> &str {
    value.get("type").and_then(Value::as_str).unwrap_or("")
}

fn error_from_response(value: &Value) -> AuthError {
    let kind = value
        .get("error_type")
        .and_then(Value::as_str)
        .unwrap_or("error");
    let description = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("greetd 返回错误")
        .to_string();
    if kind == "auth_error" {
        AuthError::Failure(if description.is_empty() {
            "凭据无效".into()
        } else {
            description
        })
    } else {
        AuthError::Error(description)
    }
}

fn authenticate_at(path: &Path, username: &str, password: &str) -> Result<Session, AuthError> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| AuthError::Error(format!("无法连接 greetd socket {}: {e}", path.display())))?;
    stream.set_read_timeout(Some(Duration::from_secs(15))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    send_message(
        &mut stream,
        &json!({"type": "create_session", "username": username}),
    )
    .map_err(AuthError::Error)?;

    // PAM may ask any number of questions, but this compact UI only has one
    // password field.  Answer the first secret prompt with the password and
    // acknowledge everything else without inventing more credentials.
    let mut password_sent = false;
    for _ in 0..128 {
        let response = read_message(&mut stream).map_err(AuthError::Error)?;
        match response_type(&response) {
            "success" => {
                return Ok(Session {
                    stream,
                    started: false,
                });
            }
            "error" => return Err(error_from_response(&response)),
            "auth_message" => {
                let kind = response
                    .get("auth_message_type")
                    .and_then(Value::as_str)
                    .unwrap_or("info");
                let reply = if kind == "secret" && !password_sent {
                    password_sent = true;
                    json!({"type": "post_auth_message_response", "response": password})
                } else {
                    json!({"type": "post_auth_message_response", "response": ""})
                };
                send_message(&mut stream, &reply).map_err(AuthError::Error)?;
            }
            other => {
                return Err(AuthError::Error(format!(
                    "greetd 返回未知响应类型 {other:?}"
                )));
            }
        }
    }
    Err(AuthError::Error("greetd 认证消息过多".into()))
}

pub fn authenticate(username: &str, password: &str) -> Result<Session, AuthError> {
    let path = socket_path()
        .ok_or_else(|| AuthError::Error("未检测到 GREETD_SOCK；无法使用 greetd 认证".into()))?;
    authenticate_at(Path::new(&path), username, password)
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.started {
            return;
        }
        // Never block shutdown for long; this is best-effort cleanup of a
        // prepared (but unstarted) greetd session.
        self.stream
            .set_write_timeout(Some(Duration::from_millis(250)))
            .ok();
        let _ = send_message(&mut self.stream, &json!({"type": "cancel_session"}));
    }
}

impl Session {
    /// Ask greetd to start the configured session.  greetd starts it after the
    /// greeter process exits, so callers should quit immediately on success.
    pub fn start(&mut self, cmd: Vec<String>, env: Vec<String>) -> Result<(), String> {
        self.stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .ok();
        self.stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .ok();
        send_message(
            &mut self.stream,
            &json!({"type": "start_session", "cmd": cmd, "env": env}),
        )?;
        // Once the request is on the wire we must not send cancel_session on
        // drop; greetd may already be starting the session.
        self.started = true;
        let response = read_message(&mut self.stream)?;
        match response_type(&response) {
            "success" => Ok(()),
            "error" => {
                let description = response
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("greetd 无法启动会话");
                Err(description.to_string())
            }
            other => Err(format!("greetd 返回未知响应类型 {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn socket_path() -> std::path::PathBuf {
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "phigrauth-greetd-test-{}-{id}.sock",
            std::process::id()
        ))
    }

    fn fake_server(
        path: std::path::PathBuf,
        expected_user: &'static str,
        expected_password: &'static str,
        expected_cmd: Vec<String>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let listener = UnixListener::bind(&path).unwrap();
            let (mut stream, _) = listener.accept().unwrap();

            let create = read_message(&mut stream).unwrap();
            assert_eq!(response_type(&create), "create_session");
            assert_eq!(create["username"], expected_user);

            send_message(
                &mut stream,
                &json!({
                    "type": "auth_message",
                    "auth_message_type": "secret",
                    "auth_message": "Password:"
                }),
            )
            .unwrap();
            let answer = read_message(&mut stream).unwrap();
            assert_eq!(response_type(&answer), "post_auth_message_response");
            assert_eq!(answer["response"], expected_password);

            send_message(&mut stream, &json!({"type": "success"})).unwrap();

            let start = read_message(&mut stream).unwrap();
            assert_eq!(response_type(&start), "start_session");
            assert_eq!(start["cmd"], serde_json::to_value(&expected_cmd).unwrap());
            send_message(&mut stream, &json!({"type": "success"})).unwrap();
            let _ = std::fs::remove_file(&path);
        })
    }

    #[test]
    fn creates_authenticates_and_starts_session() {
        let path = socket_path();
        let server = fake_server(
            path.clone(),
            "alice",
            "correct horse",
            vec!["/bin/sh".into(), "-c".into(), "exec niri-session".into()],
        );
        // The listener needs a moment to bind before the client connects.
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut session = authenticate_at(&path, "alice", "correct horse").unwrap();
        session
            .start(
                vec!["/bin/sh".into(), "-c".into(), "exec niri-session".into()],
                vec!["XDG_SESSION_TYPE=wayland".into()],
            )
            .unwrap();
        server.join().unwrap();
    }

    #[test]
    fn auth_error_is_reported_as_failure() {
        let path = socket_path();
        let server_path = path.clone();
        let server = std::thread::spawn(move || {
            let listener = UnixListener::bind(&server_path).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_message(&mut stream).unwrap();
            send_message(
                &mut stream,
                &json!({"type": "error", "error_type": "auth_error", "description": "bad password"}),
            )
            .unwrap();
            let _ = std::fs::remove_file(&server_path);
        });
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        match authenticate_at(&path, "alice", "wrong") {
            Err(AuthError::Failure(msg)) => assert_eq!(msg, "bad password"),
            other => panic!("expected auth failure, got {other:?}"),
        }
        server.join().unwrap();
    }
    #[test]
    fn second_secret_prompt_does_not_repeat_password() {
        let path = socket_path();
        let server_path = path.clone();
        let server = std::thread::spawn(move || {
            let listener = UnixListener::bind(&server_path).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_message(&mut stream).unwrap();
            send_message(
                &mut stream,
                &json!({
                    "type": "auth_message",
                    "auth_message_type": "secret",
                    "auth_message": "Password:"
                }),
            )
            .unwrap();
            let first = read_message(&mut stream).unwrap();
            assert_eq!(first["response"], "pw");
            send_message(
                &mut stream,
                &json!({
                    "type": "auth_message",
                    "auth_message_type": "secret",
                    "auth_message": "OTP:"
                }),
            )
            .unwrap();
            let second = read_message(&mut stream).unwrap();
            assert_eq!(second["response"], "");
            send_message(&mut stream, &json!({"type": "success"})).unwrap();
            let _ = std::fs::remove_file(&server_path);
        });
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let session = authenticate_at(&path, "alice", "pw").unwrap();
        drop(session);
        server.join().unwrap();
    }

    #[test]
    fn dropping_prepared_session_sends_cancel() {
        let path = socket_path();
        let server_path = path.clone();
        let server = std::thread::spawn(move || {
            let listener = UnixListener::bind(&server_path).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let _ = read_message(&mut stream).unwrap();
            send_message(&mut stream, &json!({"type": "success"})).unwrap();
            let cancel = read_message(&mut stream).unwrap();
            assert_eq!(response_type(&cancel), "cancel_session");
            let _ = std::fs::remove_file(&server_path);
        });
        for _ in 0..100 {
            if path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let session = authenticate_at(&path, "alice", "pw").unwrap();
        drop(session);
        server.join().unwrap();
    }
}
