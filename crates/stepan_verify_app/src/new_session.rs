use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme as _, IconName,
    input::{InputEvent, Textarea, TextareaState},
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::zoom::px;
use crate::ui::PressButton;
use stepan_verify_core::Provider;

use crate::{
    attach::{self, Attachments, AttachmentsEvent},
    project_picker::{Picked, ProjectPicker},
    ui::{self, Palette},
};

/// Emitted when the user confirms the form.
pub struct StartSession {
    pub provider: Provider,
    pub cwd: PathBuf,
    pub prompt: String,
    /// Images sent with the first message.
    pub images: Vec<PathBuf>,
}

impl EventEmitter<StartSession> for NewSessionForm {}

/// Body of the "Новая сессия" dialog: project, agent, first message.
pub struct NewSessionForm {
    pub picker: Entity<ProjectPicker>,
    provider: Provider,
    prompt: Entity<TextareaState>,
    attachments: Entity<Attachments>,
    _subscriptions: Vec<Subscription>,
}

impl NewSessionForm {
    /// `projects`: candidate working directories, most recent first.
    pub fn new(projects: Vec<PathBuf>, loading: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let picker = cx.new(|_| ProjectPicker::new(projects, loading));
        let prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 12)
                .submit_on_enter(true)
                .placeholder("Что сделать? (Enter — запустить)")
        });
        prompt.update(cx, |p, cx| p.focus(window, cx));
        let attachments = cx.new(|_| Attachments::new("new-session-attachment", true));
        let subscriptions = vec![
            cx.subscribe(&prompt, |this, _, event, cx| match event {
                InputEvent::PressEnter { .. } => this.confirm(cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }),
            cx.subscribe(&picker, |_, _, _: &Picked, cx| cx.notify()),
            // Non-images go into the message as paths, like in the composer.
            cx.subscribe_in(&attachments, window, |this, _, event, window, cx| match event {
                AttachmentsEvent::NotImages(paths) => this.prompt.update(cx, |p, cx| {
                    let text = attach::append_mentions(&p.value(), paths);
                    p.set_value(text, window, cx);
                }),
                AttachmentsEvent::Changed => cx.notify(),
            }),
        ];
        Self { picker, provider: Provider::Claude, prompt, attachments, _subscriptions: subscriptions }
    }

    fn can_start(&self, cx: &App) -> bool {
        self.picker.read(cx).selected().is_some()
            && !self.prompt.read(cx).value().trim().is_empty()
            && !self.attachments.read(cx).is_capturing()
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if !self.can_start(cx) {
            return;
        }
        let Some(cwd) = self.picker.read(cx).selected().cloned() else { return };
        let prompt = self.prompt.read(cx).value().trim().to_owned();
        let images = self.attachments.read(cx).paths().to_vec();
        cx.emit(StartSession { provider: self.provider, cwd, prompt, images });
    }
}

impl Render for NewSessionForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        let provider_button = |id: &'static str, provider: Provider, cx: &mut Context<Self>| {
            let active = self.provider == provider;
            h_flex()
                .id(id)
                .h(px(32.))
                .px(px(12.))
                .gap(px(8.))
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
                .child(ui::provider_chip(provider, &p, &theme))
                .child(div().text_size(px(13.5)).font_weight(FontWeight::BOLD).text_color(if active { p.ink } else { p.muted }).child(provider.label()))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.provider = provider;
                    cx.notify();
                }))
        };
        let label = |text: &'static str| ui::eyebrow(text, &p, &theme);

        v_flex()
            .id("new-session-form")
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
            .child(v_flex().gap_2().child(label("Проект")).child(self.picker.clone()))
            .child(
                v_flex()
                    .gap_2()
                    .child(label("Агент"))
                    .child(
                        h_flex()
                            .self_start()
                            .p(px(3.))
                            .gap(px(2.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(p.border)
                            .bg(p.sunken)
                            .child(provider_button("provider-claude", Provider::Claude, cx))
                            .child(provider_button("provider-codex", Provider::Codex, cx)),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(label("Задача"))
                    .child(Textarea::new(&self.prompt).on_paste({
                        let tray = self.attachments.clone();
                        move |item, _, cx| tray.update(cx, |t, cx| t.paste(item, cx))
                    }))
                    .child(self.attachments.clone()),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        attach::attach_button("new-session-attach", &self.attachments, "Прикрепить картинку или файл")
                            .small()
                            .label("Прикрепить"),
                    )
                    .child(
                        attach::screenshot_button("new-session-screenshot", &self.attachments, cx)
                            .small()
                            .label("Снимок экрана"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.))
                            .text_color(p.faint)
                            .child("или Cmd+V, или перетащи сюда"),
                    )
                    .child(
                        PressButton::new("start-session")
                            .primary()
                            .icon(IconName::Play)
                            .label("Запустить")
                            .disabled(!self.can_start(cx))
                            .on_click(cx.listener(|this, _, _, cx| this.confirm(cx))),
                    ),
            )
    }
}
