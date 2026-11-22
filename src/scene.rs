//! Layered renderer for the PhigrAuth greeter.
//!
//! The reference SVGs are rendered as independent Cairo surfaces (via resvg)
//! and composited with the animations described in 参考/要求.md.  Positions are
//! expressed in the reference viewBox (508 x 285.75) and scaled to the output.

use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::Arc;

use gtk::cairo;

pub const VB_W: f64 = 507.99994;
pub const VB_H: f64 = 285.74997;

pub const ARROW_SPREAD: f32 = 159.8;
pub const BOOT_ELEM_DROP: f32 = 22.036;
pub const BOOT_BARS_DROP: f32 = 116.947;

pub const TITLE_TEXT_X: f32 = 254.0;
pub const ENV_TEXT_X: f32 = 339.30;
pub const ENV_TEXT_Y: f32 = 227.75;
pub const ENV_FONT_SIZE: f32 = 8.7233;

const ASSETS: &[(&str, &str)] = &[
    ("arrows_left", include_str!("../assets/arrows_left.svg")),
    ("arrows_right", include_str!("../assets/arrows_right.svg")),
    ("connector", include_str!("../assets/connector.svg")),
    ("bar0", include_str!("../assets/bar0.svg")),
    ("bar1", include_str!("../assets/bar1.svg")),
    ("bar2", include_str!("../assets/bar2.svg")),
    ("bar0_focused", include_str!("../assets/bar0_focused.svg")),
    ("bar1_focused", include_str!("../assets/bar1_focused.svg")),
    ("bar2_focused", include_str!("../assets/bar2_focused.svg")),
    ("title_denied", include_str!("../assets/title_denied.svg")),
    (
        "title_verifying",
        include_str!("../assets/title_verifying.svg"),
    ),
    ("title_granted", include_str!("../assets/title_granted.svg")),
    (
        "subtitle_denied",
        include_str!("../assets/subtitle_denied.svg"),
    ),
    (
        "subtitle_retry",
        include_str!("../assets/subtitle_retry.svg"),
    ),
    (
        "subtitle_verifying",
        include_str!("../assets/subtitle_verifying.svg"),
    ),
    (
        "subtitle_granted",
        include_str!("../assets/subtitle_granted.svg"),
    ),
];

fn asset(name: &str) -> &'static str {
    ASSETS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| *s)
        .unwrap_or_else(|| panic!("missing asset {name}"))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Palette {
    Red,
    Yellow,
    Green,
}

