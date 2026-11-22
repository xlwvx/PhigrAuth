use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionKind {
    Wayland,
    X11,
    Tty,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub name: String,
    pub exec: String,
    pub kind: SessionKind,
    pub desktop_names: Option<String>,
    pub desktop_id: Option<String>,
}

impl Session {
    /// Name shown in the greeter.  The desktop entry id (DesktopNames) is
    /// preferred because that is what the reference art uses ("niri" rather
    /// than "Niri").
    pub fn display(&self) -> &str {
        self.desktop_names
            .as_deref()
            .and_then(|d| d.split(':').next())
            .filter(|d| !d.is_empty())
            .unwrap_or(&self.name)
    }
}

#[derive(Clone, Debug)]
pub struct UserInfo {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
}

pub fn current_user() -> UserInfo {
    let uid = unsafe { libc::geteuid() };
    user_by_uid(uid).unwrap_or_else(|| UserInfo {
        name: std::env::var("USER").unwrap_or_else(|_| "user".into()),
        uid,
        gid: unsafe { libc::getegid() },
        home: std::env::var("HOME").unwrap_or_else(|_| "/".into()),
        shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
    })
}

fn passwd_to_user(pw: &libc::passwd) -> UserInfo {
    UserInfo {
        name: unsafe { cstr(pw.pw_name) },
        uid: pw.pw_uid,
        gid: pw.pw_gid,
        home: unsafe { cstr(pw.pw_dir) },
        shell: unsafe { cstr(pw.pw_shell) },
    }
}

pub fn user_by_uid(uid: u32) -> Option<UserInfo> {
    let mut buf: Vec<libc::c_char> = vec![0; 16 * 1024];
    loop {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc =
            unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut result) };
        if rc == libc::ERANGE {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        return Some(passwd_to_user(&pwd));
    }
}

pub fn user_by_name(name: &str) -> Option<UserInfo> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut buf: Vec<libc::c_char> = vec![0; 16 * 1024];
    loop {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwnam_r(
                c.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        return Some(passwd_to_user(&pwd));
    }
}

unsafe fn cstr(p: *const libc::c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}

fn parse_desktop(path: &Path, kind: SessionKind) -> Option<Session> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut name = None;
    let mut exec = None;
    let mut desktop_names = None;
    let mut try_exec = None;
    let mut localized_names: Vec<(String, String)> = Vec::new();
    let mut hidden = false;
    let mut no_display = false;
    let mut section = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_string();
            continue;
        }
        if section != "Desktop Entry" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "Name" {
            name = Some(value.to_string());
        } else if let Some(locale) = key.strip_prefix("Name[").and_then(|k| k.strip_suffix(']')) {
            localized_names.push((locale.to_string(), value.to_string()));
        } else {
            match key {
                "Exec" => exec = Some(value.to_string()),
                "DesktopNames" => desktop_names = Some(value.to_string()),
                "TryExec" => try_exec = Some(value.to_string()),
                "Hidden" => hidden = value.eq_ignore_ascii_case("true"),
                "NoDisplay" => no_display = value.eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
    }
    if hidden || no_display {
        return None;
    }
    if let Some(try_exec) = try_exec.filter(|s| !s.is_empty()) {
        if !command_exists(&try_exec) {
            return None;
        }
    }
    let name = localized_name(name, &localized_names).filter(|s| !s.is_empty())?;
    let exec = exec.filter(|s| !s.is_empty())?;
    let desktop_id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty());
    Some(Session {
        name,
        exec,
        kind,
        desktop_names,
        desktop_id,
    })
}

fn command_exists(program: &str) -> bool {
    let path = Path::new(program);
    if path.is_absolute() {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file()
        })
    })
}

fn localized_name(default: Option<String>, localized: &[(String, String)]) -> Option<String> {
    let locale = std::env::var("LC_ALL")
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .or_else(|_| std::env::var("LANG"))
        .unwrap_or_default();
    let locale = locale.split('.').next().unwrap_or("").trim();
    if !locale.is_empty() && locale != "C" && locale != "POSIX" {
        if let Some((_, name)) = localized
            .iter()
            .find(|(lang, _)| lang.eq_ignore_ascii_case(locale))
            .filter(|(_, name)| !name.is_empty())
        {
            return Some(name.clone());
        }
        let lang = locale.split('_').next().unwrap_or(locale);
        if let Some((_, name)) = localized
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(lang))
            .filter(|(_, name)| !name.is_empty())
        {
            return Some(name.clone());
        }
    }
    default
}

