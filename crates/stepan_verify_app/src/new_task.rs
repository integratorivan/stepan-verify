use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme as _, IconName,
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    h_flex, v_flex,
};
use gpui_kit::*;
use crate::zoom::px;
use crate::ui::PressButton;
use stepan_verify_core::{NewTask, Provider, TaskStatus, worktree::Isolation};

use crate::{
    attach::{self, Attachments, AttachmentsEvent},
    models,
    project_picker::{Picked, ProjectPicker},
    ui::{self, Palette},
};

/// Emitted when the user confirms the form.
pub struct CreateTask(pub NewTask);

impl EventEmitter<CreateTask> for NewTaskForm {}

/// Body of the "Новая задача" dialog.
pub struct NewTaskForm {
    pub picker: Entity<ProjectPicker>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    labels: Entity<InputState>,
    /// Screenshots, logs… copied next to the task on create.
    attachments: Entity<Attachments>,
    /// Straight to "К работе" (agents may take it) vs "Входящие".
    ready: bool,
    /// Separate copy, the project folder, or decide at run time (None).
    isolation: Option<Isolation>,
    /// Model per agent; None = Настройки → «Модель по умолчанию» at run time.
    claude_model: Option<String>,
    codex_model: Option<String>,
    /// «Сначала план»: the agent draws the target state for approval before coding.
    plan: bool,
    _subscriptions: Vec<Subscription>,
}

