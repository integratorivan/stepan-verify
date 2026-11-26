//! stepan_verify design language (prototype: PostHog warmth + Evil Martians type +
//! reui agent-activity cards). The gpui theme (assets/theme.json) covers the
//! standard tokens; this module adds the ones it has no slot for and the small
//! building blocks every screen shares.

use gpui_kit::component::{ActiveTheme as _, Theme, ThemeRegistry, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use stepan_verify_core::Provider;

/// Install the stepan_verify light/dark themes and follow the system appearance.
pub fn init(cx: &mut App) {
    let registry = ThemeRegistry::global_mut(cx);
    if let Err(err) = registry.load_themes_from_str(include_str!("../assets/theme.json")) {
        eprintln!("stepan_verify: theme: {err:#}");
        return;
    }
    let themes = registry.themes();
    let (Some(light), Some(dark)) = (themes.get("stepan_verify Light").cloned(), themes.get("stepan_verify Dark").cloned()) else { return };
    Theme::update(cx, |t| {
        t.light_theme = light;
        t.dark_theme = dark;
    });
    let appearance = cx.window_appearance();
    Theme::change(appearance, None, cx);
}

/// The embedded text face, always available.
pub const DEFAULT_FONT: &str = "Instrument Sans";
/// Monospace faces every Mac has, tried when the chosen one is missing.
const MONO_FALLBACKS: [&str; 2] = ["SF Mono", "Menlo"];

/// Font families on this machine plus the embedded ones, sorted, no hidden `.`-faces.
pub fn font_families(cx: &App) -> Vec<String> {
    let mut all = cx.text_system().all_font_names();
    all.push(DEFAULT_FONT.to_owned());
    all.retain(|f| !f.starts_with('.'));
    all.sort_by_key(|f| f.to_lowercase());
    all.dedup();
    all
}

/// Put the fonts from settings into both themes (so light/dark switches keep
/// them) and into the live theme. GPUI panics on a family it cannot find, so a
/// missing choice falls back instead of reaching the text system.
pub fn apply_fonts(cx: &mut App) {
    let config = crate::settings::config(cx).appearance.clone();
    let have = font_families(cx);
    let installed = |f: &str| have.iter().any(|h| h == f);
    let text: SharedString = if installed(&config.font) { config.font.clone() } else { DEFAULT_FONT.to_owned() }.into();
    let mono: SharedString = if installed(&config.mono_font) {
        config.mono_font.clone()
    } else {
        MONO_FALLBACKS.iter().find(|f| installed(f)).map_or("Menlo", |f| *f).to_owned()
    }
    .into();
    Theme::update(cx, |t| {
        for slot in [&mut t.light_theme, &mut t.dark_theme] {
            let mut c = (**slot).clone();
            c.font_family = Some(text.clone());
            c.mono_font_family = Some(mono.clone());
            *slot = std::rc::Rc::new(c);
        }
        t.font_family = text;
        t.mono_font_family = mono;
    });
    cx.refresh_windows();
}

/// A status: solid color, soft background, readable text on the soft background.
#[derive(Clone, Copy)]
pub struct Tone {
    pub solid: Hsla,
    pub soft: Hsla,
    pub text: Hsla,
}

/// Tokens the gpui theme has no slot for.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Sidebar, the frame around session cards, kanban columns.
    pub sunken: Hsla,
    /// Cards and panels.
    pub surface: Hsla,
    /// Hover, the current step, diff headers.
    pub surface2: Hsla,
    pub border: Hsla,
    /// Button outlines and their bottom edge.
    pub border_strong: Hsla,
    /// Headings and the dark button.
    pub ink: Hsla,
    pub fg: Hsla,
    pub muted: Hsla,
    /// Timestamps, paths.
    pub faint: Hsla,
    pub accent: Tone,
    pub accent_edge: Hsla,
    /// Running.
    pub blue: Tone,
    /// Needs your answer, modified.
    pub amber: Tone,
    /// Needs approval, removed.
    pub red: Tone,
    /// Done, added.
    pub green: Tone,
    pub claude: (Hsla, Hsla, Hsla),
    pub codex: (Hsla, Hsla, Hsla),
}

