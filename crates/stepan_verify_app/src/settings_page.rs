//! Settings: agent access mode, default agent, autopilot limit, report model,
//! document rules and the post-session distiller, fonts, updates.

use gpui_kit::component::{
    ActiveTheme, Icon, IconName, IndexPath, Sizable as _, Theme, h_flex,
    select::{SearchableVec, Select, SelectEvent, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use stepan_verify_core::{AgentMode, Provider, config::ProviderSetting};

use crate::{
    app::AppView,
    settings,
    ui::{self, Palette, PressButton},
};

/// A searchable list of the machine's font families.
pub type FontPicker = SelectState<SearchableVec<String>>;

/// Make the text / code font pickers the first time the page shows.
pub fn ensure_pickers(app: &mut AppView, window: &mut Window, cx: &mut Context<AppView>) {
    if app.font_pickers.is_some() {
        return;
    }
    let families = ui::font_families(cx);
    let config = settings::config(cx).appearance.clone();
    let mut make = |current: &str, mono: bool| {
        let selected = families.iter().position(|f| f == current).map(IndexPath::new);
        let list = families.clone();
        let state = cx.new(|cx| FontPicker::new(SearchableVec::new(list), selected, window, cx).searchable(true));
        cx.subscribe(&state, move |_, _, event: &SelectEvent<SearchableVec<String>>, cx| {
            if let SelectEvent::Confirm(Some(family)) = event {
                let family = family.clone();
                settings::update(cx, move |c| if mono { c.appearance.mono_font = family } else { c.appearance.font = family });
                ui::apply_fonts(cx);
            }
        })
        .detach();
        state
    };
    let text = make(&config.font, false);
    let mono = make(&config.mono_font, true);
    app.font_pickers = Some([text, mono]);
}

pub fn render(app: &AppView, _: &mut Window, cx: &mut Context<AppView>) -> impl IntoElement {
    let theme = cx.theme().clone();
    let p = Palette::of(&theme);
    let config = settings::config(cx).clone();
    let home = tilde(&app.home.root);

    let modes = h_flex().w_full().gap(px(10.)).items_stretch().children(AgentMode::ALL.map(|mode| {
        let active = config.agents.mode == mode;
        mode_card(mode, active, &p, &theme)
            .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, |c| c.agents.mode = mode)))
    }));

    let providers = segmented(
        &p,
        [(ProviderSetting::Claude, Provider::Claude, "Claude"), (ProviderSetting::Codex, Provider::Codex, "Codex")].map(|(setting, provider, label)| {
            let active = config.agents.default_provider == setting;
            segment(SharedString::from(format!("provider-{label}")), active, &p)
                .gap(px(8.))
                .child(ui::provider_chip(provider, &p, &theme))
                .child(div().text_size(px(13.5)).font_weight(FontWeight::BOLD).text_color(if active { p.ink } else { p.muted }).child(label))
                .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, |c| c.agents.default_provider = setting)))
                .into_any_element()
        }),
    );

    let limits = h_flex()
        .gap(px(14.))
        .child(segmented(
            &p,
            [1usize, 2, 3, 4].map(|n| {
                let active = config.agents.autopilot_limit == n;
                segment(SharedString::from(format!("autopilot-{n}")), active, &p)
                    .w(px(48.))
                    .justify_center()
                    .text_size(px(13.5))
                    .font_weight(if active { FontWeight::BOLD } else { FontWeight::MEDIUM })
                    .text_color(if active { p.ink } else { p.muted })
                    .child(n.to_string())
                    .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, |c| c.agents.autopilot_limit = n)))
                    .into_any_element()
            }),
        ))
        .child(
            div()
                .max_w(px(320.))
                .text_size(px(12.5))
                .text_color(p.muted)
                .child("аренда задачи — 15 мин, продлевается, пока агент пишет в журнал"),
        );

    let models = segmented(
        &p,
        [("haiku", "~40 с"), ("sonnet", "1–3 мин"), ("opus", "3–6 мин")].map(|(m, time)| {
            let active = config.changes.model == m;
            segment(SharedString::from(format!("changes-model-{m}")), active, &p)
                .gap(px(8.))
                .child(ui::mono(m, &theme).text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(if active { p.ink } else { p.muted }))
                .child(ui::mono(time, &theme).text_size(px(10.5)).text_color(p.faint))
                .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, |c| c.changes.model = m.to_owned())))
                .into_any_element()
        }),
    );

    // Codex sessions are reviewed by Codex: its default model or one from its cache.
    let codex_default = stepan_verify_core::config::codex_default_model();
    let mut codex_options: Vec<(String, String)> =
        vec![(String::new(), format!("по умолчанию{}", codex_default.as_deref().map(|m| format!(" · {m}")).unwrap_or_default()))];
    codex_options.extend(stepan_verify_core::config::codex_models().into_iter().map(|(slug, _)| (slug.clone(), slug)));
    let codex_models = h_flex()
        .flex_wrap()
        .max_w(px(440.))
        .p(px(3.))
        .gap(px(2.))
        .rounded(px(8.))
        .border_1()
        .border_color(p.border)
        .bg(p.sunken)
        .children(codex_options.into_iter().map(|(slug, label)| {
            let active = config.changes.codex_model == slug;
            segment(SharedString::from(format!("changes-codex-{label}")), active, &p)
                .h(px(28.))
                .px(px(10.))
                .child(ui::mono(label, &theme).text_size(px(12.)).font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::NORMAL }).text_color(if active { p.ink } else { p.muted }))
                .on_click(cx.listener(move |_, _, _, cx| {
                    let slug = slug.clone();
                    settings::update(cx, move |c| c.changes.codex_model = slug)
                }))
        }));

    let media_modes = segmented(
        &p,
        stepan_verify_core::config::MediaMode::ALL.map(|m| {
            let active = config.changes.media == m;
            segment(SharedString::from(format!("media-mode-{}", m.label())), active, &p)
                .text_size(px(13.))
                .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .text_color(if active { p.ink } else { p.muted })
                .child(m.label())
                .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, move |c| c.changes.media = m)))
                .into_any_element()
        }),
    );

    let brief = segmented(
        &p,
        [(true, "вкл"), (false, "выкл")].map(|(on, label)| {
            let active = config.docs.brief == on;
            segment(SharedString::from(format!("docs-brief-{label}")), active, &p)
                .text_size(px(13.))
                .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                .text_color(if active { p.ink } else { p.muted })
                .child(label)
                .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, move |c| c.docs.brief = on)))
                .into_any_element()
        }),
    );

    let distill = h_flex()
        .flex_wrap()
        .gap(px(10.))
        .child(segmented(
            &p,
            stepan_verify_core::config::DistillMode::ALL.map(|m| {
                let active = config.docs.distill == m;
                segment(SharedString::from(format!("distill-{}", m.label())), active, &p)
                    .text_size(px(13.))
                    .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                    .text_color(if active { p.ink } else { p.muted })
                    .child(m.label())
                    .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, move |c| c.docs.distill = m)))
                    .into_any_element()
            }),
        ))
        .when(config.docs.distill == stepan_verify_core::config::DistillMode::Auto, |row| {
            row.child(segmented(
                &p,
                [5u64, 10, 20, 30].map(|n| {
                    let active = config.docs.distill_after_min == n;
                    segment(SharedString::from(format!("distill-after-{n}")), active, &p)
                        .justify_center()
                        .text_size(px(13.))
                        .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                        .text_color(if active { p.ink } else { p.muted })
                        .child(format!("{n} мин"))
                        .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, move |c| c.docs.distill_after_min = n)))
                        .into_any_element()
                }),
            ))
        });

    // What is in use right now (a missing choice has fallen back).
    let (text_font, mono_font) = (theme.font_family.clone(), theme.mono_font_family.clone());
    let font_row = |picker: Option<&Entity<FontPicker>>, id: &'static str, sample: &'static str, family: SharedString, chosen: &str, mono: bool| {
        v_flex()
            .gap(px(10.))
            .w(px(420.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .children(picker.map(|state| Select::new(state).id(id).w(px(280.)).menu_max_h(px(360.)).placeholder("Шрифт")))
                    .child(
                        PressButton::new(SharedString::from(format!("{id}-reset")))
                            .small()
                            .ghost()
                            .label("По умолчанию")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                settings::update(cx, move |c| {
                                    let d = stepan_verify_core::config::AppearanceConfig::default();
                                    if mono { c.appearance.mono_font = d.mono_font } else { c.appearance.font = d.font }
                                });
                                ui::apply_fonts(cx);
                                this.font_pickers = None;
                                ensure_pickers(this, window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(p.border)
                    .bg(p.surface)
                    .font_family(family.clone())
                    .text_size(px(if mono { 12.5 } else { 14. }))
                    .text_color(p.fg)
                    .child(sample),
            )
            .when(family.as_ref() != chosen, |d| {
                d.child(div().text_size(px(12.)).text_color(p.amber.text).child(format!("«{chosen}» не найден на этом Mac — сейчас {family}")))
            })
    };
    let pickers = app.font_pickers.clone();
    let text_fonts = font_row(pickers.as_ref().map(|p| &p[0]), "font-text", "Агент закончил разбор: p95 упал с 182 до 61 мс. Съешь же ещё этих мягких французских булок.", text_font, &config.appearance.font, false);
    let mono_fonts = font_row(pickers.as_ref().map(|p| &p[1]), "font-mono", "fn search(q: &str) -> Vec<Item> { /* 0O 1lI {} => != */ }", mono_font, &config.appearance.mono_font, true);

    let update_state = crate::update::state(cx);
    let update_line: String = match &update_state {
        crate::update::State::Idle => "ещё не проверял".into(),
        crate::update::State::Checking => "проверяю…".into(),
        crate::update::State::UpToDate => "это последняя версия".into(),
        crate::update::State::Available(r) => format!("доступна {} — кнопка в сайдбаре", r.version),
        crate::update::State::Installing(r) => format!("ставлю {}…", r.version),
        crate::update::State::Failed(e) => format!("не получилось: {e}"),
    };
    let updates = v_flex()
        .gap(px(10.))
        .child(
            h_flex()
                .gap(px(10.))
                .child(segmented(
                    &p,
                    [(true, "проверять"), (false, "не проверять")].map(|(on, label)| {
                        let active = config.updates.check == on;
                        segment(SharedString::from(format!("updates-{label}")), active, &p)
                            .text_size(px(13.))
                            .font_weight(if active { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                            .text_color(if active { p.ink } else { p.muted })
                            .child(label)
                            .on_click(cx.listener(move |_, _, _, cx| settings::update(cx, move |c| c.updates.check = on)))
                            .into_any_element()
                    }),
                ))
                .child(
                    PressButton::new("updates-check")
                        .small()
                        .icon(IconName::RefreshCw)
                        .label("Проверить сейчас")
                        .on_click(cx.listener(|_, _, _, cx| crate::update::check(cx))),
                ),
        )
        .child(
            h_flex()
                .gap(px(8.))
                .child(ui::mono(format!("Stepan {}", crate::update::VERSION), &theme).text_size(px(12.)).text_color(p.fg))
                .child(div().text_size(px(12.5)).text_color(p.muted).child(update_line)),
        );

    div().id("settings").size_full().overflow_y_scroll().child(
        v_flex()
            .w_full()
            .max_w(px(940.))
            .mx_auto()
            .px(px(32.))
            .pt(px(36.))
            .pb(px(40.))
            .child(ui::page_title("Настройки", &p))
            .child(
                h_flex()
                    .mt(px(6.))
                    .min_w_0()
                    .gap(px(6.))
                    .text_size(px(13.5))
                    .text_color(p.muted)
                    .child("Общая папка:")
                    .child(ui::mono(home.clone(), &theme).min_w_0().truncate().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.fg)),
            )
            .child(row(
                "Режим агентов",
                "Сколько агенты могут делать без твоего подтверждения. Применяется к новым процессам агентов; в открытой сессии — после её переоткрытия.",
                modes,
                &p,
            ))
            .child(row("Агент по умолчанию", "Кого запускает автопилот для задач без метки agent=codex.", providers, &p))
            .child(row("Автопилот", "Сколько агентов он может держать в работе одновременно.", limits, &p))
            .child(row(
                "Отчёт «Изменения» · Claude",
                "Отчёт пишет агент той же семьи, что и сессия. Для Claude-сессий — эта модель. Sonnet — баланс скорости и качества.",
                models,
                &p,
            ))
            .child(row(
                "Отчёт «Изменения» · Codex",
                "Для Codex-сессий отчёт собирает Codex. Список моделей — из кэша Codex; «по умолчанию» — модель из ~/.codex/config.toml.",
                codex_models,
                &p,
            ))
            .child(row(
                "Before / after",
                "После хода агента с правками интерфейса агент той же семьи снимает «Было → Стало» скиллом pr-before-after: скриншоты, пары, GIF. Хранится в ~/.stepan_verify/tmp/sessions. В «Изменениях» всегда есть кнопка «Снять before/after».",
                media_modes,
                &p,
            ))
            .child(row("Шрифт текста", "Интерфейс, документы, сообщения. Instrument Sans встроен в Stepan и есть всегда; остальные — из шрифтов этого Mac.", text_fonts, &p))
            .child(row("Шрифт кода", "Код, диффы, пути, команды, метки. Моноширинный — иначе колонки поедут.", mono_fonts, &p))
            .child(row(
                "Обновления",
                "Новые версии приходят из GitHub Releases (репозиторий публичный). Установленное приложение обновляется само и перезапускается; запущенное из cargo — открывает страницу релиза.",
                updates,
                &p,
            ))
            .child(row(
                "Документы · правила агентам",
                "Каждая сессия из Stepan получает правила полки (разборы, решения, фактура проекта, вопросы тебе) и то, что полка уже знает о проекте: project.md, решения в силе, открытые вопросы.",
                brief,
                &p,
            ))
            .child(row(
                "Документы · итоги сессий",
                "Когда сессия затихла, агент той же семьи (модель — как у «Изменений») читает новое в ней и записывает решения, фактуру проекта и открытые вопросы. Чаще всего записывать нечего — тогда он ничего не пишет.",
                distill,
                &p,
            ))
            .child(
                h_flex()
                    .pt(px(22.))
                    .border_t_1()
                    .border_color(p.border)
                    .gap(px(24.))
                    .child(ui::mono(format!("Хранится в {home}/config.toml"), &theme).min_w_0().truncate().text_size(px(11.)).text_color(p.faint))
                    .child(ui::mono(format!("Stepan {} · gpui-kit", env!("CARGO_PKG_VERSION")), &theme).flex_shrink_0().text_size(px(11.)).text_color(p.faint)),
            ),
    )
}

fn tilde(path: &std::path::Path) -> String {
    let s = path.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

/// A settings row: title and hint on the left, the control on the right.
fn row(title: &'static str, hint: &'static str, control: impl IntoElement, p: &Palette) -> impl IntoElement {
    h_flex()
        .mt(px(22.))
        .pt(px(22.))
        .pb(px(2.))
        .border_t_1()
        .border_color(p.border)
        .items_start()
        .gap(px(28.))
        .child(
            v_flex()
                .w(px(280.))
                .flex_shrink_0()
                .gap(px(8.))
                .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(p.ink).child(title))
                .child(div().text_size(px(13.)).line_height(px(20.)).text_color(p.muted).child(hint)),
        )
        .child(h_flex().flex_1().min_w_0().items_start().child(control))
}

/// What each mode lets the agent do on its own: (what, allowed).
fn permissions(mode: AgentMode) -> [(&'static str, bool); 3] {
    match mode {
        AgentMode::Full => [("правки", true), ("команды", true), ("сеть", true)],
        AgentMode::AutoEdits => [("правки", true), ("команды", false), ("сеть", false)],
        AgentMode::Ask => [("правки", false), ("команды", false), ("сеть", false)],
    }
}

fn level(mode: AgentMode) -> usize {
    match mode {
        AgentMode::Full => 3,
        AgentMode::AutoEdits => 2,
        AgentMode::Ask => 1,
    }
}

fn mode_card(mode: AgentMode, active: bool, p: &Palette, theme: &Theme) -> Stateful<Div> {
    let lvl = level(mode);
    v_flex()
        .id(SharedString::from(format!("mode-{mode:?}")))
        .flex_1()
        .min_w_0()
        .p(px(13.))
        .gap(px(10.))
        .rounded(px(10.))
        .bg(p.surface)
        .cursor_pointer()
        .map(|this| {
            if active {
                this.border_2().border_color(p.ink).shadow(ui::edge_shadow(p.ink, 2.))
            } else {
                this.border_1().border_color(p.border).shadow(ui::card_shadow()).hover(|s| s.border_color(p.border_strong))
            }
        })
        .child(
            h_flex()
                .gap(px(8.))
                .child(radio(active, p))
                .child(div().text_size(px(14.)).font_weight(FontWeight::BOLD).text_color(p.ink).child(mode.label())),
        )
        .child(div().text_size(px(12.5)).line_height(px(19.)).text_color(p.muted).child(mode.description()))
        .child(v_flex().gap(px(3.)).children(permissions(mode).map(|(what, ok)| {
            let (icon, color, text) = if ok {
                (IconName::Check, p.green.text, what.to_owned())
            } else {
                (IconName::Info, p.amber.text, format!("{what} — спросит"))
            };
            h_flex()
                .gap(px(6.))
                .child(Icon::new(icon).xsmall().text_color(color))
                .child(ui::mono(text, theme).text_size(px(11.)).text_color(color))
        })))
        .child(div().flex_1())
        .child(h_flex().gap(px(4.)).children((0..3).map(|i| {
            div().flex_1().h(px(3.)).rounded_full().bg(if i < lvl { p.ink } else { p.border })
        })))
}

fn radio(active: bool, p: &Palette) -> impl IntoElement {
    div()
        .size(px(15.))
        .flex_shrink_0()
        .rounded_full()
        .border_1()
        .border_color(if active { p.accent.solid } else { p.border_strong })
        .bg(p.surface)
        .flex()
        .items_center()
        .justify_center()
        .when(active, |this| this.border_color(p.ink).child(div().size(px(8.)).rounded_full().bg(p.accent.solid)))
}

/// Segmented control frame.
fn segmented(p: &Palette, items: impl IntoIterator<Item = AnyElement>) -> impl IntoElement {
    h_flex()
        .p(px(3.))
        .gap(px(2.))
        .rounded(px(8.))
        .border_1()
        .border_color(p.border)
        .bg(p.sunken)
        .children(items)
}

fn segment(id: SharedString, active: bool, p: &Palette) -> Stateful<Div> {
    h_flex()
        .id(id)
        .h(px(32.))
        .px(px(14.))
        .items_center()
        .rounded(px(6.))
        .border_1()
        .border_color(transparent_black())
        .cursor_pointer()
        .map(|this| {
            if active {
                this.bg(p.surface).border_color(p.border).shadow(ui::edge_shadow(p.border_strong, 1.))
            } else {
                this.hover(|s| s.bg(p.ink.opacity(0.04)))
            }
        })
}
