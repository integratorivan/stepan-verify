//! «Сначала план»: the planning agent draws the target state in the same
//! report schema as the changes map (plus `stages` and `questions`), Stepan
//! renders it with the map machinery, and once the plan is approved the
//! implementation session's map is held against it («план vs факт»).

use std::{collections::HashSet, path::Path};

use gpui_kit::component::{Icon, IconName, Sizable as _, Theme, h_flex, skeleton::Skeleton, spinner::Spinner, text::TextView, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use stepan_verify_core::{Home, Phase, Task, TaskStatus, plans};

use super::{
    ChangesEvent, ChangesView, Lens, MAP_RULES, Panel, PlanMode, Report, SCHEMA, VIEW_RULES,
    map::{self, Graph, NodeStatus},
    severity_badge,
};
use crate::{
    format::ago,
    ui::{self, Palette, PressButton, Tone},
    zoom::px,
};

const PLAN_SCHEMA: &str = "Поля сверх схемы карты:\n\
 \"stages\": [{\"title\": \"этап простыми словами\", \"result\": \"что человек сможет запустить или увидеть после этапа\", \"nodes\": [\"id узлов из code\"], \"check\": \"как проверить: команда, тест, где посмотреть\"}],\n\
 \"questions\": [{\"id\": \"q1\", \"question\": \"что решить человеку\", \"options\": [\"вариант\", \"вариант\"], \"recommendation\": \"что выбрал бы ты\", \"why\": \"почему\"}]";

/// The first message of a planning session, after the task itself.
pub fn plan_brief(home: &Home, task: &Task) -> String {
    let draft = plans::draft(home, &task.id);
    let last = plans::latest(home, &task.id).map(|n| plans::report_path(home, &task.id, n));
    let previous = match last {
        Some(path) => format!("\n\nПрошлая итерация плана: {} — начни с неё и учти замечания человека.", path.display()),
        None => String::new(),
    };
    format!(
        "Порядок работы: СНАЧАЛА ПЛАН. Сейчас ты только планируешь: изучи код (текущая папка — проект) и покажи, к какому состоянию ты приведёшь код. \
         Ничего не меняй в проекте: не правь файлы, не коммить, не ставь зависимости, не запускай миграции. Читать код и запускать безопасные команды (поиск, тесты как есть) можно.\n\n\
         Человек не читает планы текстом: Stepan нарисует твой план картой — той же, что показывает изменения сессии, только про будущее. \
         Поэтому план — это JSON целевого состояния.\n\n\
         Папка плана: {draft} (дальше $PLAN). Запиши $PLAN/report.json по схеме карты, но про то, что БУДЕТ:\n\
         - `code.added` — что появится, `code.changed` — что изменится (`signature_before` — как сейчас, `signature` — как станет), `code.removed` — что уберётся, \
         `code.context` — 3–10 существующих узлов рядом, которые не меняются. Имена и пути — как будут в коде. {MAP_RULES}\n\
         - `dtos` — формы данных и контрактов, которые появятся или изменятся (поля со `status`).\n\
         - `views` — {VIEW_RULES} В плане деревья вызовов, компонентов и файлов пиши как было → станет (`diff: true`). \
         Макет UI — не больше одного HTML-файла $PLAN/explainer.html (вид `html`), только если план меняет интерфейс.\n\
         - `stages` — 2–6 вертикальных срезов по порядку: после каждого человек может что-то запустить или увидеть (`result`), у каждого — как проверить (`check`) и узлы (`nodes`, id из `code`). \
         Каждый узел из added/changed/removed — хотя бы в одном этапе.\n\
         - `questions` — решения, которые должен принять человек, с твоей рекомендацией. Нечего решать — пустой список. Не спрашивай то, что можно выяснить из кода.\n\
         - `risks` и `decisions` (что выбрал и почему) — как обычно; `flow`, `structure`, `diagrams`, `ui`, `tests`, `flows` — пусто, если не нужны.\n\
         - `title` — суть плана одной строкой, `summary` — 2–3 предложения, что получит пользователь.\n\n\
         Схема карты:\n{SCHEMA}\n{PLAN_SCHEMA}\n\n\
         Когда $PLAN/report.json записан, запусти `stepan_verify task plan-ready \"одна строка: что предлагаешь\"` — команда проверит план, сохранит его итерацией и отдаст задачу человеку. \
         Ругается — поправь и запусти снова. Потом остановись. Человек либо пришлёт замечания (тогда перепиши план целиком новой итерацией: снова $PLAN/report.json и plan-ready), \
         либо утвердит его — реализацию начнёт новая сессия.\n\
         Если без ответа человека план не составить — `stepan_verify task block \"вопрос\"` и остановись.{previous}",
        draft = draft.display(),
    )
}

/// What the implementation session is told about the approved plan.
pub fn implement_brief(report: &Path, run: usize) -> String {
    let stages = plans::outline(report).map(|o| o.stages).unwrap_or_default();
    let list: String = stages
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let result = if s.result.is_empty() { String::new() } else { format!(" — результат: {}", s.result) };
            let check = if s.check.is_empty() { String::new() } else { format!("; проверка: {}", s.check) };
            format!("{}. {}{result}{check}\n", i + 1, s.title)
        })
        .collect();
    format!(
        "План утверждён человеком (итерация #{run}): {report}. Это контракт: делай именно эти узлы (`code.added/changed/removed`), формы данных (`dtos`) и устройство (`views`). \
         Где человек не ответил на вопрос из `questions`, следуй рекомендации из плана.\n\n\
         Иди по этапам по порядку:\n{list}\n\
         После каждого этапа запусти его проверку и отметь: `stepan_verify task log \"этап N: что сделано — проверка: итог\"`. \
         Если план приходится менять по ходу — отметь в task log, что и почему.",
        report = report.display(),
    )
}