impl NewTaskForm {
    pub fn new(projects: Vec<PathBuf>, loading: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picker = cx.new(|_| ProjectPicker::new(projects, loading));
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Что нужно сделать"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx).auto_grow(4, 14).placeholder("Подробности, ссылки, критерии готовности…")
        });
        let labels = cx.new(|cx| InputState::new(window, cx).placeholder("kind=bug, area=admin"));
        let attachments = cx.new(|_| Attachments::new("new-task-attachment", false));
        title.update(cx, |t, cx| t.focus(window, cx));
        let subscriptions = vec![
            cx.subscribe(&title, |this, _, event, cx| match event {
                InputEvent::PressEnter { .. } => this.confirm(cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.subscribe(&picker, |_, _, _: &Picked, cx| cx.notify()),
            cx.subscribe(&attachments, |_, _, _: &AttachmentsEvent, cx| cx.notify()),
        ];
        Self { picker, title, body, labels, attachments, ready: true, isolation: None, claude_model: None, codex_model: None, plan: false, _subscriptions: subscriptions }
    }

    fn can_create(&self, cx: &App) -> bool {
        !self.title.read(cx).value().trim().is_empty() && !self.attachments.read(cx).is_capturing()
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.can_create(cx) {
            return;
        }
        let labels = self
            .labels
            .read(cx)
            .value()
            .split([',', ' '])
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect();
        cx.emit(CreateTask(NewTask {
            title: self.title.read(cx).value().trim().to_owned(),
            body: self.body.read(cx).value().trim().to_owned(),
            cwd: self.picker.read(cx).selected().cloned(),
            labels,
            depends_on: Vec::new(),
            status: if self.ready { TaskStatus::Todo } else { TaskStatus::Inbox },
            isolation: self.isolation,
            claude_model: self.claude_model.clone(),
            codex_model: self.codex_model.clone(),
            attachments: self.attachments.read(cx).paths().to_vec(),
            plan: self.plan,
        }));
    }
}

impl NewTaskForm {
    /// The agent's chip and its model dropdown; «по умолчанию» follows Настройки.
    fn model_choice(&self, provider: Provider, p: &Palette, theme: &gpui_kit::component::Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let current = match provider {
            Provider::Claude => self.claude_model.clone(),
            Provider::Codex => self.codex_model.clone(),
        };
        let fallback = models::settings_default(provider, cx).or_else(|| models::cli_default(provider));
        let form = cx.entity().downgrade();
        h_flex()
            .gap_2()
            .child(ui::provider_chip(provider, p, theme))
            .child(models::picker(
                SharedString::from(format!("new-task-model-{}", provider.label())),
                provider,
                current,
                fallback,
                move |model, _, cx| {
                    form.update(cx, |form, cx| {
                        match provider {
                            Provider::Claude => form.claude_model = model,
                            Provider::Codex => form.codex_model = model,
                        }
                        cx.notify();
                    })
                    .ok();
                },
            ))
    }
}

impl Render for NewTaskForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let field = |label: &'static str| ui::eyebrow(label, &p, &theme);
        let segment = |id: &'static str, label: &'static str, active: bool| ui::segment(id, label, active, &p).h(px(30.)).px(px(12.)).text_size(px(13.));
        let status_button = |id: &'static str, label: &'static str, ready: bool, cx: &mut Context<Self>| {
            segment(id, label, self.ready == ready).on_click(cx.listener(move |this, _, _, cx| {
                this.ready = ready;
                cx.notify();
            }))
        };
        let isolation_button = |id: &'static str, label: &'static str, isolation: Option<Isolation>, cx: &mut Context<Self>| {
            segment(id, label, self.isolation == isolation).on_click(cx.listener(move |this, _, _, cx| {
                this.isolation = isolation;
                cx.notify();
            }))
        };
        let plan_button = |id: &'static str, label: &'static str, plan: bool, cx: &mut Context<Self>| {
            segment(id, label, self.plan == plan).on_click(cx.listener(move |this, _, _, cx| {
                this.plan = plan;
                cx.notify();
            }))
        };
        let plan_hint = if self.plan {
            "Агент нарисует карту того, к чему приведёт код; ты утвердишь — потом делает"
        } else {
            "Агент сразу берётся за код"
        };
        let isolation_hint = match self.isolation {
            None => "Отдельная копия, если в проекте уже работает другой агент",
            Some(Isolation::Worktree) => "Своя git-копия и ветка stepan/<задача>",
            Some(Isolation::Here) => "Прямо в папке проекта",
        };
        let paste = |tray: &Entity<Attachments>| {
            let tray = tray.clone();
            move |item: &ClipboardItem, _: &mut Window, cx: &mut App| tray.update(cx, |t, cx| t.paste(item, cx))
        };
        v_flex()
            .id("new-task-form")
            .gap_4()
            .key_context(attach::CONTEXT)
            .on_action({
                let tray = self.attachments.clone();
                move |_: &attach::TakeScreenshot, _, cx| tray.update(cx, |t, cx| t.screenshot(cx))
            })
            .drag_over::<ExternalPaths>({
                let ring = theme.ring;
                move |style, _, _, _| style.border_2().border_color(ring).rounded(px(6.))
            })
            .on_drop({
                let tray = self.attachments.clone();
                move |paths: &ExternalPaths, _, cx| tray.update(cx, |t, cx| t.add(paths.paths(), cx))
            })
            .child(v_flex().gap_2().child(field("Задача")).child(Input::new(&self.title)))
            .child(
                v_flex()
                    .gap_2()
                    .child(field("Описание"))
                    .child(Textarea::new(&self.body).on_paste(paste(&self.attachments))),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(field("Вложения"))
                    .child(self.attachments.clone())
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                attach::attach_button("new-task-attach", &self.attachments, "Прикрепить картинки или файлы")
                                    .small()
                                    .label("Прикрепить"),
                            )
                            .child(
                                attach::screenshot_button("new-task-screenshot", &self.attachments, cx)
                                    .small()
                                    .label("Снимок экрана"),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.))
                                    .text_color(p.faint)
                                    .child("или Cmd+V в описании, или перетащи сюда"),
                            ),
                    ),
            )
            .child(v_flex().gap_2().child(field("Проект")).child(self.picker.clone()))
            .child(
                v_flex().gap_2().child(field("Где работать")).child(
                    h_flex()
                        .gap_3()
                        .child(
                            ui::segmented(&p)
                                .flex_shrink_0()
                                .child(isolation_button("isolation-auto", "Авто", None, cx))
                                .child(isolation_button("isolation-worktree", "Отдельная копия", Some(Isolation::Worktree), cx))
                                .child(isolation_button("isolation-here", "Здесь", Some(Isolation::Here), cx)),
                        )
                        .child(div().min_w_0().truncate().text_size(px(12.)).text_color(p.faint).child(isolation_hint)),
                ),
            )
            .child(
                v_flex().gap_2().child(field("Порядок работы")).child(
                    h_flex()
                        .gap_3()
                        .child(
                            ui::segmented(&p)
                                .flex_shrink_0()
                                .child(plan_button("workflow-direct", "Сразу делать", false, cx))
                                .child(plan_button("workflow-plan", "Сначала план", true, cx)),
                        )
                        .child(div().min_w_0().truncate().text_size(px(12.)).text_color(p.faint).child(plan_hint)),
                ),
            )
            .child(
                v_flex().gap_2().child(field("Модель")).child(
                    h_flex()
                        .gap_4()
                        .child(self.model_choice(Provider::Claude, &p, &theme, cx))
                        .child(self.model_choice(Provider::Codex, &p, &theme, cx)),
                ),
            )
            .child(v_flex().gap_2().child(field("Метки")).child(Input::new(&self.labels)))
            .child(
                v_flex().gap_2().child(field("Куда")).child(
                    ui::segmented(&p)
                        .self_start()
                        .child(status_button("to-todo", "К работе — агенты могут брать", true, cx))
                        .child(status_button("to-inbox", "Во входящие — разберу сам", false, cx)),
                ),
            )
            .child(
                h_flex().justify_end().child(
                    PressButton::new("create-task")
                        .primary()
                        .icon(IconName::Plus)
                        .label("Создать")
                        .disabled(!self.can_create(cx))
                        .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                ),
            )
    }
}