fn hex(v: u32) -> Hsla {
    rgb(v).into()
}

fn tone(solid: u32, soft: u32, text: u32) -> Tone {
    Tone { solid: hex(solid), soft: hex(soft), text: hex(text) }
}

impl Palette {
    pub fn light() -> Self {
        Self {
            sunken: hex(0xE5E7E0),
            surface: hex(0xFDFDF8),
            surface2: hex(0xF5F5EF),
            border: hex(0xD3D4CC),
            border_strong: hex(0xB9BAB1),
            ink: hex(0x151515),
            fg: hex(0x23251D),
            muted: hex(0x65675E),
            faint: hex(0x9A9C92),
            accent: tone(0xF54E00, 0xFDE4D6, 0xB53D06),
            accent_edge: hex(0xB33A00),
            blue: tone(0x1D4AFF, 0xE2E7FF, 0x1238D6),
            amber: tone(0xF1A82C, 0xFFF0CC, 0x8A5800),
            red: tone(0xE0352B, 0xFCE2DE, 0xB3241B),
            green: tone(0x2F9E57, 0xDCF1E2, 0x1E7A40),
            claude: (hex(0xFBE3D3), hex(0xA63A06), hex(0xF0C2A3)),
            codex: (hex(0x23251D), hex(0xEEEFE9), hex(0x23251D)),
        }
    }

    pub fn dark() -> Self {
        Self {
            sunken: hex(0x1E1F23),
            surface: hex(0x25272C),
            surface2: hex(0x2C2E34),
            border: hex(0x36383F),
            border_strong: hex(0x4A4C54),
            ink: hex(0xF4F4EE),
            fg: hex(0xE9E9E2),
            muted: hex(0xA2A49B),
            faint: hex(0x6F7169),
            accent: tone(0xFF6A1F, 0x3B2418, 0xFF9B66),
            accent_edge: hex(0xB84A10),
            blue: tone(0x6E8BFF, 0x1D2547, 0x9DB0FF),
            amber: tone(0xF5B544, 0x3A2E14, 0xF5C46A),
            red: tone(0xFF5A4E, 0x3D1E1C, 0xFF8A80),
            green: tone(0x4CC57A, 0x173323, 0x6FD898),
            claude: (hex(0x3B2418), hex(0xFF9B66), hex(0x5A3420)),
            codex: (hex(0xE9E9E2), hex(0x17181B), hex(0xE9E9E2)),
        }
    }

    pub fn of(theme: &Theme) -> Self {
        if theme.is_dark() { Self::dark() } else { Self::light() }
    }
}

// ------------------------------------------------------------------ type

/// Page title: 26/600 (SF Rounded is heavy enough; size carries the hierarchy).
pub fn page_title(text: impl Into<SharedString>, p: &Palette) -> Div {
    div().text_size(px(26.)).font_weight(FontWeight::SEMIBOLD).line_height(px(32.)).text_color(p.ink).child(text.into())
}

/// Section heading: 15/600, a mono `[n]` counter and a hairline to the edge.
pub fn section_title(text: impl Into<SharedString>, count: Option<String>, p: &Palette, theme: &Theme) -> Div {
    h_flex()
        .gap_2()
        .items_center()
        .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(text.into()))
        .when_some(count, |this, c| this.child(mono(format!("[{c}]"), theme).text_color(p.faint)))
        .child(div().flex_1().h(px(1.)).bg(p.border))
}

/// Monospace text at the detail size.
pub fn mono(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div().font_family(theme.mono_font_family.clone()).text_size(px(11.5)).child(text.into())
}

/// Small uppercase mono label above a value or a block.
pub fn eyebrow(text: impl Into<SharedString>, p: &Palette, theme: &Theme) -> Div {
    let text: SharedString = text.into();
    div()
        .font_family(theme.mono_font_family.clone())
        .text_size(px(10.5))
        .text_color(p.muted)
        .child(SharedString::from(text.to_uppercase()))
}