impl Palette {
    pub fn colors(self) -> PaletteColors {
        match self {
            Palette::Red => PaletteColors {
                grad_a: [0xd6, 0x0a, 0x00],
                grad_b: [0xd4, 0x0a, 0x00],
                fill: [0xd4, 0x0b, 0x00],
                stroke: [0xdb, 0x0b, 0x00],
                line: [0xd6, 0x0c, 0x00],
                accent: [0xd6, 0x0b, 0x00],
            },
            Palette::Yellow => PaletteColors {
                grad_a: [0xf3, 0xdd, 0x44],
                grad_b: [0xf3, 0xdb, 0x44],
                fill: [0xf3, 0xdc, 0x44],
                stroke: [0xf3, 0xdc, 0x44],
                line: [0xf3, 0xdc, 0x44],
                accent: [0xf3, 0xdc, 0x44],
            },
            Palette::Green => PaletteColors {
                grad_a: [0x67, 0xf2, 0x3a],
                grad_b: [0x67, 0xf2, 0x3a],
                fill: [0x67, 0xf2, 0x3a],
                stroke: [0x67, 0xf2, 0x3a],
                line: [0x67, 0xf2, 0x3a],
                accent: [0x67, 0xf2, 0x3a],
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PaletteColors {
    pub grad_a: [u8; 3],
    pub grad_b: [u8; 3],
    pub fill: [u8; 3],
    pub stroke: [u8; 3],
    pub line: [u8; 3],
    pub accent: [u8; 3],
}

impl PaletteColors {
    pub fn mix(self, other: PaletteColors, t: f32) -> PaletteColors {
        let t = t.clamp(0.0, 1.0);
        if t <= 0.0 {
            return self;
        }
        if t >= 1.0 {
            return other;
        }
        // Palette colors are embedded in the cache key as hex strings.  During
        // the 0.18s crossfade, unquantized channels would create a new key for
        // every frame and re-rasterize the whole layer.  Quantizing only the
        // intermediate values to 8 levels per channel keeps the cache useful;
        // the exact endpoint palettes are returned above.
        let quantize = |v: u8| (v / 32) * 32;
        let f = |a: [u8; 3], b: [u8; 3]| {
            let mix = |x: u8, y: u8| quantize((x as f32 + (y as f32 - x as f32) * t).round() as u8);
            [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
        };
        PaletteColors {
            grad_a: f(self.grad_a, other.grad_a),
            grad_b: f(self.grad_b, other.grad_b),
            fill: f(self.fill, other.fill),
            stroke: f(self.stroke, other.stroke),
            line: f(self.line, other.line),
            accent: f(self.accent, other.accent),
        }
    }
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TitleKind {
    Denied,
    Verifying,
    Granted,
}

impl TitleKind {
    fn asset(self) -> &'static str {
        match self {
            TitleKind::Denied => "title_denied",
            TitleKind::Verifying => "title_verifying",
            TitleKind::Granted => "title_granted",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SubtitleKind {
    Denied,
    Retry,
    Verifying,
    Granted,
}

impl SubtitleKind {
    fn asset(self) -> &'static str {
        match self {
            SubtitleKind::Denied => "subtitle_denied",
            SubtitleKind::Retry => "subtitle_retry",
            SubtitleKind::Verifying => "subtitle_verifying",
            SubtitleKind::Granted => "subtitle_granted",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BarVis {
    pub text: String,
    pub focused: bool,
}

#[derive(Clone, Debug)]
pub struct Wipe {
    /// 0 = solid block, 1 = fully revealed.
    pub reveal: f32,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Clone, Debug)]
pub struct VisualState {
    pub palette: PaletteColors,

    pub title: TitleKind,
    pub title_prev: Option<TitleKind>,
    pub title_mix: f32,
    pub subtitle: SubtitleKind,
    pub subtitle_prev: Option<SubtitleKind>,
    pub subtitle_mix: f32,

    pub bars: [BarVis; 3],
    pub elem_y: f32,
    pub bars_y: f32,
    pub rise: f32,

    pub title_scale: f32,
    pub title_alpha: f32,
    pub arrows_alpha: f32,
    pub arrows_spread: f32,
    pub subtitle_alpha: f32,
    pub bars_alpha: f32,

    pub footer_alpha: f32,
    pub footer_env_hint: bool,

    pub red_flash: f32,
    pub bg_alpha: f32,
    pub bg_dim: f32,
    pub wipe: Option<Wipe>,
    pub toast: Option<String>,
}

impl Default for VisualState {
    fn default() -> Self {
        let palette = Palette::Red.colors();
        VisualState {
            palette,
            title: TitleKind::Denied,
            title_prev: None,
            title_mix: 1.0,
            subtitle: SubtitleKind::Denied,
            subtitle_prev: None,
            subtitle_mix: 1.0,
            bars: [
                BarVis {
                    text: "user: xlwvx".into(),
                    focused: false,
                },
                BarVis {
                    text: "password".into(),
                    focused: false,
                },
                BarVis {
                    text: "env: niri".into(),
                    focused: false,
                },
            ],
            elem_y: 0.0,
            bars_y: 0.0,
            rise: 0.0,
            title_scale: 1.0,
            title_alpha: 1.0,
            arrows_alpha: 1.0,
            arrows_spread: 0.0,
            subtitle_alpha: 1.0,
            bars_alpha: 1.0,
            footer_alpha: 1.0,
            footer_env_hint: false,
            red_flash: 0.0,
            bg_alpha: 1.0,
            bg_dim: 0.0,
            wipe: None,
            toast: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

const fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

const ARROW_CROP: Rect = rect(0.0, 40.0, 508.0, 165.0);
const TITLE_CROP: Rect = rect(60.0, 62.0, 398.0, 110.0);
const SUBTITLE_CROP: Rect = rect(178.0, 126.0, 160.0, 40.0);
const CONNECTOR_CROP: Rect = rect(0.0, 218.0, 508.0, 14.0);
const BAR_CROP: [Rect; 3] = [
    rect(110.0, 196.0, 112.0, 58.0),
    rect(181.0, 196.0, 145.0, 58.0),
    rect(285.0, 196.0, 112.0, 58.0),
];
const FOOTER_CROP: Rect = rect(0.0, 269.0, 508.0, 16.75);
const TOAST_CROP: Rect = rect(0.0, 256.0, 508.0, 21.0);

#[derive(Clone, PartialEq, Eq, Hash)]
enum Key {
    ArrowsLeft(String, String),
    ArrowsRight(String, String),
    Connector(String),
    Bar {
        index: u8,
        fill: String,
        stroke: String,
        text: String,
        focused: bool,
    },
    Title(TitleKind),
    Subtitle(SubtitleKind),
    Footer {
        accent: String,
        env_hint: bool,
    },
    Toast(String),
}

#[derive(Clone)]
struct Layer {
    surface: cairo::ImageSurface,
    crop: Rect,
    ink: Option<Rect>,
}

pub struct Renderer {
    pub width: i32,
    pub height: i32,
    scale: f64,
    offset_x: f64,
    offset_y: f64,
    background: Option<cairo::ImageSurface>,
    cache: HashMap<Key, Layer>,
    fontdb: Arc<fontdb::Database>,
}

impl Renderer {
    pub fn new(width: i32, height: i32) -> Self {
        let mut r = Renderer {
            width: 0,
            height: 0,
            scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            background: None,
            cache: HashMap::new(),
            fontdb: crate::fonts::database(),
        };
        r.set_size(width, height);
        r
    }

    pub fn set_size(&mut self, width: i32, height: i32) {
        if width == self.width && height == self.height {
            return;
        }
        self.width = width.max(1);
        self.height = height.max(1);
        self.scale = (self.width as f64 / VB_W).min(self.height as f64 / VB_H);
        self.offset_x = (self.width as f64 - VB_W * self.scale) / 2.0;
        self.offset_y = (self.height as f64 - VB_H * self.scale) / 2.0;
        self.cache.clear();
        self.background = None;
    }

    pub fn paint(&mut self, cr: &cairo::Context, st: &VisualState) {
        if self.width == 0 || self.height == 0 {
            return;
        }
        if self.background.is_none() {
            self.background = Some(self.render_background());
        }

        cr.set_operator(cairo::Operator::Source);
        cr.set_source_rgb(0.0, 0.0, 0.0);
        cr.paint().ok();
        cr.set_operator(cairo::Operator::Over);

        if let Some(bg) = &self.background {
            if st.bg_alpha > 0.001 {
                cr.set_source_surface(bg, 0.0, 0.0).ok();
                cr.paint_with_alpha(st.bg_alpha as f64).ok();
            }
        }
        // On a successful login only the background fades to black; the
        // elements float away above it at full brightness.
        if st.bg_dim > 0.001 {
            cr.set_source_rgba(0.0, 0.0, 0.0, st.bg_dim as f64);
            cr.paint().ok();
        }

        let s = self.scale;
        let ox = self.offset_x;
        let oy = self.offset_y;

        // --- arrows -------------------------------------------------------
        let spread = st.arrows_spread as f64;
        let arrow_dy = (st.elem_y - st.rise) as f64;
        if st.arrows_alpha > 0.001 {
            let key = Key::ArrowsLeft(hex(st.palette.grad_a), hex(st.palette.grad_b));
            if let Some(layer) = self.layer_for(&key) {
                Self::paint_layer(cr, &layer, -spread, arrow_dy, st.arrows_alpha, ox, oy, s);
            }
            let key = Key::ArrowsRight(hex(st.palette.grad_a), hex(st.palette.grad_b));
            if let Some(layer) = self.layer_for(&key) {
                Self::paint_layer(cr, &layer, spread, arrow_dy, st.arrows_alpha, ox, oy, s);
            }
        }

        // --- title --------------------------------------------------------
        let title_dy = (st.elem_y - st.rise) as f64;
        if st.title_alpha > 0.001 {
            if let Some(prev) = st.title_prev {
                if st.title_mix < 0.999 {
                    self.paint_title(
                        cr,
                        prev,
                        title_dy,
                        st.title_scale,
                        st.title_alpha * (1.0 - st.title_mix),
                        ox,
                        oy,
                        s,
                    );
                }
            }
            self.paint_title(
                cr,
                st.title,
                title_dy,
                st.title_scale,
                st.title_alpha * st.title_mix,
                ox,
                oy,
                s,
            );
        }

        // --- subtitle -----------------------------------------------------
        let subtitle_dy = (st.elem_y - st.rise) as f64;
        if st.subtitle_alpha > 0.001 {
            if let Some(prev) = st.subtitle_prev {
                if st.subtitle_mix < 0.999 {
                    if let Some(layer) = self.layer_for(&Key::Subtitle(prev)) {
                        Self::paint_layer(
                            cr,
                            &layer,
                            0.0,
                            subtitle_dy,
                            st.subtitle_alpha * (1.0 - st.subtitle_mix),
                            ox,
                            oy,
                            s,
                        );
                    }
                }
            }
            if let Some(layer) = self.layer_for(&Key::Subtitle(st.subtitle)) {
                Self::paint_layer(
                    cr,
                    &layer,
                    0.0,
                    subtitle_dy,
                    st.subtitle_alpha * st.subtitle_mix,
                    ox,
                    oy,
                    s,
                );
            }
        }

        // --- input bars ---------------------------------------------------
        let bars_dy = (st.bars_y - st.rise) as f64;
        if let Some(line) = self.layer_for(&Key::Connector(hex(st.palette.line))) {
            Self::paint_layer(cr, &line, 0.0, bars_dy, st.bars_alpha, ox, oy, s);
        }
        for i in 0..3 {
            let focused = st.bars[i].focused;
            let fill = hex(st.palette.fill);
            let stroke = hex(st.palette.stroke);
            let key = Key::Bar {
                index: i as u8,
                fill,
                stroke,
                text: st.bars[i].text.clone(),
                focused: false,
            };
            if let Some(layer) = self.layer_for(&key) {
                Self::paint_layer(cr, &layer, 0.0, bars_dy, st.bars_alpha, ox, oy, s);
            }
            if focused {
                let key = Key::Bar {
                    index: i as u8,
                    fill: String::new(),
                    stroke: String::new(),
                    text: st.bars[i].text.clone(),
                    focused: true,
                };
                if let Some(layer) = self.layer_for(&key) {
                    Self::paint_layer(cr, &layer, 0.0, bars_dy, st.bars_alpha, ox, oy, s);
                }
            }
        }
        if let Some(w) = &st.wipe {
            if w.reveal < 0.999 && w.h > 0.5 {
                let reveal = w.reveal.clamp(0.0, 1.0) as f64;
                // Collapse from the top down to the text's bottom edge.
                let height = w.h as f64 * (1.0 - reveal);
                cr.set_source_rgb(
                    st.palette.accent[0] as f64 / 255.0,
                    st.palette.accent[1] as f64 / 255.0,
                    st.palette.accent[2] as f64 / 255.0,
                );
                cr.rectangle(
                    ox + w.x as f64 * s,
                    oy + (w.y as f64 + w.h as f64 - height) * s,
                    w.w as f64 * s,
                    height * s,
                );
                cr.fill().ok();
            }
        }

        // --- footer -------------------------------------------------------
        if st.footer_alpha > 0.001 {
            let key = Key::Footer {
                accent: hex(st.palette.accent),
                env_hint: st.footer_env_hint,
            };
            if let Some(layer) = self.layer_for(&key) {
                Self::paint_layer(cr, &layer, 0.0, -st.rise as f64, st.footer_alpha, ox, oy, s);
            }
            if let Some(toast) = &st.toast {
                let key = Key::Toast(toast.clone());
                if let Some(layer) = self.layer_for(&key) {
                    Self::paint_layer(cr, &layer, 0.0, -st.rise as f64, st.footer_alpha, ox, oy, s);
                }
            }
        }

        // --- full screen overlays ----------------------------------------
        if st.red_flash > 0.001 {
            cr.set_source_rgba(
                0x55 as f64 / 255.0,
                0x08 as f64 / 255.0,
                0x08 as f64 / 255.0,
                st.red_flash as f64,
            );
            cr.paint().ok();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_title(
        &mut self,
        cr: &cairo::Context,
        kind: TitleKind,
        dy: f64,
        scale: f32,
        alpha: f32,
        ox: f64,
        oy: f64,
        s: f64,
    ) {
        let Some(layer) = self.layer_for(&Key::Title(kind)) else {
            return;
        };
        let (px, py) = layer
            .ink
            .map(|i| (i.x + i.w / 2.0, i.y + i.h / 2.0))
            .unwrap_or((TITLE_TEXT_X, 120.0));
        let sc = scale.clamp(0.01, 4.0) as f64;
        let dev_x = ox + px as f64 * s;
        let dev_y = oy + (py as f64 + dy) * s;
        cr.save().ok();
        cr.translate(dev_x, dev_y);
        cr.scale(sc, sc);
        cr.translate(-dev_x, -dev_y);
        Self::paint_layer(cr, &layer, 0.0, dy, alpha, ox, oy, s);
        cr.restore().ok();
    }

    fn paint_layer(
        cr: &cairo::Context,
        layer: &Layer,
        dx: f64,
        dy: f64,
        alpha: f32,
        ox: f64,
        oy: f64,
        s: f64,
    ) {
        if alpha <= 0.001 {
            return;
        }
        let x = ox + (layer.crop.x as f64 + dx) * s;
        let y = oy + (layer.crop.y as f64 + dy) * s;
        cr.set_source_surface(&layer.surface, x, y).ok();
        cr.paint_with_alpha(alpha.clamp(0.0, 1.0) as f64).ok();
    }

    fn layer_for(&mut self, key: &Key) -> Option<Layer> {
        if !self.cache.contains_key(key) {
            if self.cache.len() > 260 {
                self.cache.clear();
            }
            if let Some(layer) = self.build_layer(key) {
                self.cache.insert(key.clone(), layer);
            }
        }
        self.cache.get(key).cloned()
    }

    fn build_layer(&mut self, key: &Key) -> Option<Layer> {
        let k = self.scale;
        let mut opt = usvg::Options::default();
        opt.fontdb = self.fontdb.clone();
        let (_name, crop, svg) = match key {
            Key::ArrowsLeft(a, b) => (
                "arrows_left",
                ARROW_CROP,
                asset("arrows_left")
                    .replace("__GRAD_A__", a)
                    .replace("__GRAD_B__", b),
            ),
            Key::ArrowsRight(a, b) => (
                "arrows_right",
                ARROW_CROP,
                asset("arrows_right")
                    .replace("__GRAD_A__", a)
                    .replace("__GRAD_B__", b),
            ),
            Key::Connector(c) => (
                "connector",
                CONNECTOR_CROP,
                asset("connector").replace("__LINE__", c),
            ),
            Key::Bar {
                index,
                fill,
                stroke,
                text,
                focused,
            } => {
                let idx = *index as usize;
                let name = if *focused {
                    match idx {
                        0 => "bar0_focused",
                        1 => "bar1_focused",
                        _ => "bar2_focused",
                    }
                } else {
                    match idx {
                        0 => "bar0",
                        1 => "bar1",
                        _ => "bar2",
                    }
                };
                let mut svg = asset(name).replace("__TEXT__", &svg_runs(text));
                if !*focused {
                    svg = svg.replace("__FILL__", fill).replace("__STROKE__", stroke);
                }
                // Squeeze very long values so they stay inside the bar.
                let max_width = [58.0f32, 84.0, 62.0][idx];
                if let Some(ink) = text_ink(text) {
                    if ink.w > max_width {
                        let attr = format!(
                            "<text textLength=\"{max_width:.2}\" lengthAdjust=\"spacingAndGlyphs\""
                        );
                        svg = svg.replacen("<text", &attr, 1);
                    }
                }
                (name, BAR_CROP[idx], svg)
            }
            Key::Title(kind) => ("title", TITLE_CROP, asset(kind.asset()).to_string()),
            Key::Subtitle(kind) => ("subtitle", SUBTITLE_CROP, asset(kind.asset()).to_string()),
            Key::Footer { accent, env_hint } => {
                ("footer", FOOTER_CROP, footer_svg(accent, *env_hint))
            }
            Key::Toast(text) => ("toast", TOAST_CROP, toast_svg(text)),
        };

        let tree = match usvg::Tree::from_data(svg.as_bytes(), &opt) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("phigrauth: cannot parse layer {_name}: {e}");
                return None;
            }
        };
        let w = (crop.w as f64 * k).round().max(1.0) as u32;
        let h = (crop.h as f64 * k).round().max(1.0) as u32;
        let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
        // The SVG documents are authored at 1920x1080 (3.7795 tree pixels per
        // viewBox unit).  `k` is output pixels per viewBox unit, so the resvg
        // scale is k / 3.7795.
        let tree_scale = (k * (VB_W / 1920.0)) as f32;
        let transform = tiny_skia::Transform::from_row(
            tree_scale,
            0.0,
            0.0,
            tree_scale,
            -(crop.x as f64 * k) as f32,
            -(crop.y as f64 * k) as f32,
        );
        resvg::render(&tree, transform, &mut pixmap.as_mut());
        if std::env::var_os("PHIGRAUTH_DUMP_LAYERS").is_some() {
            let _ = std::fs::create_dir_all(".work/layers");
            let _ = pixmap.save_png(format!(
                ".work/layers/{_name}_{}_{}.png",
                crop.x as i32, crop.y as i32
            ));
            eprintln!("layer {_name} {}x{} crop {:?}", w, h, crop);
        }

        let mut ink: Option<Rect> = None;
        {
            let data = pixmap.data();
            let mut x0 = w;
            let mut y0 = h;
            let mut x1 = 0u32;
            let mut y1 = 0u32;
            for y in 0..h {
                for x in 0..w {
                    if data[((y * w + x) * 4 + 3) as usize] > 2 {
                        x0 = x0.min(x);
                        y0 = y0.min(y);
                        x1 = x1.max(x + 1);
                        y1 = y1.max(y + 1);
                    }
                }
            }
            if x1 > x0 && y1 > y0 {
                ink = Some(Rect {
                    x: crop.x + x0 as f32 / k as f32,
                    y: crop.y + y0 as f32 / k as f32,
                    w: (x1 - x0) as f32 / k as f32,
                    h: (y1 - y0) as f32 / k as f32,
                });
            }
        }

        // tiny-skia stores premultiplied RGBA; Cairo ARGB32 expects BGRA in
        // little-endian memory.  Swap the red and blue channels.
        let data = pixmap.data();
        let mut bgra = Vec::with_capacity(data.len());
        for px in data.chunks_exact(4) {
            bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
        }
        let stride = (w * 4) as i32;
        let surface = cairo::ImageSurface::create_for_data(
            bgra,
            cairo::Format::ARgb32,
            w as i32,
            h as i32,
            stride,
        )
        .ok()?;
        surface.mark_dirty();
        Some(Layer { surface, crop, ink })
    }

    fn render_background(&mut self) -> cairo::ImageSurface {
        let w = self.width;
        let h = self.height;
        let surf =
            cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).expect("background surface");
        let cr = cairo::Context::new(&surf).expect("background context");
        cr.set_source_rgb(
            0x0c as f64 / 255.0,
            0x0c as f64 / 255.0,
            0x0c as f64 / 255.0,
        );
        cr.paint().ok();

        cr.set_source_rgb(
            0x1c as f64 / 255.0,
            0x1c as f64 / 255.0,
            0x1c as f64 / 255.0,
        );
        cr.set_line_width(h as f64 / 400.0);
        cr.set_line_cap(cairo::LineCap::Round);
        cr.set_line_join(cairo::LineJoin::Round);

        let s = 3.44f64;
        let k = 0.00112f64;
        let m = 7i32;
        let oy = s / 3f64.sqrt();
        let scale = h as f64 / 18.0;
        let cx = w as f64 / 2.0;
        let cy = h as f64 / 2.0;
        for angle in [PI / 2.0, PI / 6.0, 5.0 * PI / 6.0] {
            let ca = angle.cos();
            let sa = angle.sin();
            for n in -m..=m {
                let off = n as f64 * s;
                cr.new_path();
                let mut t = -50.0f64;
                let mut first = true;
                while t <= 50.0 {
                    let bx = t * ca - off * sa;
                    let by = t * sa + off * ca;
                    let r2 = bx * bx + (by - oy) * (by - oy);
                    let distort = 1.0 + k * r2;
                    let x = cx + bx * distort * scale;
                    let y = cy - (by - oy) * distort * scale;
                    if first {
                        cr.move_to(x, y);
                        first = false;
                    } else {
                        cr.line_to(x, y);
                    }
                    t += 0.4;
                }
                cr.stroke().ok();
            }
        }
        surf.mark_dirty();
        surf
    }
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// CJK ranges plus common CJK punctuation.  Chinese runs use Source Sans Pro
/// Normal; everything else uses Saira Medium.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF
        | 0x20000..=0x2FA1F)
        || matches!(c, '…' | '“' | '”' | '‘' | '’' | '·' | '—' | '–')
}

fn split_runs(text: &str) -> Vec<(bool, &str)> {
    let mut out: Vec<(bool, &str)> = Vec::new();
    for (idx, c) in text.char_indices() {
        let cjk = is_cjk(c);
        match out.last_mut() {
            Some((kind, run)) if *kind == cjk => {
                let start = run.as_ptr() as usize - text.as_ptr() as usize;
                let end = idx + c.len_utf8();
                *run = &text[start..end];
            }
            _ => out.push((cjk, &text[idx..idx + c.len_utf8()])),
        }
    }
    out
}

fn push_runs(out: &mut String, text: &str, fill: Option<&str>) {
    for (cjk, run) in split_runs(text) {
        if run.is_empty() {
            continue;
        }
        let (family, weight) = if cjk {
            ("Source Sans Pro", "normal")
        } else {
            ("Saira", "500")
        };
        match fill {
            Some(fill) => out.push_str(&format!(
                r#"<tspan font-family="{family}" font-weight="{weight}" fill="{fill}">{}</tspan>"#,
                xml_escape(run)
            )),
            None => out.push_str(&format!(
                r#"<tspan font-family="{family}" font-weight="{weight}">{}</tspan>"#,
                xml_escape(run)
            )),
        }
    }
}

/// SVG runs for runtime text injected into the input bars.
pub fn svg_runs(text: &str) -> String {
    let mut out = String::new();
    push_runs(&mut out, text, None);
    out
}

fn footer_svg(accent: &str, env_hint: bool) -> String {
    let gray = "#8d8d8d";
    let mut spans = String::new();
    push_runs(&mut spans, "←→", Some(accent));
    push_runs(&mut spans, " 切换焦点     ", Some(gray));
    push_runs(&mut spans, "F1", Some(accent));
    push_runs(&mut spans, " 关机     ", Some(gray));
    push_runs(&mut spans, "F2", Some(accent));
    push_runs(&mut spans, " 重启", Some(gray));
    if env_hint {
        push_runs(&mut spans, "     ", Some(gray));
        push_runs(&mut spans, "↑↓", Some(accent));
        push_runs(&mut spans, " 切换桌面环境", Some(gray));
    }
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080" viewBox="0 0 507.99994 285.74997"><text xml:space="preserve" x="254" y="283.6" text-anchor="middle" font-family="Saira" font-weight="500" font-size="7.4">{spans}</text></svg>"##
    )
}

fn toast_svg(text: &str) -> String {
    let mut spans = String::new();
    push_runs(&mut spans, text, Some("#d60b00"));
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1920" height="1080" viewBox="0 0 507.99994 285.74997"><text xml:space="preserve" x="254" y="273.5" text-anchor="middle" font-family="Saira" font-weight="500" font-size="7.8">{spans}</text></svg>"##
    )
}

/// Measure the ink box of a short bar-text string in the input-bar text
/// coordinate system (anchor at 60,20).  Used by the desktop-environment wipe.
pub fn text_ink(text: &str) -> Option<Rect> {
    let db = crate::fonts::database();

    let svg = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="30" viewBox="0 0 120 30"><text xml:space="preserve" x="60" y="20" text-anchor="middle" font-size="{}">{}</text></svg>"##,
        ENV_FONT_SIZE,
        svg_runs(text)
    );
    let mut opt = usvg::Options::default();
    opt.fontdb = db.clone();
    let tree = usvg::Tree::from_data(svg.as_bytes(), &opt).ok()?;
    let k = 4.0;
    let w = 480u32;
    let h = 120u32;
    let mut pixmap = tiny_skia::Pixmap::new(w, h)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(k, k),
        &mut pixmap.as_mut(),
    );
    let data = pixmap.data();
    let mut x0 = w;
    let mut y0 = h;
    let mut x1 = 0u32;
    let mut y1 = 0u32;
    for y in 0..h {
        for x in 0..w {
            if data[((y * w + x) * 4 + 3) as usize] > 2 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 {
        return None;
    }
    Some(Rect {
        x: x0 as f32 / k,
        y: y0 as f32 / k,
        w: (x1 - x0) as f32 / k,
        h: (y1 - y0) as f32 / k,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_mix_endpoints_are_exact() {
        let a = Palette::Red.colors();
        let b = Palette::Green.colors();
        assert_eq!(a.mix(b, 0.0).fill, a.fill);
        assert_eq!(a.mix(b, 1.0).fill, b.fill);
    }

    #[test]
    fn palette_mix_intermediate_is_quantized() {
        let a = Palette::Red.colors();
        let b = Palette::Green.colors();
        let mid = a.mix(b, 0.5);
        for c in [mid.grad_a, mid.fill, mid.stroke, mid.line, mid.accent] {
            assert!(c.iter().all(|v| v % 32 == 0));
        }
    }
}
