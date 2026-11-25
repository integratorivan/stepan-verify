use std::path::PathBuf;

use gpui_kit::component::{
    ActiveTheme, IconName,
    tooltip::Tooltip,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use crate::ui::PressButton;

use crate::ui::{self, Palette};

/// Project chips (recent working directories) plus "Другая папка…".
/// Shared by the new-session and new-task forms.
pub struct ProjectPicker {
    projects: Vec<PathBuf>,
    /// The list is still being scanned.
    loading: bool,
    selected: Option<PathBuf>,
}

/// The selection changed.
pub struct Picked;

impl EventEmitter<Picked> for ProjectPicker {}

impl ProjectPicker {
    pub fn new(projects: Vec<PathBuf>, loading: bool) -> Self {
        Self { selected: projects.first().cloned(), projects, loading }
    }

    pub fn selected(&self) -> Option<&PathBuf> {
        self.selected.as_ref()
    }

    /// Fill in projects once the session scan finishes.
    pub fn set_projects(&mut self, projects: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.loading = false;
        for dir in projects {
            if !self.projects.contains(&dir) {
                self.projects.push(dir);
            }
        }
        if self.selected.is_none() {
            self.selected = self.projects.first().cloned();
            cx.emit(Picked);
        }
        cx.notify();
    }

    fn select(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        self.projects.retain(|p| *p != dir);
        self.projects.insert(0, dir.clone());
        self.selected = Some(dir);
        cx.emit(Picked);
        cx.notify();
    }

    fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Выбрать проект".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(mut picked))) = paths.await else { return };
            let Some(dir) = picked.pop() else { return };
            this.update(cx, |this, cx| this.select(dir, cx)).ok();
        })
        .detach();
    }
}

impl Render for ProjectPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let p = Palette::of(&theme);
        v_flex()
            .gap_2()
            .when(self.loading && self.projects.is_empty(), |this| this.child(crate::loading::chips(&theme)))
            .child(div().flex().flex_wrap().gap(px(6.)).children(self.projects.iter().take(14).map(|dir| {
                let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let active = self.selected.as_ref() == Some(dir);
                let target = dir.clone();
                let tip = dir.display().to_string();
                div()
                    .id(SharedString::from(format!("project:{}", dir.display())))
                    .h(px(28.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .text_size(px(13.))
                    .cursor_pointer()
                    .map(|this| {
                        if active {
                            this.bg(p.ink).border_color(p.ink).text_color(theme.background).font_weight(FontWeight::SEMIBOLD)
                        } else {
                            this.bg(p.surface)
                                .border_color(p.border)
                                .text_color(p.fg)
                                .shadow(ui::edge_shadow(p.border, 1.))
                                .hover(|s| s.border_color(p.border_strong))
                        }
                    })
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = Some(target.clone());
                        cx.emit(Picked);
                        cx.notify();
                    }))
                    .child(name)
            })))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        PressButton::new("pick-folder")
                            .small()
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .label("Другая папка…")
                            .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
                    )
                    .when_some(self.selected.clone(), |this, dir| {
                        this.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme.mono_font_family.clone())
                                .text_size(px(11.))
                                .text_color(p.faint)
                                .child(dir.display().to_string()),
                        )
                    }),
            )
    }
}