/// Synthetic "tty shell" session appended to the environment list.  An empty
/// `exec` means "use the authenticated user's login shell"; launch_session
/// resolves it after authentication so the correct user's shell is used.
pub fn tty_shell_session(command: Option<&[String]>) -> Session {
    Session {
        name: "tty shell".into(),
        exec: command.map(|c| c.join(" ")).unwrap_or_default(),
        kind: SessionKind::Tty,
        desktop_names: Some("tty shell".into()),
        desktop_id: Some("tty-shell".into()),
    }
}

pub fn discover(dirs: &[PathBuf]) -> Vec<Session> {
    let mut out = Vec::new();
    for dir in dirs {
        let kind = if dir.file_name().and_then(|n| n.to_str()) == Some("xsessions") {
            SessionKind::X11
        } else {
            SessionKind::Wayland
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "desktop").unwrap_or(false))
            .collect();
        files.sort();
        for file in files {
            if let Some(session) = parse_desktop(&file, kind) {
                let duplicate =
                    out.iter()
                        .any(|s: &Session| match (&s.desktop_id, &session.desktop_id) {
                            (Some(a), Some(b)) => a == b,
                            _ => s.name == session.name,
                        });
                if !duplicate {
                    out.push(session);
                }
            }
        }
    }
    out
}

pub fn pick_default(sessions: &[Session], wanted: Option<&str>, current: Option<&str>) -> usize {
    if let Some(wanted) = wanted {
        if let Some(i) = sessions.iter().position(|s| {
            s.name.eq_ignore_ascii_case(wanted)
                || s.desktop_id
                    .as_deref()
                    .is_some_and(|id| id.eq_ignore_ascii_case(wanted))
        }) {
            return i;
        }
        if let Some(i) = sessions
            .iter()
            .position(|s| s.name.to_lowercase().contains(&wanted.to_lowercase()))
        {
            return i;
        }
    }
    if let Some(current) = current {
        if let Some(i) = sessions.iter().position(|s| {
            s.name.eq_ignore_ascii_case(current)
                || s.desktop_names
                    .as_deref()
                    .map(|d| d.split(':').any(|x| x.eq_ignore_ascii_case(current)))
                    .unwrap_or(false)
        }) {
            return i;
        }
    }
    0
}