// ------------------------------------------------------------------ surfaces

/// Soft sunken frame: header and footer sit on it, the content card inside.
pub fn frame(id: impl Into<ElementId>, p: &Palette) -> Stateful<Div> {
    v_flex().id(id).p(px(4.)).rounded(px(12.)).border_1().border_color(p.border).bg(p.sunken)
}

/// Frame tinted by a status (a card that waits for you).
pub fn frame_toned(id: impl Into<ElementId>, p: &Palette, tone: Option<Tone>) -> Stateful<Div> {
    let f = frame(id, p);
    match tone {
        Some(t) => f.border_color(mix(t.solid, p.border, 0.5)),
        None => f,
    }
}

/// The card inside a frame.
pub fn inner(p: &Palette) -> Div {
    v_flex().rounded(px(9.)).border_1().border_color(p.border).bg(p.surface).overflow_hidden().shadow(card_shadow())
}

/// A standalone card (panel, kanban card, metric strip).
pub fn card(p: &Palette) -> Div {
    v_flex().rounded(px(12.)).border_1().border_color(p.border).bg(p.surface).shadow(card_shadow())
}

pub fn card_shadow() -> Vec<BoxShadow> {
    vec![
        BoxShadow { color: hsla(75. / 360., 0.12, 0.13, 0.04), offset: point(px(0.), px(1.)), blur_radius: px(0.), spread_radius: px(0.), inset: false },
        BoxShadow { color: hsla(75. / 360., 0.12, 0.13, 0.06), offset: point(px(0.), px(1.)), blur_radius: px(3.), spread_radius: px(0.), inset: false },
    ]
}

/// PostHog-style chunky edge under a control: a solid 2px shadow of `edge`.
pub fn edge_shadow(edge: Hsla, depth: f32) -> Vec<BoxShadow> {
    vec![BoxShadow { color: edge, offset: point(px(0.), px(depth)), blur_radius: px(0.), spread_radius: px(0.), inset: false }]
}

pub fn popover_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow { color: hsla(75. / 360., 0.12, 0.13, 0.28), offset: point(px(0.), px(12.)), blur_radius: px(32.), spread_radius: px(-8.), inset: false }]
}

/// Linear mix of two colors in RGB space.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    let l = |x: f32, y: f32| x * t + y * (1. - t);
    Rgba { r: l(a.r, b.r), g: l(a.g, b.g), b: l(a.b, b.b), a: 1. }.into()
}

// ------------------------------------------------------------------ marks

/// Soft status badge: "Работает", "Нужен ответ".
pub fn badge(text: impl Into<SharedString>, tone: Tone) -> Div {
    div()
        .flex_shrink_0()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(tone.soft)
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(tone.text)
        .child(text.into())
}

