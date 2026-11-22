mod app;
mod auth;
mod config;
mod fonts;
mod greetd;
mod scene;
mod sessions;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::cairo;
use gtk::glib;
use gtk::prelude::*;

use app::{App, Focus, Key, Phase};
use config::Config;
use scene::{Palette, Renderer, SubtitleKind, TitleKind, VisualState};

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path: Option<String> = None;
    let mut snapshots: Option<String> = None;
    let mut dry_power = false;
    let mut windowed = false;
    let mut user_override: Option<String> = None;
    let mut dev_mock = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                config_path = args.get(i).cloned();
            }
            "--snapshots" => {
                i += 1;
                snapshots = args.get(i).cloned();
            }
            "--user" => {
                i += 1;
                user_override = args.get(i).cloned();
            }
            "--dry-power" => dry_power = true,
            "--dev-mock" => dev_mock = true,
            "--version" | "-V" => {
                println!("phigrauth {}", env!("CARGO_PKG_VERSION"));
                return glib::ExitCode::SUCCESS;
            }
            "--windowed" => windowed = true,
            "--help" | "-h" => {
                print_help();
                return glib::ExitCode::SUCCESS;
            }
            other => {
                eprintln!("phigrauth: unknown argument {other}");
                print_help();
                return glib::ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    if dev_mock {
        eprintln!("phigrauth: WARNING: development mock authentication is enabled");
        auth::enable_dev_mock();
    }
    let config = Config::load(config_path.as_deref().map(std::path::Path::new));

    if let Some(dir) = snapshots {
        return match render_snapshots(&dir, &config, user_override.as_deref()) {
            Ok(()) => glib::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("phigrauth: snapshot error: {e}");
                glib::ExitCode::FAILURE
            }
        };
    }

    run_gtk(config, dry_power, windowed, user_override)
}

fn print_help() {
    print!(
        r#"PhigrAuth — TTY session manager / greeter

Usage: phigrauth [options]

Options:
  --config <path>     configuration file
  --user <name>       default user name
  --dry-power         do not run systemctl for F1/F2 (for testing)
  --dev-mock          accept PHIGRAUTH_DEV_PASSWORD instead of PAM (dev only)
  --windowed          do not go fullscreen
  --snapshots <dir>   render reference snapshots and exit
  -h, --help          show this help
  -V, --version       show version

Environment:
  PHIGRAUTH_USER, PHIGRAUTH_DEV_PASSWORD (with --dev-mock)
"#
    );
}