/// Remarks on a plan: the agent rewrites it as a new iteration.
pub fn remarks_header(title: &str, draft: &Path) -> String {
    format!(
        "Замечания к плану «{title}» — перепиши план новой итерацией (полный {}/report.json, затем `stepan_verify task plan-ready`):\n",
        draft.display()
    )
}

// ------------------------------------------------------------------ plan vs fact

/// How the implementation map covers the approved plan.
#[derive(Debug, Default, PartialEq)]
pub struct Coverage {
    /// Plan nodes the implementation has (indices into the plan graph).
    pub done: Vec<usize>,
    /// Plan nodes not there yet.
    pub unmet: Vec<usize>,
    /// Changed nodes the plan did not have (indices into the fact graph).
    pub extra: Vec<usize>,
}

/// The nodes a map is about: what changes, not the context around it.
fn planned(n: &map::MapNode) -> bool {
    !n.implicit && n.status != NodeStatus::Context && n.test.is_none()
}

/// `crate::app::Foo::bar(x)` → `bar`: the code name without path and arguments.
fn short_name(name: &str) -> String {
    let head = name.split('(').next().unwrap_or(name).trim();
    map::normalize(head.rsplit("::").next().unwrap_or(head).rsplit('.').next().unwrap_or(head))
}

fn same_file(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim().trim_start_matches("./"), b.trim().trim_start_matches("./"));
    a.is_empty() || b.is_empty() || a == b || a.ends_with(&format!("/{b}")) || b.ends_with(&format!("/{a}"))
}

/// One plan node and one changed node are the same thing: the same id, or
/// the same code name in the same file.
pub fn same_node(plan: &map::MapNode, fact: &map::MapNode) -> bool {
    if plan.key == fact.key {
        return true;
    }
    let name = short_name(&plan.name);
    !name.is_empty() && name == short_name(&fact.name) && same_file(&plan.file, &fact.file)
}