/// Badge with a leading dot, for "● Работает".
pub fn dot_badge(text: impl Into<SharedString>, tone: Tone) -> Div {
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(5.))
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(tone.soft)
        .text_size(px(11.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(tone.text)
        .child(div().size(px(6.)).rounded_full().bg(tone.solid))
        .child(text.into())
}

/// Mono agent chip: `claude` (orange), `codex` (ink).
pub fn provider_chip(provider: Provider, p: &Palette, theme: &Theme) -> Div {
    let (bg, fg, border) = match provider {
        Provider::Claude => p.claude,
        Provider::Codex => p.codex,
    };
    div()
        .flex_shrink_0()
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(border)
        .bg(bg)
        .font_family(theme.mono_font_family.clone())
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(fg)
        .whitespace_nowrap()
        .child(provider.label().to_lowercase())
}

/// Neutral mono chip for tool names, paths, labels. Never wider than its
/// row: a long label ends in «…».
pub fn chip(text: impl Into<SharedString>, p: &Palette, theme: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .max_w_full()
        .truncate()
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(p.border)
        .bg(p.surface2)
        .font_family(theme.mono_font_family.clone())
        .text_size(px(11.))
        .text_color(p.muted)
        .child(text.into())
}

/// A chip with a branch mark: a task's own copy, a session in a worktree.
pub fn branch_chip(text: impl Into<SharedString>, p: &Palette, theme: &Theme) -> Div {
    h_flex()
        .flex_shrink(1.)
        .max_w_full()
        .min_w_0()
        .gap(px(3.))
        .px(px(5.))
        .rounded(px(4.))
        .border_1()
        .border_color(p.border)
        .bg(p.surface2)
        .font_family(theme.mono_font_family.clone())
        .text_size(px(11.))
        .text_color(p.muted)
        .child(gpui_kit::component::Icon::empty().path("icons/git-branch.svg").size(px(11.)).flex_shrink_0())
        .child(div().min_w_0().truncate().child(text.into()))
}

/// The sunken track of a segmented choice; children are `segment`s.
pub fn segmented(p: &Palette) -> Div {
    h_flex().p(px(3.)).gap(px(2.)).rounded(px(8.)).border_1().border_color(p.border).bg(p.sunken)
}

/// One option of a segmented choice; the caller sets the size and what a click does.
pub fn segment(id: &'static str, label: &'static str, active: bool, p: &Palette) -> Stateful<Div> {
    h_flex()
        .id(id)
        .rounded(px(6.))
        .border_1()
        .border_color(transparent_black())
        .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
        .text_color(if active { p.ink } else { p.muted })
        .cursor_pointer()
        .map(|this| {
            if active {
                this.bg(p.surface).border_color(p.border).shadow(edge_shadow(p.border_strong, 1.))
            } else {
                this.hover(|s| s.bg(p.ink.opacity(0.04)))
            }
        })
        .child(label)
}

/// Small filled dot (activity marker).
pub fn dot(color: Hsla) -> Div {
    div().size(px(7.)).flex_shrink_0().rounded_full().bg(color)
}

// ------------------------------------------------------------------ buttons

#[derive(Clone, Copy, PartialEq, Eq)]
enum PressKind {
    Neutral,
    Primary,
    Ghost,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PressSize {
    XSmall,
    Small,
    Medium,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// PostHog's skeuomorphic button: a dark base under a face lifted by 2px.
/// Hover lifts the face to 3px, press sinks it to 1.5px.
#[derive(IntoElement)]
pub struct PressButton {
    id: ElementId,
    kind: PressKind,
    size: PressSize,
    icon: Option<gpui_kit::component::Icon>,
    label: Option<SharedString>,
    loading: bool,
    disabled: bool,
    full: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl PressButton {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            kind: PressKind::Neutral,
            size: PressSize::Medium,
            icon: None,
            label: None,
            loading: false,
            disabled: false,
            full: false,
            tooltip: None,
            on_click: None,
        }
    }
    pub fn primary(mut self) -> Self {
        self.kind = PressKind::Primary;
        self
    }
    /// The default look; kept so call sites read like gpui-kit buttons.
    pub fn outline(mut self) -> Self {
        if self.kind == PressKind::Ghost {
            self.kind = PressKind::Neutral;
        }
        self
    }
    /// Flat toolbar button without the base.
    pub fn ghost(mut self) -> Self {
        self.kind = PressKind::Ghost;
        self
    }
    pub fn small(mut self) -> Self {
        self.size = PressSize::Small;
        self
    }
    pub fn xsmall(mut self) -> Self {
        self.size = PressSize::XSmall;
        self
    }
    pub fn icon(mut self, icon: impl Into<gpui_kit::component::Icon>) -> Self {
        self.icon = Some(icon.into());
        self
    }
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
    pub fn w_full(mut self) -> Self {
        self.full = true;
        self
    }
    pub fn tooltip(mut self, text: impl Into<SharedString>) -> Self {
        self.tooltip = Some(text.into());
        self
    }
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(f));
        self
    }
}

