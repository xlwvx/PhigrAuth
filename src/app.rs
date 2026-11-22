use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::auth::{self, AuthOutcome};
use crate::config::Config;
use crate::greetd;
use crate::scene::{
    self, BarVis, Palette, Rect, SubtitleKind, TitleKind, VisualState, ENV_TEXT_X, ENV_TEXT_Y,
};
use crate::sessions::{self, Session, SessionKind, UserInfo};

pub const BOOT_DURATION: f32 = 2.0;
pub const VERIFY_MIN: f32 = 0.9;
pub const GRANTED_HOLD: f32 = 0.9;
pub const EXIT_DURATION: f32 = 1.0;
pub const RED_FLASH: f32 = 0.1;
pub const WIPE_DURATION: f32 = 0.42;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Boot,
    Login,
    Verifying,
    Granted,
    Exiting,
    Powering,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    User,
    Password,
    Env,
}

impl Focus {
    pub fn index(self) -> usize {
        match self {
            Focus::User => 0,
            Focus::Password => 1,
            Focus::Env => 2,
        }
    }
    fn next(self) -> Focus {
        match self {
            Focus::User => Focus::Password,
            Focus::Password => Focus::Env,
            Focus::Env => Focus::User,
        }
    }
    fn prev(self) -> Focus {
        match self {
            Focus::User => Focus::Env,
            Focus::Password => Focus::User,
            Focus::Env => Focus::Password,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Left,
    Right,
    Up,
    Down,
    Enter,
    Backspace,
    Delete,
    Clear,
    Char(char),
    F1,
    F2,
    Escape,
}

pub struct WipeAnim {
    pub start: Instant,
    pub rect: Rect,
}

struct Anim<T: Copy + PartialEq> {
    prev: T,
    cur: T,
    started: Option<Instant>,
    duration: f32,
}

impl<T: Copy + PartialEq> Anim<T> {
    fn new(value: T) -> Self {
        Anim {
            prev: value,
            cur: value,
            started: None,
            duration: 0.18,
        }
    }
    fn set(&mut self, value: T, now: Instant) {
        if value != self.cur {
            self.prev = self.cur;
            self.cur = value;
            self.started = Some(now);
        }
    }
    /// Jump to a value without a crossfade.  Used when the red failure flash
    /// is already covering the screen, so retry.svg is fully settled by the
    /// time the flash has faded.
    fn snap(&mut self, value: T) {
        self.prev = value;
        self.cur = value;
        self.started = None;
    }
    fn sample(&self, now: Instant) -> (T, T, f32) {
        match self.started {
            Some(start) => {
                let t = now.saturating_duration_since(start).as_secs_f32() / self.duration;
                if t >= 1.0 {
                    (self.cur, self.cur, 1.0)
                } else {
                    (self.prev, self.cur, smoothstep(t))
                }
            }
            None => (self.cur, self.cur, 1.0),
        }
    }
}

pub struct App {
    pub config: Config,
    pub user_info: UserInfo,
    pub sessions: Vec<Session>,
    pub session_idx: usize,
    pub user: String,
    pub password: String,
    pub focus: Focus,

    pub phase: Phase,
    pub t0: Instant,
    pub phase_start: Instant,

    palette: Anim<Palette>,
    title: Anim<TitleKind>,
    subtitle: Anim<SubtitleKind>,

    auth_rx: Option<Receiver<AuthOutcome>>,
    auth_result: Option<AuthOutcome>,
    auth_cancel: Option<Arc<AtomicBool>>,
    greetd_session: Option<greetd::Session>,
    verifying_start: Option<Instant>,
    flash_start: Option<Instant>,
    granted_start: Option<Instant>,
    exit_start: Option<Instant>,
    pub wipe: Option<WipeAnim>,

    power_tx: Option<Receiver<Result<(), String>>>,
    power_start: Option<Instant>,
    power_label: String,
    pub dry_power: bool,

    pub toast: Option<(String, Instant)>,
    pub quit_requested: bool,
    last_change: Instant,
}

impl App {
    pub fn new(config: Config, now: Instant) -> App {
        let user_info = sessions::current_user();
        let sessions = {
            let mut s = sessions::discover(&config.sessions_dirs);
            if s.is_empty() {
                let name = config
                    .default_session
                    .clone()
                    .unwrap_or_else(|| "niri".into());
                s.push(Session {
                    exec: name.clone(),
                    name,
                    kind: SessionKind::Wayland,
                    desktop_names: None,
                    desktop_id: None,
                });
            }
            if !s
                .iter()
                .any(|session| session.name.eq_ignore_ascii_case("tty shell"))
            {
                s.push(sessions::tty_shell_session(
                    config.tty_shell_command.as_deref(),
                ));
            }
            s
        };
        let current = std::env::var("XDG_CURRENT_DESKTOP").ok();
        let session_idx = sessions::pick_default(
            &sessions,
            config.default_session.as_deref(),
            current.as_deref(),
        );
        let default_user = config
            .default_user
            .clone()
            .or_else(|| {
                std::env::var("PHIGRAUTH_USER")
                    .ok()
                    .filter(|name| !name.is_empty())
            })
            .unwrap_or_default();

        App {
            config,
            user_info,
            sessions,
            session_idx,
            user: default_user,
            password: String::new(),
            focus: Focus::Password,
            phase: Phase::Boot,
            t0: now,
            phase_start: now,
            palette: Anim::new(Palette::Red),
            title: Anim::new(TitleKind::Denied),
            subtitle: Anim::new(SubtitleKind::Denied),
            auth_rx: None,
            auth_result: None,
            auth_cancel: None,
            greetd_session: None,
            verifying_start: None,
            flash_start: None,
            granted_start: None,
            exit_start: None,
            wipe: None,
            power_tx: None,
            power_start: None,
            power_label: String::new(),
            dry_power: false,
            toast: None,
            quit_requested: false,
            last_change: now,
        }
    }

    pub fn env_name(&self) -> &str {
        self.sessions
            .get(self.session_idx)
            .map(|s| s.display())
            .unwrap_or("")
    }

    fn set_palette(&mut self, palette: Palette, now: Instant) {
        self.palette.set(palette, now);
    }

    fn set_title(&mut self, title: TitleKind, now: Instant) {
        self.title.set(title, now);
    }

    fn set_subtitle(&mut self, subtitle: SubtitleKind, now: Instant) {
        self.subtitle.set(subtitle, now);
    }

    pub fn boot_elapsed(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.t0).as_secs_f32()
    }

    pub fn tick(&mut self, now: Instant) {
        if self.quit_requested {
            return;
        }
        if let Some(rx) = &self.auth_rx {
            if let Ok(outcome) = rx.try_recv() {
                self.auth_result = Some(outcome);
                self.auth_rx = None;
                self.auth_cancel = None;
            }
        }
        if let Some(rx) = &self.power_tx {
            if let Ok(result) = rx.try_recv() {
                self.power_tx = None;
                match result {
                    Ok(()) => {
                        // Keep the "powering" screen up: exiting here lets
                        // greetd restart the greeter and replay the boot
                        // animation just before the machine goes down.
                        self.toast = Some((
                            format!("已请求{}，等待系统关闭…", self.power_label),
                            now + Duration::from_secs(30),
                        ));
                        self.last_change = now;
                    }
                    Err(e) => {
                        self.phase = Phase::Login;
                        self.phase_start = now;
                        self.set_palette(Palette::Red, now);
                        self.set_title(TitleKind::Denied, now);
                        self.set_subtitle(SubtitleKind::Denied, now);
                        self.last_change = now;
                        self.toast = Some((format!("操作失败: {e}"), now + Duration::from_secs(4)));
                    }
                }
            }
        }

        match self.phase {
            Phase::Boot => {
                if now.saturating_duration_since(self.t0).as_secs_f32() >= BOOT_DURATION {
                    self.phase = Phase::Login;
                    self.phase_start = now;
                    self.focus = Focus::Password;
                    self.last_change = now;
                }
            }
            Phase::Verifying => {
                let elapsed = now
                    .saturating_duration_since(*self.verifying_start.get_or_insert(now))
                    .as_secs_f32();
                if elapsed >= VERIFY_MIN {
                    if let Some(outcome) = self.auth_result.take() {
                        self.auth_cancel = None;
                        match outcome {
                            AuthOutcome::Success => {
                                eprintln!("phigrauth: authentication succeeded for {}", self.user);
                                self.enter_granted(now);
                            }
                            AuthOutcome::GreetdSuccess(session) => {
                                eprintln!(
                                    "phigrauth: greetd authenticated {}; session ready",
                                    self.user
                                );
                                self.greetd_session = Some(session);
                                self.enter_granted(now);
                            }
                            AuthOutcome::Failure(msg) => {
                                eprintln!(
                                    "phigrauth: authentication failed for {}: {msg}",
                                    self.user
                                );
                                self.fail_and_retry(now, None);
                            }
                            AuthOutcome::Error(msg) => {
                                eprintln!(
                                    "phigrauth: authentication error for {}: {msg}",
                                    self.user
                                );
                                self.fail_and_retry(now, Some(msg));
                            }
                        }
                    } else if elapsed > 20.0 {
                        if let Some(cancel) = &self.auth_cancel {
                            cancel.store(true, Ordering::Relaxed);
                        }
                        self.auth_rx = None;
                        self.auth_cancel = None;
                        self.fail_and_retry(now, Some("认证超时".into()));
                    }
                }
            }
            Phase::Granted => {
                let elapsed = now
                    .saturating_duration_since(*self.granted_start.get_or_insert(now))
                    .as_secs_f32();
                if elapsed >= GRANTED_HOLD {
                    self.phase = Phase::Exiting;
                    self.exit_start = Some(now);
                    self.phase_start = now;
                    self.last_change = now;
                }
            }
            Phase::Exiting => {
                let elapsed = now
                    .saturating_duration_since(*self.exit_start.get_or_insert(now))
                    .as_secs_f32();
                if elapsed >= EXIT_DURATION {
                    if self.config.launch_session {
                        let launch_user = sessions::user_by_name(&self.user)
                            .unwrap_or_else(|| self.user_info.clone());
                        if let Some(mut session) = self.greetd_session.take() {
                            // greetd performs authentication and the spawn as
                            // the authenticated user.  After start_session is
                            // acknowledged it starts the session as soon as
                            // this greeter process exits.
                            if let Some(selected) = self.sessions.get(self.session_idx) {
                                let (cmd, env) = sessions::greetd_spec(selected, &launch_user);
                                eprintln!("phigrauth: starting greetd session {cmd:?}");
                                if let Err(e) = session.start(cmd, env) {
                                    self.session_error(now, format!("greetd 无法启动会话: {e}"));
                                    return;
                                }
                                self.quit_requested = true;
                                return;
                            }
                        }
                        if let Some(selected) = self.sessions.get(self.session_idx) {
                            if selected.kind != SessionKind::Tty {
                                self.session_error(
                                    now,
                                    "本地 PAM 模式不支持图形会话；请通过 greetd 启动".into(),
                                );
                                return;
                            }
                            match sessions::launch_session(selected, &launch_user) {
                                Ok(()) => {}
                                Err(e) => {
                                    self.session_error(now, format!("无法启动会话: {e}"));
                                    return;
                                }
                            }
                        }
                    } else {
                        self.greetd_session = None;
                    }
                    self.quit_requested = true;
                }
            }
            Phase::Powering => {
                let elapsed = now
                    .saturating_duration_since(*self.power_start.get_or_insert(now))
                    .as_secs_f32();
                if self.dry_power && elapsed >= 0.35 {
                    self.phase = Phase::Login;
                    self.phase_start = now;
                    self.last_change = now;
                    self.toast = Some((
                        format!("（演示）已拦截{}", self.power_label),
                        now + Duration::from_secs(3),
                    ));
                }
            }
            Phase::Login => {}
        }

        if let Some((_, until)) = &self.toast {
            if now >= *until {
                self.toast = None;
            }
        }
    }

    pub fn needs_frame(&self, now: Instant) -> bool {
        if now
            .saturating_duration_since(self.last_change)
            .as_secs_f32()
            < 0.6
        {
            return true;
        }
        // Powering is a static screen; keep polling the worker without
        // re-rasterizing the whole SVG scene at refresh rate.
        if self.phase == Phase::Powering {
            return false;
        }
        // Boot/Granted/Exiting have continuous animation; Verifying is a
        // static screen after its title/palette crossfade, so only redraw when
        // those animations, the flash, or a toast are still active.
        if !matches!(self.phase, Phase::Login | Phase::Verifying) {
            return true;
        }
        if self.auth_rx.is_some() && self.phase != Phase::Verifying {
            return true;
        }
        // animations still running?
        let (p0, p1, _) = self.palette.sample(now);
        if p0 != p1 {
            return true;
        }
        let (t0, t1, _) = self.title.sample(now);
        if t0 != t1 {
            return true;
        }
        let (s0, s1, _) = self.subtitle.sample(now);
        if s0 != s1 {
            return true;
        }
        if let Some(f) = self.flash_start {
            if now.saturating_duration_since(f).as_secs_f32() < RED_FLASH {
                return true;
            }
        }
        if let Some(w) = &self.wipe {
            if now.saturating_duration_since(w.start).as_secs_f32() < WIPE_DURATION {
                return true;
            }
        }
        self.toast.is_some()
    }

    pub fn visual(&self, now: Instant) -> VisualState {
        let (pal_prev, pal_cur, pal_mix) = self.palette.sample(now);
        let palette = if pal_mix < 1.0 {
            pal_prev.colors().mix(pal_cur.colors(), pal_mix)
        } else {
            pal_cur.colors()
        };
        let (title_prev, title_cur, title_mix) = self.title.sample(now);
        let (sub_prev, sub_cur, subtitle_mix) = self.subtitle.sample(now);

        let mut st = VisualState {
            palette,
            title: title_cur,
            title_prev: if title_mix < 0.999 {
                Some(title_prev)
            } else {
                None
            },
            title_mix,
            subtitle: sub_cur,
            subtitle_prev: if subtitle_mix < 0.999 {
                Some(sub_prev)
            } else {
                None
            },
            subtitle_mix,
            ..VisualState::default()
        };

        // bar contents
        let login = self.phase == Phase::Login;
        let focused = |idx: usize| login && self.focus.index() == idx;
        let user_text = if self.user.is_empty() {
            "user".to_string()
        } else {
            format!("user: {}", self.user)
        };
        st.bars = [
            BarVis {
                text: if focused(0) {
                    self.user.clone()
                } else {
                    user_text
                },
                focused: focused(0),
            },
            BarVis {
                text: if focused(1) {
                    "*".repeat(self.password.chars().count())
                } else {
                    "password".into()
                },
                focused: focused(1),
            },
            BarVis {
                text: if focused(2) {
                    self.env_name().to_string()
                } else {
                    format!("env: {}", self.env_name())
                },
                focused: focused(2),
            },
        ];

        // boot animation
        if self.phase == Phase::Boot {
            let bt = self.boot_elapsed(now);
            let p = (bt / 1.0).clamp(0.0, 1.0);
            // Both entering steps use a quadratic ease-out: fast at first,
            // then slowing down towards the target state.
            let e = ease_out_quad(p);
            st.bg_alpha = e;
            st.title_scale = 0.62 + 0.38 * e;
            st.title_alpha = e;
            st.arrows_spread = scene::ARROW_SPREAD * (1.0 - ease_out_quad(p));
            st.subtitle_alpha = ease_out_quad(((bt - 0.15) / 0.7).clamp(0.0, 1.0));
            st.elem_y = scene::BOOT_ELEM_DROP;
            st.bars_y = scene::BOOT_BARS_DROP;
            st.footer_alpha = 0.0;

            // The big word blinks twice starting at 0.7s.
            if (0.700..0.745).contains(&bt) || (0.775..0.820).contains(&bt) {
                st.title_alpha = 0.0;
            }
            if bt >= 1.0 {
                let q = ((bt - 1.0) / 1.0).clamp(0.0, 1.0);
                let e2 = ease_out_quad(q);
                st.elem_y = scene::BOOT_ELEM_DROP * (1.0 - e2);
                st.bars_y = scene::BOOT_BARS_DROP * (1.0 - e2);
                // The bars blink twice as they settle, starting at 1.7s.
                if (1.700..1.745).contains(&bt) || (1.775..1.820).contains(&bt) {
                    st.bars_alpha = 0.0;
                }
            }
        } else {
            st.bg_alpha = 1.0;
            st.elem_y = 0.0;
            st.bars_y = 0.0;
            st.arrows_spread = 0.0;
            st.footer_alpha = 1.0;
            if self.phase == Phase::Login {
                // The footer fades in right after the boot animation.
                let bt = self.boot_elapsed(now);
                st.footer_alpha = smoothstep(((bt - 2.0) / 0.2).clamp(0.0, 1.0));
            }
            if self.phase == Phase::Verifying {
                st.footer_alpha = 0.55;
            }
        }

        // screen shake / failure flash
        if let Some(start) = self.flash_start {
            let p = now.saturating_duration_since(start).as_secs_f32() / RED_FLASH;
            if p < 1.0 {
                st.red_flash = 1.0 - p;
            }
        }

        // success exit
        if self.phase == Phase::Exiting {
            if let Some(start) = self.exit_start {
                let p = (now.saturating_duration_since(start).as_secs_f32() / EXIT_DURATION)
                    .clamp(0.0, 1.0);
                st.rise = 330.0 * ease_in_cubic(p);
                st.bg_dim = smoothstep(p);
            }
        }

        // environment wipe
        if let Some(w) = &self.wipe {
            let p = (now.saturating_duration_since(w.start).as_secs_f32() / WIPE_DURATION)
                .clamp(0.0, 1.0);
            let reveal = if p < 0.2 {
                0.0
            } else {
                smoothstep((p - 0.2) / 0.8)
            };
            st.wipe = Some(scene::Wipe {
                reveal,
                x: w.rect.x,
                y: w.rect.y,
                w: w.rect.w,
                h: w.rect.h,
            });
        }

        st.footer_env_hint = login && self.focus == Focus::Env;
        st.toast = self.toast.as_ref().map(|(m, _)| m.clone());
        st
    }

    pub fn handle_key(&mut self, key: Key, now: Instant) {
        self.last_change = now;
        if key == Key::Escape {
            if self.phase == Phase::Verifying {
                self.cancel_verify(now);
            }
            return;
        }
        match key {
            Key::F1 | Key::F2 => {
                if matches!(self.phase, Phase::Login) {
                    let poweroff = matches!(key, Key::F1);
                    self.start_power(poweroff, now);
                }
                return;
            }
            _ => {}
        }
        if self.phase != Phase::Login {
            return;
        }
        match key {
            Key::Left => {
                self.wipe = None;
                self.focus = self.focus.prev();
            }
            Key::Right => {
                self.wipe = None;
                self.focus = self.focus.next();
            }
            Key::Up | Key::Down => {
                if self.focus == Focus::Env && self.sessions.len() > 1 {
                    let delta = if matches!(key, Key::Up) { -1 } else { 1 };
                    let n = self.sessions.len() as isize;
                    self.session_idx = ((self.session_idx as isize + delta).rem_euclid(n)) as usize;
                    self.start_wipe(now);
                }
            }
            Key::Enter => self.start_verify(now),
            Key::Backspace => match self.focus {
                Focus::User => {
                    self.user.pop();
                }
                Focus::Password => {
                    self.password.pop();
                }
                Focus::Env => {}
            },
            Key::Delete | Key::Clear => match self.focus {
                Focus::User => self.user.clear(),
                Focus::Password => self.wipe_password(),
                Focus::Env => {}
            },
            Key::Char(c) => match self.focus {
                Focus::User => {
                    if self.user.chars().count() < 64 {
                        self.user.push(c);
                    }
                }
                Focus::Password => {
                    if self.password.chars().count() < 128 {
                        self.password.push(c);
                    }
                }
                Focus::Env => {}
            },
            Key::F1 | Key::F2 | Key::Escape => {}
        }
    }

    fn session_error(&mut self, now: Instant, message: String) {
        self.phase = Phase::Login;
        self.phase_start = now;
        self.exit_start = None;
        self.set_palette(Palette::Red, now);
        self.set_title(TitleKind::Denied, now);
        self.set_subtitle(SubtitleKind::Denied, now);
        self.toast = Some((message, now + Duration::from_secs(4)));
        self.last_change = now;
    }

    fn enter_granted(&mut self, now: Instant) {
        self.phase = Phase::Granted;
        self.phase_start = now;
        self.granted_start = Some(now);
        self.wipe_password();
        self.set_palette(Palette::Green, now);
        self.set_title(TitleKind::Granted, now);
        self.set_subtitle(SubtitleKind::Granted, now);
        self.last_change = now;
    }

    fn cancel_verify(&mut self, now: Instant) {
        self.auth_result = None;
        self.auth_rx = None;
        self.fail_and_retry(now, Some("已取消认证".into()));
    }

    fn wipe_password(&mut self) {
        // Best-effort wipe before releasing the buffer.  The auth worker has
        // its own copy, but the long-lived UI buffer should not retain it.
        unsafe {
            self.password.as_mut_vec().fill(0);
        }
        self.password.clear();
    }

    fn fail_and_retry(&mut self, now: Instant, toast: Option<String>) {
        if let Some(cancel) = &self.auth_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        self.auth_cancel = None;
        self.greetd_session = None;
        self.phase = Phase::Login;
        self.phase_start = now;
        self.focus = Focus::Password;
        // The user explicitly asked for the password box to be cleared after
        // a failed verification.
        self.wipe_password();
        self.flash_start = Some(now);
        // Hide the verifying -> retry switch behind the opaque #550808 flash.
        // When the red fades, the retry state is already fully rendered.
        self.palette.snap(Palette::Red);
        self.title.snap(TitleKind::Denied);
        self.subtitle.snap(SubtitleKind::Retry);
        self.last_change = now;
        self.toast = toast.map(|m| (m, now + Duration::from_secs(4)));
    }

    fn start_wipe(&mut self, now: Instant) {
        let name = self.env_name().to_string();
        // text_ink is expressed around the anchor (60, 20); translate it into
        // the bar text anchor of the reference viewBox.  The wipe cover uses
        // the full line height so it reads as a solid block, not a thin bar.
        let (x, w) = match scene::text_ink(&name) {
            Some(ink) => (ENV_TEXT_X + (ink.x - 60.0) - 0.6, ink.w + 1.2),
            None => (ENV_TEXT_X - 15.0, 30.0),
        };
        self.wipe = Some(WipeAnim {
            start: now,
            rect: Rect {
                x,
                y: ENV_TEXT_Y - 7.3,
                w,
                h: 9.2,
            },
        });
    }

    fn start_verify(&mut self, now: Instant) {
        if self.user.is_empty() {
            self.focus = Focus::User;
            self.toast = Some(("请输入用户名".into(), now + Duration::from_secs(3)));
            return;
        }
        if self.phase != Phase::Login {
            return;
        }
        self.phase = Phase::Verifying;
        self.phase_start = now;
        self.verifying_start = Some(now);
        self.auth_result = None;
        self.greetd_session = None;
        self.flash_start = None;
        self.wipe = None;
        self.set_palette(Palette::Yellow, now);
        self.set_title(TitleKind::Verifying, now);
        self.set_subtitle(SubtitleKind::Verifying, now);
        let cancel = Arc::new(AtomicBool::new(false));
        self.auth_cancel = Some(cancel.clone());
        let (tx, rx): (Sender<AuthOutcome>, Receiver<AuthOutcome>) = channel();
        auth::spawn(
            self.config.auth_backend,
            self.config.pam_service.clone(),
            self.user.clone(),
            self.password.clone(),
            cancel,
            tx,
        );
        self.auth_rx = Some(rx);
    }

    fn start_power(&mut self, poweroff: bool, now: Instant) {
        let command = if poweroff {
            self.config.poweroff.clone()
        } else {
            self.config.reboot.clone()
        };
        self.power_label = if poweroff {
            "关机".into()
        } else {
            "重启".into()
        };
        self.phase = Phase::Powering;
        self.phase_start = now;
        self.power_start = Some(now);
        self.toast = Some((
            format!("正在{}…", self.power_label),
            now + Duration::from_secs(8),
        ));
        if self.dry_power {
            return;
        }
        let program = command.clone();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let result = sessions::run_power(&program);
            let _ = tx.send(result);
        });
        self.power_tx = Some(rx);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.wipe_password();
    }
}

