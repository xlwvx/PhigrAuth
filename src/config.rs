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
    /// Load configuration.  An explicitly requested `--config` path is fatal
    /// when it cannot be read or parsed; an automatically discovered file only
    /// emits a warning and keeps defaults.
    pub fn load(path: Option<&Path>) -> Result<Config, String> {
        let explicit = path.is_some();
        let mut cfg = Config::default();
        let path = path.map(|p| p.to_path_buf()).or_else(default_config_path);
        let Some(path) = path else {
            return Ok(cfg);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if explicit => {
                return Err(format!("无法读取配置 {}: {e}", path.display()));
            }
            Err(_) => return Ok(cfg),
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
            Err(e) if explicit => {
                return Err(format!("无法解析配置 {}: {e}", path.display()));
            }
            Err(e) => {
                eprintln!("phigrauth: invalid config {}: {e}", path.display());
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

fn default_config_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("XDG_CONFIG_HOME") {
        let p = PathBuf::from(dir).join("phigrauth/config.toml");
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".config/phigrauth/config.toml");
        if p.exists() {
            return Some(p);
        }
    }
    let p = PathBuf::from("/etc/phigrauth/config.toml");
    if p.exists() {
        return Some(p);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_missing_config_is_fatal() {
        let path = Path::new("/definitely/not/a/phigrauth/config.toml");
        assert!(Config::load(Some(path)).is_err());
    }

    #[test]
    fn explicit_invalid_config_is_fatal() {
        let path = std::env::temp_dir().join(format!(
            "phigrauth-invalid-config-{}.toml",
            std::process::id()
        ));
        std::fs::write(&path, "pam_service = [").unwrap();
        let result = Config::load(Some(&path));
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
    }
}