impl RenderOnce for PressButton {
    /// PostHog LemonButton: one element, 1px border, a solid `0 3px 0 -1px`
    /// shadow in the frame color. Hover lifts by 0.5px (shadow 3.5px), press
    /// sinks by 0.5px (shadow 2.5px); both darken the border. White text on orange.
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        use gpui_kit::component::{Sizable as _, spinner::Spinner, tooltip::Tooltip};
        const DEPTH: f32 = 3.;
        let theme = cx.theme();
        let dark = theme.is_dark();
        let p = Palette::of(theme);
        // Face height (without the 3px chrome), horizontal padding, font size, gap.
        let (height, pad, text, gap) = match self.size {
            PressSize::XSmall => (23., 8., 12., 4.),
            PressSize::Small => (28., 10., 13., 6.),
            PressSize::Medium => (34., 12., 14., 8.),
        };
        let icon_only = self.label.is_none();
        // (background, border, border on hover/press, frame shadow, text)
        let (bg, border, border_hover, frame, fg) = match (self.kind, dark) {
            (PressKind::Primary, false) => (p.accent.solid, p.accent_edge, hex(0x8A2C00), hex(0xD64400), gpui_kit::white()),
            (PressKind::Primary, true) => (p.accent.solid, p.accent_edge, hex(0xFF9B66), hex(0xC9500F), gpui_kit::white()),
            (PressKind::Neutral, false) => (hex(0xF6F6F1), hex(0xCACBC3), hex(0xA9AAA2), hex(0xE1DED8), hex(0x111111)),
            (PressKind::Neutral, true) => (p.surface2, p.border_strong, p.faint, p.border, p.fg),
            (PressKind::Ghost, _) => (gpui_kit::transparent_black(), gpui_kit::transparent_black(), gpui_kit::transparent_black(), gpui_kit::transparent_black(), p.fg),
        };
        let interactive = !self.disabled && !self.loading;
        let icon_el = if self.loading {
            Some(Spinner::new().color(fg).map(|s| if self.size == PressSize::XSmall { s.xsmall() } else { s.small() }).into_any_element())
        } else {
            self.icon.map(|i| {
                let i = i.text_color(fg).opacity(0.85);
                (if self.size == PressSize::XSmall { i.xsmall() } else { i.small() }).into_any_element()
            })
        };
        let content = h_flex()
            .size_full()
            .gap(px(gap))
            .justify_center()
            .text_size(px(text))
            .font_weight(FontWeight::MEDIUM)
            .text_color(fg)
            .whitespace_nowrap()
            .children(icon_el)
            .when_some(self.label, |d, l| d.child(l));
        let chrome = |depth: f32| {
            vec![BoxShadow { color: frame, offset: point(px(0.), px(depth)), blur_radius: px(0.), spread_radius: px(-1.), inset: false }]
        };

        div()
            .id(self.id.clone())
            .relative()
            .flex_shrink_0()
            .h(px(height))
            .when(self.full, |d| d.w_full())
            .when(!icon_only, |d| d.px(px(pad)))
            .when(icon_only, |d| d.w(px(height)))
            .rounded(px(6.))
            .when(self.disabled, |d| d.opacity(0.65))
            .when_some(self.tooltip, |d, t| d.tooltip(move |window, cx| Tooltip::new(t.clone()).build(window, cx)))
            .map(|d| {
                if self.kind == PressKind::Ghost {
                    d.when(interactive, |d| d.hover(|s| s.bg(p.ink.opacity(0.075))).active(|s| s.bg(p.ink.opacity(0.12))))
                } else {
                    d.mb(px(DEPTH))
                        .border_1()
                        .border_color(border)
                        .bg(bg)
                        .shadow(chrome(DEPTH))
                        .when(interactive, |d| {
                            d.hover(|s| s.top(px(-0.5)).border_color(border_hover).shadow(chrome(DEPTH + 0.5)))
                                .active(|s| s.top(px(0.5)).border_color(border_hover).shadow(chrome(DEPTH - 0.5)))
                        })
                }
            })
            .when_some(self.on_click.filter(|_| interactive), |d, f| d.cursor_pointer().on_click(move |e, w, cx| f(e, w, cx)))
            .child(content)
    }
}