pub fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

pub fn ease_out_quad(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x) * (1.0 - x)
}

pub fn ease_in_cubic(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * x
}

impl App {
    /// Construct an app in a specific settled visual state.  Used by the
    /// headless snapshot mode and by tests.
    pub fn demo(config: Config, phase: Phase, now: Instant) -> App {
        let mut app = App::new(config, now);
        app.phase = phase;
        app.phase_start = now;
        app.t0 = now - Duration::from_secs(3);
        app
    }

    pub fn settle(&mut self, palette: Palette, title: TitleKind, subtitle: SubtitleKind) {
        self.palette = Anim::new(palette);
        self.title = Anim::new(title);
        self.subtitle = Anim::new(subtitle);
    }

    pub fn set_boot_start(&mut self, t: Instant) {
        self.t0 = t;
        self.phase = Phase::Boot;
        self.phase_start = t;
        self.focus = Focus::Password;
    }

    pub fn set_flash_start(&mut self, t: Option<Instant>) {
        self.flash_start = t;
    }

    pub fn set_exit_start(&mut self, t: Option<Instant>) {
        self.exit_start = t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionKind;

    fn test_app() -> (App, Instant) {
        let t0 = Instant::now();
        let mut app = App::new(Config::default(), t0);
        app.sessions = vec![
            Session {
                name: "niri".into(),
                exec: "niri-session".into(),
                kind: SessionKind::Wayland,
                desktop_names: Some("niri".into()),
                desktop_id: Some("niri".into()),
            },
            Session {
                name: "COSMIC".into(),
                exec: "cosmic-session".into(),
                kind: SessionKind::Wayland,
                desktop_names: Some("COSMIC".into()),
                desktop_id: Some("cosmic".into()),
            },
        ];
        app.session_idx = 0;
        app.user = "xlwvx".into();
        (app, t0)
    }

    #[test]
    fn boot_moves_focus_to_password() {
        let (mut app, t0) = test_app();
        assert_eq!(app.phase, Phase::Boot);
        app.tick(t0 + Duration::from_millis(2100));
        assert_eq!(app.phase, Phase::Login);
        assert_eq!(app.focus, Focus::Password);
    }

    #[test]
    fn boot_visuals() {
        let (app, t0) = test_app();
        let mid = app.visual(t0 + Duration::from_millis(500));
        assert!(mid.bg_alpha > 0.05 && mid.bg_alpha < 1.0);
        assert!(mid.title_scale > 0.62 && mid.title_scale < 1.0);
        assert!(mid.bars_y > 0.0);
        let moving = app.visual(t0 + Duration::from_millis(1500));
        assert!(moving.elem_y > 0.0 && moving.elem_y < scene::BOOT_ELEM_DROP);
        let near_end = app.visual(t0 + Duration::from_millis(1950));
        assert!(near_end.bars_y < 1.0);
        assert!(near_end.elem_y < 0.5);
    }

    #[test]
    fn focus_cycles_and_wraps() {
        let (mut app, t0) = test_app();
        app.phase = Phase::Login;
        app.focus = Focus::Password;
        app.handle_key(Key::Right, t0);
        assert_eq!(app.focus, Focus::Env);
        app.handle_key(Key::Right, t0);
        assert_eq!(app.focus, Focus::User);
        app.handle_key(Key::Left, t0);
        assert_eq!(app.focus, Focus::Env);
    }

    #[test]
    fn password_is_masked_only_when_focused() {
        let (mut app, t0) = test_app();
        app.phase = Phase::Login;
        app.focus = Focus::Password;
        for c in "abc".chars() {
            app.handle_key(Key::Char(c), t0);
        }
        let st = app.visual(t0);
        assert_eq!(st.bars[1].text, "***");
        app.handle_key(Key::Left, t0); // user
        let st = app.visual(t0);
        assert_eq!(st.bars[1].text, "password");
        assert_eq!(st.bars[0].text, "xlwvx");
        assert_eq!(st.bars[2].text, "env: niri");
        assert!(!st.footer_env_hint);
        app.handle_key(Key::Left, t0); // env
        let st = app.visual(t0);
        assert_eq!(st.bars[2].text, "niri");
        assert!(st.footer_env_hint);
    }

    #[test]
    fn env_switch_starts_wipe() {
        let (mut app, t0) = test_app();
        app.phase = Phase::Login;
        app.focus = Focus::Env;
        app.handle_key(Key::Down, t0);
        assert_eq!(app.session_idx, 1);
        assert!(app.wipe.is_some());
        let st = app.visual(t0 + Duration::from_millis(20));
        assert_eq!(st.bars[2].text, "COSMIC");
        let w = st.wipe.expect("wipe");
        assert!(w.reveal < 0.5);
        let st2 = app.visual(t0 + Duration::from_millis(600));
        assert!(st2.wipe.is_none() || st2.wipe.unwrap().reveal > 0.99);
    }

    #[test]
    fn failure_clears_password() {
        let (mut app, t0) = test_app();
        app.phase = Phase::Login;
        app.focus = Focus::Password;
        app.password = "secret".into();
        app.fail_and_retry(t0, None);
        assert_eq!(app.password, "");
        assert_eq!(app.phase, Phase::Login);
        assert_eq!(app.focus, Focus::Password);
        let st = app.visual(t0);
        assert_eq!(st.bars[1].text, "");
        assert!(st.bars[1].focused);
        assert_eq!(st.title, TitleKind::Denied);
        assert_eq!(st.subtitle, SubtitleKind::Retry);
        assert!(st.subtitle_prev.is_none());
        assert!(st.title_prev.is_none());
    }

    #[test]
    fn tty_shell_is_available() {
        let config = Config {
            sessions_dirs: vec![],
            default_session: None,
            ..Config::default()
        };
        let app = App::new(config, Instant::now());
        assert!(app.sessions.iter().any(|s| s.display() == "tty shell"));
    }

    #[test]
    fn local_pam_refuses_graphical_session() {
        let (mut app, t0) = test_app();
        app.config.launch_session = true;
        app.phase = Phase::Exiting;
        app.exit_start = Some(t0 - Duration::from_secs(2));
        app.tick(t0);
        assert_eq!(app.phase, Phase::Login);
        assert!(app
            .toast
            .as_ref()
            .is_some_and(|(message, _)| message.contains("本地 PAM")));
    }

    #[test]
    fn power_success_does_not_exit_into_boot_animation() {
        let (mut app, t0) = test_app();
        app.phase = Phase::Powering;
        app.power_start = Some(t0);
        let (tx, rx) = channel();
        app.power_tx = Some(rx);
        tx.send(Ok(())).unwrap();
        app.tick(t0 + Duration::from_millis(10));
        assert_eq!(app.phase, Phase::Powering);
        assert!(!app.quit_requested);
    }

    #[test]
    fn escape_cancels_verifying() {
        let (mut app, t0) = test_app();
        app.phase = Phase::Verifying;
        app.verifying_start = Some(t0);
        let (_tx, rx) = channel();
        app.auth_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        app.auth_cancel = Some(cancel.clone());
        app.password = "secret".into();
        app.handle_key(Key::Escape, t0);
        assert_eq!(app.phase, Phase::Login);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(app.password.is_empty());
    }

    #[test]
    fn empty_user_shows_user_without_colon() {
        let (mut app, t0) = test_app();
        app.phase = Phase::Login;
        app.focus = Focus::Password;
        app.user.clear();
        let st = app.visual(t0);
        assert_eq!(st.bars[0].text, "user");
        app.focus = Focus::User;
        let st = app.visual(t0);
        assert_eq!(st.bars[0].text, "");
    }

    #[test]
    fn entering_ease_is_quadratic_ease_out() {
        assert!((ease_out_quad(0.0) - 0.0).abs() < 1e-6);
        assert!((ease_out_quad(0.5) - 0.75).abs() < 1e-6);
        assert!((ease_out_quad(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn retry_visual_has_single_subtitle() {
        let (mut app, t0) = test_app();
        app.phase = Phase::Login;
        app.settle(Palette::Red, TitleKind::Denied, SubtitleKind::Retry);
        let st = app.visual(t0);
        assert_eq!(st.subtitle, SubtitleKind::Retry);
        assert!(st.subtitle_prev.is_none());
    }
    #[test]
    fn greetd_success_starts_session_and_quits() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;
        use std::sync::{Arc, Mutex};

        use crate::config::AuthBackend;

        fn write_msg(stream: &mut std::os::unix::net::UnixStream, value: &serde_json::Value) {
            let payload = serde_json::to_vec(value).unwrap();
            stream
                .write_all(&(payload.len() as u32).to_ne_bytes())
                .unwrap();
            stream.write_all(&payload).unwrap();
            stream.flush().unwrap();
        }
        fn read_msg(stream: &mut std::os::unix::net::UnixStream) -> serde_json::Value {
            let mut len = [0u8; 4];
            stream.read_exact(&mut len).unwrap();
            let mut buf = vec![0u8; u32::from_ne_bytes(len) as usize];
            stream.read_exact(&mut buf).unwrap();
            serde_json::from_slice(&buf).unwrap()
        }

        let path =
            std::env::temp_dir().join(format!("phigrauth-app-greetd-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let recorded = Arc::new(Mutex::new(None::<serde_json::Value>));
        let recorded_server = recorded.clone();
        let server_path = path.clone();
        let server = std::thread::spawn(move || {
            let listener = UnixListener::bind(&server_path).unwrap();
            let (mut stream, _) = listener.accept().unwrap();
            let create = read_msg(&mut stream);
            assert_eq!(create["type"], "create_session");
            assert_eq!(create["username"], "alice");
            write_msg(
                &mut stream,
                &serde_json::json!({
                    "type": "auth_message",
                    "auth_message_type": "secret",
                    "auth_message": "Password:"
                }),
            );
            let answer = read_msg(&mut stream);
            assert_eq!(answer["type"], "post_auth_message_response");
            assert_eq!(answer["response"], "correcthorse");
            write_msg(&mut stream, &serde_json::json!({"type": "success"}));
            let start = read_msg(&mut stream);
            assert_eq!(start["type"], "start_session");
            *recorded_server.lock().unwrap() = Some(start);
            write_msg(&mut stream, &serde_json::json!({"type": "success"}));
            let _ = std::fs::remove_file(&server_path);
        });

        for _ in 0..100 {
            if path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        std::env::set_var("GREETD_SOCK", &path);

        let mut config = Config::default();
        config.auth_backend = AuthBackend::Auto;
        config.launch_session = true;
        let t0 = Instant::now();
        let mut app = App::new(config, t0);
        // Under greetd the process user is `greeter`, so the username must
        // not be prefilled with it.
        assert!(app.user.is_empty());
        app.t0 = t0 - Duration::from_secs(3);
        app.phase = Phase::Login;
        app.focus = Focus::Password;
        app.user = "alice".into();
        app.password = "correcthorse".into();
        app.sessions = vec![Session {
            name: "test".into(),
            exec: "true".into(),
            kind: SessionKind::Wayland,
            desktop_names: Some("test".into()),
            desktop_id: Some("test".into()),
        }];
        app.session_idx = 0;
        app.start_verify(t0);

        let deadline = Instant::now() + Duration::from_secs(8);
        while !app.quit_requested && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            app.tick(Instant::now());
        }
        std::env::remove_var("GREETD_SOCK");

        assert!(app.quit_requested, "app did not start a greetd session");
        server.join().unwrap();
        let start = recorded.lock().unwrap().take().expect("start_session");
        let cmd = start["cmd"].as_array().unwrap();
        assert_eq!(cmd[0], "/bin/sh");
        assert!(cmd[2].as_str().unwrap().contains("exec true"));
    }
}