fn run_gtk(
    config: Config,
    dry_power: bool,
    windowed: bool,
    user_override: Option<String>,
) -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id("dev.xlwvx.PhigrAuth")
        .build();

    let config = Rc::new(config);
    app.connect_activate(move |app| {
        // Build the renderer first: loading the system font database can take
        // a moment and should not eat into the boot animation.
        let renderer = Rc::new(RefCell::new(Renderer::new(1920, 1080)));
        let now = Instant::now();
        let mut state = App::new((*config).clone(), now);
        state.dry_power = dry_power;
        if let Some(user) = &user_override {
            state.user = user.clone();
        }
        let state = Rc::new(RefCell::new(state));

        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("PhigrAuth")
            .decorated(false)
            .build();
        window.set_default_size(1920, 1080);

        let provider = gtk::CssProvider::new();
        provider.load_from_data("window { background-color: #000000; }");
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        if let Some(cursor) = gtk::gdk::Cursor::from_name("none", None) {
            window.set_cursor(Some(&cursor));
        }

        let area = gtk::DrawingArea::new();
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_can_focus(true);
        area.set_focusable(true);
        window.set_child(Some(&area));

        // draw
        {
            let state = state.clone();
            let renderer = renderer.clone();
            area.set_draw_func(move |_area, cr, width, height| {
                let now = Instant::now();
                {
                    let mut r = renderer.borrow_mut();
                    r.set_size(width, height);
                }
                let st = {
                    let s = state.borrow();
                    s.visual(now)
                };
                renderer.borrow_mut().paint(cr, &st);
            });
        }

        // frame clock
        {
            let state = state.clone();
            let area2 = area.clone();
            let window2 = window.clone();
            area.add_tick_callback(move |_w, _clock| {
                let now = Instant::now();
                let (quit, wants) = {
                    let mut s = state.borrow_mut();
                    s.tick(now);
                    (s.quit_requested, s.needs_frame(now))
                };
                if wants {
                    area2.queue_draw();
                }
                if quit {
                    window2.close();
                }
                glib::ControlFlow::Continue
            });
        }

        // keyboard
        {
            let state = state.clone();
            let area2 = area.clone();
            let controller = gtk::EventControllerKey::new();
            controller.set_propagation_phase(gtk::PropagationPhase::Capture);
            controller.connect_key_pressed(move |_, keyval, _keycode, modifiers| {
                let key = match keyval {
                    gtk::gdk::Key::Left => Some(Key::Left),
                    gtk::gdk::Key::Right => Some(Key::Right),
                    gtk::gdk::Key::Up => Some(Key::Up),
                    gtk::gdk::Key::Down => Some(Key::Down),
                    gtk::gdk::Key::Return | gtk::gdk::Key::KP_Enter => Some(Key::Enter),
                    gtk::gdk::Key::BackSpace => Some(Key::Backspace),
                    gtk::gdk::Key::Delete => Some(Key::Delete),
                    gtk::gdk::Key::F1 => Some(Key::F1),
                    gtk::gdk::Key::F2 => Some(Key::F2),
                    _ => None,
                };
                let key = key.or_else(|| {
                    if modifiers.intersects(
                        gtk::gdk::ModifierType::CONTROL_MASK
                            | gtk::gdk::ModifierType::ALT_MASK
                            | gtk::gdk::ModifierType::SUPER_MASK,
                    ) {
                        if keyval == gtk::gdk::Key::u {
                            Some(Key::Clear)
                        } else {
                            None
                        }
                    } else {
                        keyval
                            .to_unicode()
                            .filter(|c| !c.is_control())
                            .map(Key::Char)
                    }
                });
                if let Some(key) = key {
                    let now = Instant::now();
                    state.borrow_mut().handle_key(key, now);
                    area2.queue_draw();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            window.add_controller(controller);
        }

        window.present();
        area.grab_focus();
        if !windowed {
            window.fullscreen();
        }
    });

    app.run_with_args(&["phigrauth"])
}

fn render_snapshots(dir: &str, config: &Config, user: Option<&str>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let (w, h) = (1920, 1080);
    let mut renderer = Renderer::new(w, h);
    let now = Instant::now();
    let user = user
        .map(|s| s.to_string())
        .unwrap_or_else(|| sessions::current_user().name);

    let mut shots: Vec<(String, VisualState)> = Vec::new();

    // boot timeline
    for (name, t) in [
        ("boot_000", 0.0f32),
        ("boot_050", 0.50),
        ("boot_070", 0.70),
        ("boot_072", 0.72),
        ("boot_080", 0.80),
        ("boot_100", 1.00),
        ("boot_140", 1.40),
        ("boot_170", 1.70),
        ("boot_172", 1.72),
        ("boot_190", 1.90),
        ("boot_202", 2.02),
        ("boot_220", 2.20),
    ] {
        let mut a = App::demo(config.clone(), Phase::Boot, now);
        a.user = user.clone();
        a.set_boot_start(now - Duration::from_secs_f32(t));
        a.tick(now);
        shots.push((name.into(), a.visual(now)));
    }

    let mut base = App::demo(config.clone(), Phase::Login, now);
    base.user = user.clone();
    base.password = "123".into();
    base.settle(Palette::Red, TitleKind::Denied, SubtitleKind::Denied);
    base.focus = Focus::Password;
    shots.push(("login_password_focus".into(), base.visual(now)));

    let mut a = base_copy(&base, now, config, &user);
    a.focus = Focus::User;
    shots.push(("login_user_focus".into(), a.visual(now)));

    let mut a = base_copy(&base, now, config, &user);
    a.focus = Focus::Env;
    shots.push(("login_env_focus".into(), a.visual(now)));

    let mut a = base_copy(&base, now, config, &user);
    a.settle(Palette::Red, TitleKind::Denied, SubtitleKind::Retry);
    a.password.clear();
    shots.push(("retry".into(), a.visual(now)));

    // Desktop environment list includes the synthetic tty shell entry.
    let mut a = base_copy(&base, now, config, &user);
    a.focus = Focus::Env;
    a.session_idx = a.sessions.len().saturating_sub(1);
    shots.push(("env_tty_shell".into(), a.visual(now)));

    // Reference comparison state: no focused bar and no footer hints.
    let mut a = base_copy(&base, now, config, &user);
    a.phase = Phase::Verifying;
    a.settle(Palette::Red, TitleKind::Denied, SubtitleKind::Denied);
    let mut st = a.visual(now);
    st.footer_alpha = 0.0;
    shots.push(("denied_unfocused".into(), st));

    // Pure background, for compositing the reference SVGs over it.
    let mut st = base.visual(now);
    st.title_alpha = 0.0;
    st.arrows_alpha = 0.0;
    st.subtitle_alpha = 0.0;
    st.bars_alpha = 0.0;
    st.footer_alpha = 0.0;
    st.red_flash = 0.0;
    st.bg_dim = 0.0;
    st.bg_alpha = 1.0;
    shots.push(("background_only".into(), st));

    let mut a = base_copy(&base, now, config, &user);
    a.phase = Phase::Verifying;
    a.settle(
        Palette::Yellow,
        TitleKind::Verifying,
        SubtitleKind::Verifying,
    );
    shots.push(("verifying".into(), a.visual(now)));

    let mut a = base_copy(&base, now, config, &user);
    a.phase = Phase::Granted;
    a.settle(Palette::Green, TitleKind::Granted, SubtitleKind::Granted);
    shots.push(("granted".into(), a.visual(now)));

    let mut a = base_copy(&base, now, config, &user);
    a.settle(Palette::Red, TitleKind::Denied, SubtitleKind::Retry);
    a.password.clear();
    a.set_flash_start(Some(now - Duration::from_secs_f32(0.03)));
    shots.push(("failed_flash".into(), a.visual(now)));

    let mut a = base_copy(&base, now, config, &user);
    a.phase = Phase::Exiting;
    a.settle(Palette::Green, TitleKind::Granted, SubtitleKind::Granted);
    a.set_exit_start(Some(now - Duration::from_secs_f32(0.5)));
    shots.push(("exit_mid".into(), a.visual(now)));

    let mut a = base_copy(&base, now, config, &user);
    a.focus = Focus::Env;
    if let Some(ink) = scene::text_ink(a.env_name()) {
        a.wipe = Some(app::WipeAnim {
            start: now - Duration::from_secs_f32(0.18),
            rect: scene::Rect {
                x: scene::ENV_TEXT_X + (ink.x - 60.0),
                y: scene::ENV_TEXT_Y + (ink.y - 20.0),
                w: ink.w,
                h: ink.h,
            },
        });
    }
    shots.push(("env_wipe".into(), a.visual(now)));

    for (name, st) in shots {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h)
            .map_err(std::io::Error::other)?;
        let cr = cairo::Context::new(&surface).map_err(std::io::Error::other)?;
        renderer.paint(&cr, &st);
        let path = std::path::Path::new(dir).join(format!("{name}.png"));
        let mut file = std::fs::File::create(&path)?;
        surface
            .write_to_png(&mut file)
            .map_err(std::io::Error::other)?;
        println!("{}", path.display());
    }
    Ok(())
}

fn base_copy(base: &App, now: Instant, config: &Config, user: &str) -> App {
    let mut a = App::demo(config.clone(), base.phase, now);
    a.user = user.to_string();
    a.password = base.password.clone();
    a.focus = base.focus;
    a.session_idx = base.session_idx;
    a.settle(Palette::Red, TitleKind::Denied, SubtitleKind::Denied);
    a
}