/// Strip XDG desktop-entry field codes from an Exec line.
pub fn clean_exec(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            if let Some(&next) = chars.peek() {
                if next == '%' {
                    chars.next();
                    out.push('%');
                } else {
                    chars.next();
                }
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

fn niri_program() -> String {
    // Command::new("niri") would be resolved with greetd's PATH, not the PATH
    // supplied in start_session's env.  Use an absolute path when we can.
    for candidate in ["/usr/bin/niri", "/usr/local/bin/niri"] {
        if Path::new(candidate).exists() {
            return candidate.to_string();
        }
    }
    "niri".to_string()
}

fn niri_session_exec(exec: &str) -> bool {
    let program = exec.split_whitespace().next().unwrap_or("");
    program == "niri-session" || program.ends_with("/niri-session")
}

/// Command and extra environment for greetd's start_session.  greetd performs
/// the actual spawn as the authenticated user.
pub fn greetd_spec(session: &Session, user: &UserInfo) -> (Vec<String>, Vec<String>) {
    let shell = if user.shell.is_empty() || user.shell.ends_with("nologin") {
        "/bin/sh"
    } else {
        &user.shell
    };
    let cmd = if session.kind == SessionKind::Tty && session.exec.is_empty() {
        vec![shell.to_string(), "-l".to_string()]
    } else {
        let exec = clean_exec(&session.exec);
        // niri-session starts niri through a freshly-created systemd user
        // manager.  When greetd starts the session immediately after PAM, that
        // manager can still be starting, and niri-session exits before
        // starting niri.service.  niri --session is the same command used by
        // niri.service itself, and does not depend on that race.
        if niri_session_exec(&exec) {
            vec![niri_program(), "--session".to_string()]
        } else {
            vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                format!("exec {exec}"),
            ]
        }
    };

    let mut env = Vec::new();
    match session.kind {
        SessionKind::Wayland => env.push("XDG_SESSION_TYPE=wayland".to_string()),
        SessionKind::X11 => env.push("XDG_SESSION_TYPE=x11".to_string()),
        SessionKind::Tty => {
            env.push("XDG_SESSION_TYPE=tty".to_string());
            env.push("TERM=linux".to_string());
        }
    }
    env.push(format!("XDG_SESSION_DESKTOP={}", session.display()));
    env.push(format!(
        "XDG_CURRENT_DESKTOP={}",
        session
            .desktop_names
            .clone()
            .unwrap_or_else(|| session.display().to_string())
    ));
    // Do not override XDG_RUNTIME_DIR or PATH here: PAM/pam_systemd already
    // created them, and start_session env is appended on top of that.
    (cmd, env)
}

pub fn launch_session(session: &Session, user: &UserInfo) -> std::io::Result<()> {
    let cmd = if session.kind == SessionKind::Tty && session.exec.is_empty() {
        let shell = if user.shell.is_empty() || user.shell.ends_with("nologin") {
            "/bin/sh"
        } else {
            &user.shell
        };
        format!("{shell} -l")
    } else {
        clean_exec(&session.exec)
    };
    if cmd.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "empty Exec line",
        ));
    }

    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(format!("exec {cmd}"));
    command
        .env_clear()
        .env("HOME", &user.home)
        .env("USER", &user.name)
        .env("LOGNAME", &user.name)
        .env("SHELL", &user.shell)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("XDG_SESSION_DESKTOP", session.display())
        .env(
            "XDG_CURRENT_DESKTOP",
            session
                .desktop_names
                .clone()
                .unwrap_or_else(|| session.display().to_string()),
        )
        .env("XDG_RUNTIME_DIR", format!("/run/user/{}", user.uid))
        .current_dir(if user.home.is_empty() {
            "/"
        } else {
            &user.home
        });

    match session.kind {
        SessionKind::Wayland => {
            command.env("XDG_SESSION_TYPE", "wayland");
        }
        SessionKind::X11 => {
            command.env("XDG_SESSION_TYPE", "x11");
        }
        SessionKind::Tty => {
            command.env("TERM", "linux");
        }
    }

    if unsafe { libc::geteuid() } == 0 {
        let name = std::ffi::CString::new(user.name.clone()).unwrap_or_default();
        let uid = user.uid;
        let gid = user.gid;
        unsafe {
            command.pre_exec(move || {
                if libc::initgroups(name.as_ptr(), gid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setgid(gid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setuid(uid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = command.spawn()?;
    // Reap in the background; the greeter itself does not need to wait.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub fn run_power(command: &[String]) -> Result<(), String> {
    let Some(program) = command.first() else {
        return Err("未配置电源命令".into());
    };
    let output = Command::new(program)
        .args(&command[1..])
        .output()
        .map_err(|e| format!("无法执行 {program}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let msg = stderr.trim();
        let msg = if msg.is_empty() { stdout.trim() } else { msg };
        Err(if msg.is_empty() {
            format!("{program} 返回 {}", output.status)
        } else {
            msg.lines().next().unwrap_or(msg).to_string()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> UserInfo {
        UserInfo {
            name: "alice".into(),
            uid: 1000,
            gid: 1000,
            home: "/home/alice".into(),
            shell: "/bin/zsh".into(),
        }
    }

    #[test]
    fn greetd_spec_for_wayland_session() {
        let session = Session {
            name: "niri".into(),
            exec: "niri-session".into(),
            kind: SessionKind::Wayland,
            desktop_names: Some("niri".into()),
            desktop_id: Some("niri".into()),
        };
        let (cmd, env) = greetd_spec(&session, &user());
        assert!(cmd[0].ends_with("niri"));
        assert_eq!(cmd[1], "--session");
        assert!(env.iter().any(|e| e == "XDG_SESSION_TYPE=wayland"));
        assert!(env.iter().any(|e| e == "XDG_SESSION_DESKTOP=niri"));
        assert!(!env.iter().any(|e| e.starts_with("XDG_RUNTIME_DIR=")));
        assert!(!env.iter().any(|e| e.starts_with("PATH=")));
    }

    #[test]
    fn greetd_spec_uses_users_shell_for_tty_shell() {
        let session = tty_shell_session(None);
        let (cmd, env) = greetd_spec(&session, &user());
        assert_eq!(cmd, vec!["/bin/zsh".to_string(), "-l".to_string()]);
        assert!(env.iter().any(|e| e == "XDG_SESSION_TYPE=tty"));
        assert!(env.iter().any(|e| e == "TERM=linux"));
    }
}
