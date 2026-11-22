use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthBackend {
    Auto,
    Pam,
    Greetd,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub auth_backend: AuthBackend,
    pub pam_service: String,
    pub default_user: Option<String>,
    pub default_session: Option<String>,
    pub sessions_dirs: Vec<PathBuf>,
    pub poweroff: Vec<String>,
    pub reboot: Vec<String>,
    pub launch_session: bool,
    pub tty_shell_command: Option<Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            auth_backend: AuthBackend::Auto,
            pam_service: "login".into(),
            default_user: None,
            default_session: None,
            sessions_dirs: default_sessions_dirs(),
            poweroff: vec!["systemctl".into(), "poweroff".into()],
            reboot: vec!["systemctl".into(), "reboot".into()],
            launch_session: true,
            tty_shell_command: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    auth_backend: Option<String>,
    pam_service: Option<String>,
    default_user: Option<String>,
    default_session: Option<String>,
    sessions_dirs: Option<Vec<String>>,
    poweroff_command: Option<Vec<String>>,
    reboot_command: Option<Vec<String>>,
    launch_session: Option<bool>,
    tty_shell_command: Option<Vec<String>>,
}

impl Config {
    /// Load the single system configuration file.  Missing file means the
    /// built-in defaults; a present but unreadable/invalid file is fatal.
    pub fn load() -> Result<Config, String> {
        Self::load_from(Path::new("/etc/phigrauth/config.toml"))
    }

    fn load_from(path: &Path) -> Result<Config, String> {
        let mut cfg = Config::default();
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(cfg),
            Err(e) => {
                return Err(format!("无法读取配置 {}: {e}", path.display()));
            }
        };
        match toml::from_str::<FileConfig>(&text) {
            Ok(fc) => {
                if let Some(v) = fc.auth_backend {
                    cfg.auth_backend = match v.to_ascii_lowercase().as_str() {
                        "auto" => AuthBackend::Auto,
                        "pam" => AuthBackend::Pam,
                        "greetd" => AuthBackend::Greetd,
                        other => {
                            eprintln!("phigrauth: unknown auth_backend {other:?}, using auto");
                            AuthBackend::Auto
                        }
                    };
                }
                if let Some(v) = fc.pam_service {
                    cfg.pam_service = v;
                }
                cfg.default_user = fc.default_user.filter(|s| !s.is_empty());
                cfg.default_session = fc.default_session.filter(|s| !s.is_empty());
                if let Some(dirs) = fc.sessions_dirs {
                    cfg.sessions_dirs = dirs.into_iter().map(PathBuf::from).collect();
                }
                if let Some(cmd) = fc.poweroff_command {
                    if !cmd.is_empty() {
                        cfg.poweroff = cmd;
                    }
                }
                if let Some(cmd) = fc.reboot_command {
                    if !cmd.is_empty() {
                        cfg.reboot = cmd;
                    }
                }
                if let Some(v) = fc.launch_session {
                    cfg.launch_session = v;
                }
                cfg.tty_shell_command = fc.tty_shell_command.filter(|v| !v.is_empty());
            }
            Err(e) => {
                return Err(format!("无法解析配置 {}: {e}", path.display()));
            }
        }
        Ok(cfg)
    }
}

fn default_sessions_dirs() -> Vec<PathBuf> {
    let data_dirs =
        std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let mut out = Vec::new();
    for base in data_dirs.split(':').filter(|s| !s.is_empty()) {
        let base = PathBuf::from(base);
        out.push(base.join("wayland-sessions"));
        out.push(base.join("xsessions"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config(contents: Option<&str>) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "phigrauth-config-test-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        if let Some(contents) = contents {
            std::fs::write(&path, contents).unwrap();
        }
        path
    }

    #[test]
    fn missing_file_uses_builtin_defaults() {
        let path = temp_config(None);
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.pam_service, "login");
        assert!(cfg.launch_session);
        assert!(!path.exists());
    }

    #[test]
    fn present_but_invalid_file_is_fatal() {
        let path = temp_config(Some("pam_service = ["));
        let result = Config::load_from(&path);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }
}