pub fn coverage(plan: &Graph, fact: &Graph) -> Coverage {
    let mut out = Coverage::default();
    let mut used: HashSet<usize> = HashSet::new();
    for (i, p) in plan.nodes.iter().enumerate().filter(|(_, n)| planned(n)) {
        match fact.nodes.iter().enumerate().find(|(j, f)| !used.contains(j) && planned(f) && same_node(p, f)) {
            Some((j, _)) => {
                used.insert(j);
                out.done.push(i);
            }
            None => out.unmet.push(i),
        }
    }
    out.extra = fact.nodes.iter().enumerate().filter(|(j, f)| planned(f) && !used.contains(j)).map(|(j, _)| j).collect();
    out
}

/// Nodes of a stage on the plan map: by id, else by name.
pub fn stage_nodes(g: &Graph, ids: &[String]) -> HashSet<usize> {
    let keys: Vec<String> = ids.iter().map(|k| map::normalize(k)).collect();
    g.nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| keys.iter().any(|k| *k == n.key || *k == map::normalize(&n.name) || (!n.entity.is_empty() && *k == map::normalize(&n.entity))))
        .map(|(i, _)| i)
        .collect()
}

// ------------------------------------------------------------------ rendering

impl ChangesView {
    pub(super) fn render_plan_header(&self, plan: &PlanMode, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let run = self.selected.and_then(|i| self.runs.get(i));
        let r = self.report();
        let eyebrow = match run {
            Some(run) if r.is_some() => {
                let by = Some(run.meta.reviewer.clone()).filter(|s| !s.is_empty()).map(|s| format!(" · {s}")).unwrap_or_default();
                format!("План · итерация {} · {}{by}", run.n, run.meta.at.map(ago).unwrap_or_default())
            }
            _ => "План".to_owned(),
        };
        let neutral = Tone { solid: p.faint, soft: p.surface2, text: p.muted };
        let state: Option<AnyElement> = plan.meta.as_ref().map(|t| match (t.phase(), t.meta.status, t.meta.plan_approved) {
            (Some(Phase::Implement), _, Some(n)) => ui::dot_badge(format!("утверждён · итерация #{n}"), p.green).into_any_element(),
            (_, TaskStatus::InProgress, _) => h_flex()
                .gap(px(6.))
                .text_size(px(12.5))
                .text_color(p.blue.text)
                .child(Spinner::new().xsmall().color(p.blue.solid))
                .child("агент пишет план")
                .into_any_element(),
            (_, TaskStatus::Review, _) => ui::dot_badge("ждёт твоего решения", p.amber).into_any_element(),
            (_, TaskStatus::Blocked, _) => ui::dot_badge("агенту нужен ответ", p.amber).into_any_element(),
            (_, status, _) => ui::dot_badge(status.label(), neutral).into_any_element(),
        });
        let out = self.out.clone();
        let title = r.map(|r| r.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| self.input.title.clone());
        v_flex()
            .gap(px(8.))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().flex_1().min_w_0().child(ui::eyebrow(eyebrow, p, theme).truncate()))
                    .when_some(state, |d, s| d.child(s))
                    .child(
                        PressButton::new("plan-session")
                            .small()
                            .outline()
                            .icon(IconName::SquareTerminal)
                            .label("Сессия")
                            .tooltip("Сессия агента, который пишет план")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChangesEvent::OpenSession))),
                    )
                    .child(
                        PressButton::new("plan-folder")
                            .small()
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .tooltip("Папка с итерациями плана")
                            .on_click(move |_, window, cx| crate::preview::open_in_app(out.clone(), None, window, cx)),
                    ),
            )
            .child(div().text_size(px(24.)).font_weight(FontWeight::SEMIBOLD).line_height(px(30.)).text_color(p.ink).child(format!("План: {title}")))
    }

    /// No readable iteration yet: the agent is writing, or the report is broken.
    pub(super) fn render_plan_pending(&self, err: Option<String>, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let writing = self.plan.as_ref().and_then(|pl| pl.meta.as_ref()).is_some_and(|t| t.meta.status == TaskStatus::InProgress);
        let (icon, text): (AnyElement, String) = match (&err, writing) {
            (Some(e), _) => (Icon::new(IconName::TriangleAlert).small().text_color(p.red.solid).into_any_element(), format!("Итерацию не прочитать: {e}. Напиши агенту в сессии — пусть перепишет report.json и снова вызовет plan-ready.")),
            (None, true) => (
                Spinner::new().small().color(p.blue.solid).into_any_element(),
                "Агент изучает код и пишет план. Код он не меняет; карта появится здесь, когда он вызовет plan-ready.".to_owned(),
            ),
            (None, false) => (
                Icon::new(IconName::Network).small().text_color(p.faint).into_any_element(),
                "Плана пока нет. Запусти задачу (Claude или Codex на карточке) — агент сначала нарисует карту того, к чему приведёт код.".to_owned(),
            ),
        };
        v_flex()
            .gap(px(20.))
            .child(
                ui::card(p)
                    .p(px(16.))
                    .gap(px(12.))
                    .child(h_flex().gap_2().items_start().child(icon).child(div().flex_1().text_size(px(13.5)).line_height(px(20.)).text_color(p.fg).child(text)))
                    .child(h_flex().child(
                        PressButton::new("plan-pending-session").small().icon(IconName::SquareTerminal).label("Открыть сессию").on_click(cx.listener(|_, _, _, cx| cx.emit(ChangesEvent::OpenSession))),
                    )),
            )
            .when(writing && err.is_none(), |d| {
                d.child(ui::mono("обычно 3–10 минут", theme).text_color(p.muted))
                    .child(Skeleton::new().w(relative(0.6)).h(px(14.)).rounded(px(4.)))
                    .child(Skeleton::new().secondary().w_full().h(px(320.)).rounded(px(12.)))
            })
    }

    /// Questions first, stages above the canvas they light up, then the map.
    pub(super) fn render_plan_report(&self, g: &Graph, r: &Report, canvas_h: f32, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let dir = self.run_dir();
        let fresh: HashSet<String> = self
            .selected
            .and_then(|i| self.runs[..i].iter().rev().find_map(super::Run::ok))
            .map(|prev| g.changed_since(&map::build_report(prev)))
            .unwrap_or_default();
        v_flex()
            .gap(px(22.))
            .when(!r.questions.is_empty(), |d| d.child(self.render_questions(r, p, theme, cx)))
            .when(!r.stages.is_empty(), |d| d.child(self.render_stages(g, r, p, theme, cx)))
            .child(self.render_lens_tabs(r, p, theme, cx))
            .child(match self.lens {
                Lens::Map => self.render_map_section(g, r, &fresh, canvas_h, p, theme, cx).into_any_element(),
                Lens::Views => self.render_views(g, r, &dir, p, theme, cx),
            })
            .when(!r.summary.is_empty(), |d| {
                d.child(
                    div()
                        .max_w(px(780.))
                        .text_size(px(14.))
                        .line_height(px(22.))
                        .text_color(p.fg)
                        .child(TextView::markdown(SharedString::from(format!("plan-summary-{}", dir.display())), r.summary.clone()).selectable(true)),
                )
            })
            .when_some(self.render_decisions(g, p, theme, cx), |d, x| d.child(x))
            .when(!r.risks.is_empty(), |d| {
                d.child(
                    v_flex().gap(px(10.)).child(ui::section_title("Риски", Some(r.risks.len().to_string()), p, theme)).child(ui::card(p).overflow_hidden().children(r.risks.iter().enumerate().map(|(i, risk)| {
                        h_flex()
                            .px(px(14.))
                            .py(px(9.))
                            .gap(px(10.))
                            .items_start()
                            .when(i > 0, |d| d.border_t_1().border_color(p.border))
                            .when_some(severity_badge(&risk.severity, p), |d, (l, t)| d.child(ui::badge(l, t).flex_shrink_0()))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(2.))
                                    .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(risk.title.clone()))
                                    .when(!risk.detail.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.muted).child(risk.detail.clone())))
                                    .when(!risk.fix.is_empty(), |v| v.child(div().text_size(px(12.5)).text_color(p.fg).child(format!("Как избежать: {}", risk.fix)))),
                            )
                    }))),
                )
            })
            .when_some(r.flow.clone(), |d, x| d.child(self.render_diagram("flow".into(), &x, &dir, p, theme)))
            .when_some(r.structure.clone(), |d, x| d.child(self.render_diagram("structure".into(), &x, &dir, p, theme)))
            .children(r.diagrams.iter().enumerate().map(|(i, x)| self.render_diagram(format!("extra-{i}"), x, &dir, p, theme)))
            .into_any_element()
    }

    /// «Нужно твоё решение»: what the agent cannot decide alone, with its pick.
    fn render_questions(&self, r: &Report, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let rec = |q: &plans::Question, o: &str| !q.recommendation.is_empty() && (map::normalize(o) == map::normalize(&q.recommendation) || q.recommendation.trim().starts_with(o.trim()));
        v_flex()
            .gap(px(10.))
            .child(
                ui::section_title("Нужно твоё решение", Some(r.questions.len().to_string()), p, theme)
                    .child(ui::mono("без ответа агент возьмёт свою рекомендацию", theme).text_color(p.faint))
                    .child(div().flex_1())
                    .child(PressButton::new("plan-answer").small().outline().icon(IconName::ArrowUp).label("Ответить").on_click(cx.listener(|this, _, _, cx| {
                        this.panel = Some(Panel::Return);
                        cx.notify();
                    }))),
            )
            .child(
                v_flex()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(ui::mix(p.amber.solid, p.border, 0.45))
                    .bg(ui::mix(p.amber.soft, p.surface, 0.5))
                    .overflow_hidden()
                    .children(r.questions.iter().enumerate().map(|(i, q)| {
                        let named = q.options.iter().any(|o| rec(q, o));
                        v_flex()
                            .px(px(14.))
                            .py(px(10.))
                            .gap(px(6.))
                            .when(i > 0, |d| d.border_t_1().border_color(ui::mix(p.amber.solid, p.border, 0.7)))
                            .child(
                                h_flex()
                                    .gap(px(10.))
                                    .items_start()
                                    .child(ui::mono(format!("В{}", i + 1), theme).flex_shrink_0().pt(px(1.)).font_weight(FontWeight::SEMIBOLD).text_color(p.amber.text))
                                    .child(div().flex_1().min_w_0().text_size(px(14.)).line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(q.question.clone())),
                            )
                            .child(
                                v_flex()
                                    .pl(px(32.))
                                    .gap(px(6.))
                                    .when(!q.options.is_empty(), |d| {
                                        d.child(h_flex().gap(px(6.)).flex_wrap().children(q.options.iter().map(|o| {
                                            if rec(q, o) { ui::badge(format!("{o} · рекомендую"), p.green) } else { ui::chip(o.clone(), p, theme) }
                                        })))
                                    })
                                    .when(!q.recommendation.is_empty() && !named, |d| {
                                        d.child(h_flex().gap(px(6.)).text_size(px(12.5)).child(div().text_color(p.muted).child("Рекомендую:")).child(div().text_color(p.ink).child(q.recommendation.clone())))
                                    })
                                    .when(!q.why.is_empty(), |d| d.child(div().text_size(px(12.5)).line_height(px(18.)).text_color(p.muted).child(q.why.clone()))),
                            )
                    })),
            )
    }

    /// Vertical slices in order; a click lights the stage's nodes on the canvas.
    fn render_stages(&self, g: &Graph, r: &Report, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.plan.as_ref().and_then(|pl| pl.stage);
        let mono = theme.mono_font_family.clone();
        let cards = r.stages.iter().enumerate().map(|(i, s)| {
            let on = active == Some(i);
            let count = stage_nodes(g, &s.nodes).len();
            v_flex()
                .id(SharedString::from(format!("plan-stage-{i}")))
                .flex_shrink_0()
                .w(px(290.))
                .p(px(12.))
                .gap(px(7.))
                .rounded(px(10.))
                .border_1()
                .bg(p.surface)
                .cursor_pointer()
                .map(|d| if on { d.border_color(p.ink).shadow(ui::card_shadow()) } else { d.border_color(p.border).hover(|s| s.border_color(p.border_strong)) })
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(plan) = &mut this.plan {
                        plan.stage = if plan.stage == Some(i) { None } else { Some(i) };
                    }
                    this.lens = Lens::Map;
                    cx.notify();
                }))
                .child(
                    h_flex()
                        .gap(px(8.))
                        .items_start()
                        .child(
                            div()
                                .flex_shrink_0()
                                .size(px(20.))
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .map(|d| if on { d.bg(p.ink).text_color(p.surface) } else { d.bg(p.surface2).text_color(p.muted) })
                                .font_family(mono.clone())
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child((i + 1).to_string()),
                        )
                        .child(div().flex_1().min_w_0().line_clamp(2).text_size(px(13.5)).line_height(px(19.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(s.title.clone())),
                )
                .when(!s.result.is_empty(), |d| d.child(div().line_clamp(3).text_size(px(12.5)).line_height(px(18.)).text_color(p.fg).child(s.result.replace('`', ""))))
                .when(!s.check.is_empty(), |d| {
                    d.child(
                        h_flex()
                            .gap(px(6.))
                            .items_start()
                            .child(Icon::new(IconName::CircleCheck).xsmall().mt(px(2.)).text_color(p.faint))
                            .child(div().flex_1().min_w_0().line_clamp(2).font_family(mono.clone()).text_size(px(11.5)).line_height(px(16.)).text_color(p.muted).child(s.check.clone())),
                    )
                })
                .child(ui::mono(if count == 0 { "узлы не указаны".to_owned() } else { format!("узлов на карте: {count}") }, theme).text_color(if count == 0 { p.amber.text } else { p.faint }))
        });
        v_flex()
            .gap(px(10.))
            .child(ui::section_title("Этапы", Some(r.stages.len().to_string()), p, theme).child(ui::mono("после каждого можно что-то запустить · клик — подсветит узлы на карте", theme).text_color(p.faint)))
            .child(div().id("plan-stages-scroll").overflow_x_scroll().child(h_flex().gap(px(12.)).items_start().pb(px(4.)).children(cards)))
    }

    /// Bottom bar of a plan: remarks back to the planning agent, or approve.
    pub(super) fn render_plan_bar(&self, plan: &PlanMode, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let run = self.selected.and_then(|i| self.runs.get(i)).filter(|r| r.ok().is_some()).map(|r| r.n);
        let task = plan.meta.as_ref();
        let implementing = task.is_some_and(|t| t.phase() == Some(Phase::Implement));
        let latest = self.runs.iter().rev().find(|r| r.ok().is_some()).map(|r| r.n);
        h_flex()
            .flex_shrink_0()
            .px(px(20.))
            .py(px(10.))
            .gap(px(10.))
            .border_t_1()
            .border_color(p.border)
            .bg(p.surface)
            .child(
                ui::mono(
                    match (run, latest) {
                        (Some(n), Some(l)) if n != l => format!("{} в замечаниях · смотришь итерацию #{n}, последняя — #{l}", self.remarks.len()),
                        _ => format!("{} в замечаниях", self.remarks.len()),
                    },
                    theme,
                )
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(p.muted),
            )
            .child(
                PressButton::new("plan-remarks")
                    .small()
                    .icon(IconName::Undo2)
                    .label("Замечания к плану")
                    .disabled(implementing)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.panel = Some(Panel::Return);
                        cx.notify();
                    })),
            )
            .child(
                PressButton::new("plan-approve")
                    .small()
                    .primary()
                    .icon(IconName::Check)
                    .label(if implementing { "План утверждён" } else { "Утвердить план" })
                    .disabled(implementing || run.is_none())
                    .tooltip(match run {
                        Some(n) => format!("Итерация #{n} уйдёт агенту на реализацию по этапам"),
                        None => "Плана пока нет".to_owned(),
                    })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(n) = run {
                            cx.emit(ChangesEvent::ApprovePlan(n));
                        }
                    })),
            )
            .into_any_element()
    }

    /// «План: выполнено N из M · вне плана K» on the implementation's map.
    pub(super) fn render_coverage(&self, plan: &Report, g: &Graph, p: &Palette, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let pg = map::build_report(plan);
        let c = coverage(&pg, g);
        let total = c.done.len() + c.unmet.len();
        let row = |label: &'static str, tone: Tone| div().w(px(86.)).flex_shrink_0().pt(px(3.)).text_size(px(12.)).text_color(tone.text).child(label);
        v_flex()
            .p(px(12.))
            .gap(px(8.))
            .rounded(px(10.))
            .border_1()
            .border_color(p.border)
            .bg(p.surface)
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(Icon::empty().path("icons/list-checks.svg").small().text_color(if c.unmet.is_empty() { p.green.solid } else { p.amber.solid }))
                    .child(div().text_size(px(13.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(format!("План: выполнено {} из {total}", c.done.len())))
                    .child(div().text_size(px(13.)).text_color(if c.extra.is_empty() { p.muted } else { p.amber.text }).child(format!("· вне плана {}", c.extra.len())))
                    .child(div().flex_1())
                    .child(ui::mono(plan.title.clone(), theme).truncate().text_color(p.faint)),
            )
            .when(!c.unmet.is_empty(), |d| {
                d.child(
                    h_flex().gap(px(8.)).items_start().child(row("не сделано", p.amber)).child(
                        h_flex().flex_1().gap(px(5.)).flex_wrap().children(c.unmet.iter().take(24).map(|&i| {
                            let n = &pg.nodes[i];
                            ui::chip(format!("{} · {}", n.title(), super::status_label(n.status, true)), p, theme)
                        })),
                    ),
                )
            })
            .when(!c.extra.is_empty(), |d| {
                d.child(
                    h_flex().gap(px(8.)).items_start().child(row("вне плана", Tone { solid: p.faint, soft: p.surface2, text: p.muted })).child(
                        h_flex().flex_1().gap(px(5.)).flex_wrap().children(
                            c.extra.iter().take(24).map(|&j| self.node_chip(SharedString::from(format!("coverage-extra-{}", g.nodes[j].key)), &g.nodes[j].key, g, p, theme, cx)).collect::<Vec<_>>(),
                        ),
                    ),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Graph, Home, Report, Task, coverage, implement_brief, map, plan_brief, plans, stage_nodes};

    fn report(json: &str) -> Report {
        serde_json::from_str(json).unwrap()
    }

    #[core::prelude::v1::test]
    fn plan_report_parses_stages_and_questions() {
        let r = report(
            r#"{"title": "План", "code": {"added": [{"id": "plan_view", "name": "PlanView", "file": "src/plan.rs"}], "changed": [{"id": "meta", "name": "TaskMeta"}]},
                "stages": [{"title": "Модель", "result": "задача помнит этап", "nodes": ["meta"], "check": "cargo test"}, {"title": "Экран", "nodes": ["plan_view"]}],
                "questions": [{"id": "q1", "question": "Отдельная страница?", "options": ["да", "нет"], "recommendation": "да", "why": "не мешает сессии"}, "Где хранить?"],
                "views": [{"kind": "html", "file": "explainer.html"}]}"#,
        );
        assert_eq!(r.stages.len(), 2);
        assert_eq!(r.stages[0].check, "cargo test");
        assert_eq!(r.questions[0].recommendation, "да");
        assert_eq!(r.questions[1].question, "Где хранить?");
        let g = map::build_report(&r);
        assert_eq!(stage_nodes(&g, &r.stages[0].nodes).len(), 1);
        assert_eq!(stage_nodes(&g, &["PlanView".into()]).len(), 1, "by name too");
        // A changes report without them still parses.
        assert!(report(r#"{"title": "x"}"#).stages.is_empty());
    }

    #[core::prelude::v1::test]
    fn plan_vs_fact_matches_by_id_or_name_and_file() {
        let plan = map::build_report(&report(
            r#"{"code": {"added": [{"id": "plan_ready", "name": "Board::plan_ready", "file": "crates/core/src/tasks.rs"}, {"id": "stages", "name": "render_stages"}],
                         "changed": [{"id": "meta", "name": "TaskMeta", "file": "src/tasks.rs"}],
                         "context": [{"id": "board", "name": "Board"}]}}"#,
        ));
        let fact = map::build_report(&report(
            r#"{"code": {"added": [{"id": "x1", "name": "plan_ready(id, by, run)", "file": "crates/core/src/tasks.rs"}, {"id": "cli", "name": "plan-ready"}],
                         "changed": [{"id": "meta", "name": "TaskMeta"}, {"id": "y", "name": "render_stages", "file": "other/elsewhere.rs"}],
                         "context": [{"id": "z", "name": "Home"}]},
                "tests": [{"id": "t", "title": "tests", "file": "t.rs", "verifies": ["meta"]}]}"#,
        ));
        let c = coverage(&plan, &fact);
        let names = |g: &Graph, ix: &[usize]| ix.iter().map(|&i| g.nodes[i].name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&plan, &c.done), ["Board::plan_ready", "render_stages", "TaskMeta"]);
        assert!(c.unmet.is_empty());
        assert_eq!(names(&fact, &c.extra), ["plan-ready"], "context and tests are not out of plan");

        // A different file is a different thing.
        let other = map::build_report(&report(r#"{"code": {"added": [{"id": "q", "name": "plan_ready", "file": "src/cli.rs"}]}}"#));
        let c = coverage(&plan, &other);
        assert_eq!((c.done.len(), c.unmet.len(), c.extra.len()), (0, 3, 1));
    }

    #[core::prelude::v1::test]
    fn prompts_point_at_the_plan() {
        let root = std::env::temp_dir().join(format!("stepan_verify-plan-prompt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = Home { root: root.clone() };
        let task = Task { id: "t-1".into(), path: root.join("tasks/t-1.md"), meta: Default::default(), body: String::new() };
        let brief = plan_brief(&home, &task);
        assert!(brief.contains(&plans::draft(&home, "t-1").display().to_string()) && brief.contains("plan-ready") && brief.contains("\"stages\""), "{brief}");
        assert!(!brief.contains("Прошлая итерация"));

        let report = plans::report_path(&home, "t-1", 2);
        std::fs::create_dir_all(report.parent().unwrap()).unwrap();
        std::fs::write(&report, r#"{"stages": [{"title": "Модель", "result": "задача помнит этап", "check": "cargo test -p core"}, {"title": "Экран"}]}"#).unwrap();
        let brief = implement_brief(&report, 2);
        assert!(brief.contains("итерация #2") && brief.contains("1. Модель — результат: задача помнит этап; проверка: cargo test -p core") && brief.contains("2. Экран"), "{brief}");
        assert!(plan_brief(&home, &task).contains("Прошлая итерация плана"));
        let _ = std::fs::remove_dir_all(root);
    }
}
