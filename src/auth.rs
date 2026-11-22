use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use crate::config::AuthBackend;
use crate::greetd;

static DEV_MOCK: AtomicBool = AtomicBool::new(false);

/// Enable the development authentication shortcut.  This requires an explicit
/// command-line opt-in (--dev-mock) so an inherited environment variable can
/// never silently disable PAM.
pub fn enable_dev_mock() {
    DEV_MOCK.store(true, Ordering::Relaxed);
}

pub fn dev_mock_enabled() -> bool {
    DEV_MOCK.load(Ordering::Relaxed)
}

#[derive(Debug)]
pub enum AuthOutcome {
    /// Local PAM succeeded; the process must start the session itself.
    Success,
    /// greetd authenticated and will start the session on request.
    GreetdSuccess(greetd::Session),
    Failure(String),
    Error(String),
}

/// Authenticate `user` against a PAM service.
///
/// Development shortcut: when the `--dev-mock` flag was passed, the value of
/// `PHIGRAUTH_DEV_PASSWORD` is accepted instead of talking to PAM.  This is only
/// meant for UI development and automated screenshots; never use it on a real
/// greeter.
fn authenticate_pam(service: &str, user: &str, password: &str) -> AuthOutcome {
    if dev_mock_enabled() {
        let expected = std::env::var("PHIGRAUTH_DEV_PASSWORD").unwrap_or_default();
        return if password == expected {
            AuthOutcome::Success
        } else {
            AuthOutcome::Failure("密码错误".into())
        };
    }

    let mut client = match pam::Client::with_password(service) {
        Ok(c) => c,
        Err(e) => {
            return AuthOutcome::Error(format!("无法启动 PAM 服务 {service}: {e}"));
        }
    };
    client.conversation_mut().set_credentials(user, password);

    match client.authenticate() {
        Ok(()) => AuthOutcome::Success,
        Err(e) => {
            use pam::PamReturnCode::*;
            match e.0 {
                Auth_Err | Cred_Insufficient | Authinfo_Unavail | User_Unknown | MaxTries
                | Acct_Expired | Cred_Expired | Perm_Denied => AuthOutcome::Failure(describe(e.0)),
                _ => AuthOutcome::Error(format!("认证服务错误: {}", describe(e.0))),
            }
        }
    }
}

fn describe(code: pam::PamReturnCode) -> String {
    use pam::PamReturnCode::*;
    match code {
        Auth_Err => "密码错误".into(),
        User_Unknown => "用户不存在".into(),
        MaxTries => "尝试次数过多".into(),
        Acct_Expired => "账户已过期".into(),
        Cred_Expired => "凭据已过期".into(),
        Cred_Insufficient => "凭据不足".into(),
        Authinfo_Unavail => "无法获取认证信息".into(),
        Perm_Denied => "权限不足".into(),
        System_Err => "PAM 系统错误".into(),
        _ => format!("认证失败 ({code:?})"),
    }
}

fn authenticate_greetd(user: &str, password: &str) -> AuthOutcome {
    match greetd::authenticate(user, password) {
        Ok(session) => AuthOutcome::GreetdSuccess(session),
        Err(greetd::AuthError::Failure(message)) => AuthOutcome::Failure(message),
        Err(greetd::AuthError::Error(message)) => AuthOutcome::Error(message),
    }
}

/// Spawn authentication on a worker thread.  `Auto` prefers greetd when the
/// greeter was started by greetd (`GREETD_SOCK` is present).
pub fn spawn(
    backend: AuthBackend,
    service: String,
    user: String,
    password: String,
    tx: Sender<AuthOutcome>,
) {
    std::thread::spawn(move || {
        let outcome = if dev_mock_enabled() {
            let expected = std::env::var("PHIGRAUTH_DEV_PASSWORD").unwrap_or_default();
            if password == expected {
                AuthOutcome::Success
            } else {
                AuthOutcome::Failure("密码错误".into())
            }
        } else {
            let backend = match backend {
                AuthBackend::Auto if greetd::available() => AuthBackend::Greetd,
                AuthBackend::Auto => AuthBackend::Pam,
                other => other,
            };
            match backend {
                AuthBackend::Greetd => {
                    if let Some(path) = greetd::socket_path() {
                        eprintln!("phigrauth: authenticating through greetd ({path})");
                    }
                    authenticate_greetd(&user, &password)
                }
                AuthBackend::Pam | AuthBackend::Auto => {
                    authenticate_pam(&service, &user, &password)
                }
            }
        };
        let _ = tx.send(outcome);
    });
}
