//! «Контракты»: what the change does to the surface other code relies on —
//! types and their fields, signatures before → after, storage, dependencies
//! (manifests and lockfiles) and which packages import which.

use std::collections::{BTreeMap, BTreeSet};

use gpui_kit::component::{Theme, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use stepan_verify_core::arch::{Arch, Status};

use super::{
    ChangesView, Dto, Entry, Report,
    arch::{self, package_prefix, short_package},
};
use crate::{
    ui::{self, Palette, Tone},
    zoom::px,
};

pub fn gray(p: &Palette) -> Tone {
    Tone { solid: p.border_strong, soft: p.sunken, text: p.muted }
}

fn status_tone(s: &str, p: &Palette) -> Tone {
    match s {
        "added" => p.green,
        "removed" => p.red,
        "changed" => p.amber,
        _ => gray(p),
    }
}

fn status_word(s: &str) -> &'static str {
    match s {
        "added" => "новый",
        "removed" => "удалён",
        "changed" => "изменён",
        _ => "был",
    }
}

fn is_storage(kind: &str) -> bool {
    matches!(kind, "table" | "schema" | "migration" | "db" | "collection")
}

fn is_type(e: &Entry) -> bool {
    matches!(e.kind.as_str(), "type" | "dto") || matches!(e.role.as_str(), "type" | "dto")
}

impl ChangesView {
    pub(super) fn render_contracts(&self, r: &Report, p: &Palette, theme: &Theme) -> AnyElement {
        let g = self.arch_graph();
        let a = g.as_ref().map(|g| &g.arch);
        let mono = theme.mono_font_family.clone();

        // Types: the reviewer's DTOs plus type entries it did not describe as DTOs.
        let dtos: Vec<&Dto> = r.dtos.iter().filter(|d| d.status != "context" || d.fields.iter().any(|f| !f.status.is_empty() && f.status != "context")).collect();
        let (storage, types): (Vec<&Dto>, Vec<&Dto>) = dtos.into_iter().partition(|d| is_storage(&d.kind));
        let named: BTreeSet<String> = r.dtos.iter().map(|d| d.name.clone()).collect();
        let lists = [("added", &r.code.added), ("changed", &r.code.changed), ("removed", &r.code.removed)];
        let type_entries: Vec<(&str, &Entry)> = lists.iter().flat_map(|(s, l)| l.iter().filter(|e| is_type(e) && !named.contains(&e.name)).map(move |e| (*s, e))).collect();
        let table_entries: Vec<(&str, &Entry)> = lists.iter().flat_map(|(s, l)| l.iter().filter(|e| is_storage(&e.kind)).map(move |e| (*s, e))).collect();
        let sigs: Vec<(&str, &Entry)> = lists
            .iter()
            .flat_map(|(s, l)| l.iter().filter(|e| !is_type(e) && !is_storage(&e.kind) && (!e.signature.is_empty() || *s == "removed")).map(move |e| (*s, e)))
            .collect();
        let added_types = types.iter().filter(|d| d.status == "added").count() + type_entries.iter().filter(|(s, _)| *s == "added").count();
        let changed_types = types.iter().filter(|d| d.status == "changed").count() + type_entries.iter().filter(|(s, _)| *s == "changed").count();
        let removed_types = types.iter().filter(|d| d.status == "removed").count() + type_entries.iter().filter(|(s, _)| *s == "removed").count();

        let chip = |text: String, t: Tone| ui::badge(text, t).text_size(px(12.)).px(px(8.)).py(px(2.));
        let mut chips = vec![
            chip(format!("+{added_types} типов"), p.green),
            chip(format!("~{changed_types} изменено"), p.amber),
            chip(format!("−{removed_types} удалено"), if removed_types > 0 { p.red } else { gray(p) }),
            chip(format!("{} сигнатур", sigs.len()), gray(p)),
        ];
        if let Some(a) = a {
            let added = a.deps.iter().filter(|d| d.before.is_none()).count();
            let removed = a.deps.iter().filter(|d| d.after.is_none()).count();
            let bumped = a.deps.len() - added - removed;
            chips.push(chip(format!("пакеты +{added} −{removed} ~{bumped}"), if added + removed + bumped > 0 { p.blue } else { gray(p) }));
            let trans: usize = a.locks.iter().map(|l| l.added.len()).sum();
            if trans > 0 {
                chips.push(chip(format!("+{trans} в lock-файле"), p.blue));
            }
            let links = arch::package_links(a);
            chips.push(chip(format!("+{} связей между пакетами", links.added.len()), if links.added.is_empty() { gray(p) } else { p.green }));
            if !links.cycles.is_empty() {
                chips.push(chip(format!("{} циклов", links.cycles.len()), p.red));
            }
        }

        let section = |title: &'static str, hint: String| {
            h_flex()
                .mt(px(10.))
                .gap(px(10.))
                .items_baseline()
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(title))
                .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(hint))
        };

        let mut body = v_flex().gap(px(12.)).child(h_flex().gap(px(8.)).flex_wrap().children(chips));

        if !types.is_empty() || !type_entries.is_empty() {
            body = body
                .child(section("Типы", "поля: + новое · ~ изменено · − убрано".into()))
                .child(h_flex().gap(px(12.)).flex_wrap().items_start().children(types.iter().map(|d| dto_card(d, p, theme))).children(type_entries.iter().map(|(s, e)| entry_card(s, e, p, theme))));
        }
        if !sigs.is_empty() {
            body = body.child(section("Сигнатуры", "функции, компоненты, эндпоинты — интерфейс было → стало".into())).child(
                ui::card(p).children(sigs.iter().take(60).enumerate().map(|(i, (s, e))| {
                    let t = status_tone(s, p);
                    v_flex()
                        .px(px(14.))
                        .py(px(8.))
                        .when(i > 0, |d| d.border_t_1().border_color(p.border))
                        .child(
                            h_flex()
                                .gap(px(8.))
                                .child(ui::badge(status_word(s), t).text_size(px(10.5)))
                                .child(div().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(if e.entity.is_empty() { e.name.clone() } else { e.entity.clone() }))
                                .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(e.file.clone())),
                        )
                        .when(!e.signature_before.is_empty(), |d| d.child(div().font_family(mono.clone()).text_size(px(11.5)).text_color(p.red.text).child(format!("− {}", e.signature_before))))
                        .when(!e.signature.is_empty(), |d| {
                            let changed = !e.signature_before.is_empty();
                            d.child(div().font_family(mono.clone()).text_size(px(11.5)).text_color(if changed { p.green.text } else if *s == "removed" { p.red.text } else { p.fg }).child(if changed { format!("+ {}", e.signature) } else { e.signature.clone() }))
                        })
                })),
            );
        }
        if !storage.is_empty() || !table_entries.is_empty() {
            body = body
                .child(section("Хранилище", "таблицы, схемы, миграции".into()))
                .child(h_flex().gap(px(12.)).flex_wrap().items_start().children(storage.iter().map(|d| dto_card(d, p, theme))).children(table_entries.iter().map(|(s, e)| entry_card(s, e, p, theme))));
        }
        if let Some(a) = a {
            body = body.child(self.render_deps(a, r, p, theme)).child(render_matrix(a, p, theme));
        }
        // What is gone.
        let removed: Vec<String> = r
            .code
            .removed
            .iter()
            .map(|e| format!("{} · {}", if e.entity.is_empty() { &e.name } else { &e.entity }, e.file))
            .chain(r.dtos.iter().filter(|d| d.status == "removed").map(|d| format!("тип {}", d.name)))
            .chain(r.dtos.iter().flat_map(|d| d.fields.iter().filter(|f| f.status == "removed").map(move |f| format!("{}.{}", d.name, f.name))))
            .chain(a.into_iter().flat_map(|a| a.modules.iter().filter(|m| m.status == Status::Removed && !m.external).map(|m| format!("модуль {}", m.id))))
            .collect();
        body = body.child(section("Удалено", if removed.is_empty() { "ничего".into() } else { format!("{}", removed.len()) }));
        if removed.is_empty() {
            body = body.child(div().text_size(px(12.5)).text_color(p.muted).child("Из публичной поверхности ничего не убрано."));
        } else {
            body = body.child(ui::card(p).p(px(12.)).gap(px(4.)).children(removed.into_iter().map(|t| div().font_family(mono.clone()).text_size(px(12.)).text_color(p.red.text).child(format!("− {t}")))));
        }
        body.into_any_element()
    }

    fn render_deps(&self, a: &Arch, r: &Report, p: &Palette, theme: &Theme) -> impl IntoElement {
        let mono = theme.mono_font_family.clone();
        let why = |name: &str| r.deps.iter().find(|d| d.name == name || d.name.replace('-', "_") == name.replace('-', "_")).map(|d| d.why.clone()).unwrap_or_default();
        // Lockfile: a version bump is one name removed and added again.
        let mut bumps: Vec<String> = Vec::new();
        let mut added: Vec<String> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        for l in &a.locks {
            let old: BTreeMap<&str, &str> = l.removed.iter().filter_map(|x| x.split_once(' ')).collect();
            let new: BTreeMap<&str, &str> = l.added.iter().filter_map(|x| x.split_once(' ')).collect();
            for (n, v) in &new {
                match old.get(n) {
                    Some(o) => bumps.push(format!("{n} {o} → {v}")),
                    None => added.push(format!("{n} {v}")),
                }
            }
            removed.extend(old.iter().filter(|(n, _)| !new.contains_key(*n)).map(|(n, v)| format!("{n} {v}")));
        }
        v_flex()
            .gap(px(10.))
            .child(
                h_flex()
                    .mt(px(10.))
                    .gap(px(10.))
                    .items_baseline()
                    .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("Зависимости"))
                    .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child("манифесты и lock-файлы, посчитано из файлов")),
            )
            .child(if a.deps.is_empty() && added.is_empty() && removed.is_empty() && bumps.is_empty() {
                div().text_size(px(12.5)).text_color(p.muted).child("Зависимости не менялись.").into_any_element()
            } else {
                ui::card(p)
                    .children(a.deps.iter().enumerate().map(|(i, d)| {
                        let (word, t) = match (&d.before, &d.after) {
                            (None, _) => ("новый", p.green),
                            (_, None) => ("убран", p.red),
                            _ => ("обновлён", p.amber),
                        };
                        let w = why(&d.name);
                        v_flex()
                            .px(px(14.))
                            .py(px(9.))
                            .when(i > 0, |x| x.border_t_1().border_color(p.border))
                            .child(
                                h_flex()
                                    .gap(px(8.))
                                    .child(ui::badge(word, t).text_size(px(10.5)))
                                    .child(div().font_family(mono.clone()).text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(d.name.clone()))
                                    .child(div().font_family(mono.clone()).text_size(px(11.5)).text_color(p.muted).child(format!("{} → {}", d.before.as_deref().unwrap_or("—"), d.after.as_deref().unwrap_or("—"))))
                                    .child(div().flex_1().min_w_0().truncate().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child(format!("{} [{}]", d.manifest, d.section))),
                            )
                            .when(!w.is_empty(), |x| x.child(div().mt(px(2.)).text_size(px(12.5)).text_color(p.fg).child(w)))
                    }))
                    .when(!added.is_empty() || !removed.is_empty() || !bumps.is_empty(), |c| {
                        c.child(
                            v_flex()
                                .px(px(14.))
                                .py(px(9.))
                                .gap(px(3.))
                                .border_t_1()
                                .border_color(p.border)
                                .bg(p.surface2)
                                .child(div().text_size(px(12.)).text_color(p.muted).child("В lock-файле (с транзитивными):"))
                                .when(!added.is_empty(), |x| x.child(div().font_family(mono.clone()).text_size(px(11.5)).text_color(p.green.text).child(format!("+ {}", added.join(", ")))))
                                .when(!bumps.is_empty(), |x| x.child(div().font_family(mono.clone()).text_size(px(11.5)).text_color(p.amber.text).child(format!("~ {}", bumps.join(", ")))))
                                .when(!removed.is_empty(), |x| x.child(div().font_family(mono.clone()).text_size(px(11.5)).text_color(p.red.text).child(format!("− {}", removed.join(", "))))),
                        )
                    })
                    .into_any_element()
            })
    }
}

fn field_row(sign: &'static str, name: String, ty: String, t: Option<Tone>, p: &Palette, theme: &Theme) -> impl IntoElement {
    h_flex()
        .gap(px(6.))
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .font_family(theme.mono_font_family.clone())
        .text_size(px(11.5))
        .when_some(t, |d, t| d.bg(t.soft))
        .child(div().w(px(8.)).text_color(t.map_or(p.faint, |t| t.text)).child(sign))
        .child(div().text_color(p.ink).child(name))
        .child(div().min_w_0().truncate().text_color(p.muted).child(ty))
}

/// A signature that wraps inside the card instead of running out of it.
fn signature(sign: &'static str, text: &str, t: Option<Tone>, p: &Palette, theme: &Theme) -> impl IntoElement {
    h_flex()
        .items_start()
        .gap(px(6.))
        .px(px(6.))
        .py(px(2.))
        .rounded(px(4.))
        .font_family(theme.mono_font_family.clone())
        .text_size(px(11.5))
        .when_some(t, |d, t| d.bg(t.soft))
        .child(div().w(px(8.)).flex_shrink_0().text_color(t.map_or(p.faint, |t| t.text)).child(sign))
        .child(div().flex_1().min_w_0().text_color(p.ink).child(text.to_owned()))
}

/// Types the tab shows: the reviewer's DTOs and type entries it did not describe as DTOs.
pub fn type_count(r: &Report) -> usize {
    let named: BTreeSet<&str> = r.dtos.iter().map(|d| d.name.as_str()).collect();
    let dtos = r.dtos.iter().filter(|d| !is_storage(&d.kind) && (d.status != "context" || d.fields.iter().any(|f| !f.status.is_empty() && f.status != "context"))).count();
    let entries = [&r.code.added, &r.code.changed, &r.code.removed].into_iter().flatten().filter(|e| is_type(e) && !named.contains(e.name.as_str())).count();
    dtos + entries
}

fn card_head(status: &str, name: String, kind: String, file: String, p: &Palette, theme: &Theme) -> impl IntoElement {
    v_flex()
        .child(
            h_flex()
                .gap(px(8.))
                .child(ui::badge(status_word(status), status_tone(status, p)).text_size(px(10.5)))
                .child(div().min_w_0().truncate().font_family(theme.mono_font_family.clone()).text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(name))
                .child(div().ml_auto().font_family(theme.mono_font_family.clone()).text_size(px(10.5)).text_color(p.faint).child(kind)),
        )
        .child(div().mb(px(6.)).truncate().font_family(theme.mono_font_family.clone()).text_size(px(10.5)).text_color(p.faint).child(file))
}

fn dto_card(d: &Dto, p: &Palette, theme: &Theme) -> impl IntoElement {
    ui::card(p)
        .w(px(300.))
        .p(px(12.))
        .gap(px(2.))
        .child(card_head(&d.status, d.name.clone(), d.kind.clone(), d.file.clone(), p, theme))
        .children(d.fields.iter().map(|f| {
            let (sign, t) = match f.status.as_str() {
                "added" => ("+", Some(p.green)),
                "removed" => ("−", Some(p.red)),
                "changed" => ("~", Some(p.amber)),
                _ if d.status == "added" => ("+", Some(p.green)),
                _ => (" ", None),
            };
            field_row(sign, f.name.clone(), f.ty.clone(), t, p, theme)
        }))
        .when(!d.note.is_empty(), |c| c.child(div().mt(px(6.)).text_size(px(12.)).text_color(p.muted).child(d.note.clone())))
}

fn entry_card(status: &str, e: &Entry, p: &Palette, theme: &Theme) -> impl IntoElement {
    ui::card(p)
        .w(px(300.))
        .p(px(12.))
        .gap(px(2.))
        .child(card_head(status, e.name.clone(), if e.kind.is_empty() { e.role.clone() } else { e.kind.clone() }, e.file.clone(), p, theme))
        .when(!e.signature_before.is_empty(), |c| c.child(signature("−", &e.signature_before, Some(p.red), p, theme)))
        .when(!e.signature.is_empty(), |c| c.child(signature(if e.signature_before.is_empty() { " " } else { "+" }, &e.signature, (!e.signature_before.is_empty()).then_some(p.green), p, theme)))
        .when(!e.note.is_empty(), |c| c.child(div().mt(px(6.)).text_size(px(12.)).text_color(p.muted).child(e.note.clone())))
}

/// Which package imports which: rows import columns; each cell counts module links.
fn render_matrix(a: &Arch, p: &Palette, theme: &Theme) -> impl IntoElement {
    let prefix = package_prefix(a);
    let pkg: BTreeMap<&str, &str> = a.modules.iter().map(|m| (m.id.as_str(), m.package.as_str())).collect();
    let rows: BTreeSet<&str> = a.modules.iter().filter(|m| !m.external).map(|m| m.package.as_str()).collect();
    let cols: Vec<&str> = rows.iter().copied().chain(a.modules.iter().filter(|m| m.external).map(|m| m.package.as_str()).collect::<BTreeSet<_>>()).collect();
    let mut cell: BTreeMap<(&str, &str), (usize, usize, usize)> = BTreeMap::new();
    for e in &a.edges {
        let (Some(f), Some(t)) = (pkg.get(e.from.as_str()), pkg.get(e.to.as_str())) else { continue };
        let c = cell.entry((f, t)).or_default();
        match e.status {
            Status::Added => c.1 += 1,
            Status::Removed => c.2 += 1,
            _ => c.0 += 1,
        }
    }
    let mono = theme.mono_font_family.clone();
    let w = 120.;
    let head = h_flex()
        .px(px(12.))
        .py(px(7.))
        .border_b_1()
        .border_color(p.border)
        .child(div().w(px(150.)).flex_shrink_0())
        .children(cols.iter().map(|c| div().w(px(w)).flex_shrink_0().truncate().font_family(mono.clone()).text_size(px(10.5)).text_color(p.faint).child(short_package(c, &prefix).to_owned())));
    let body = rows.iter().map(|r| {
        h_flex()
            .px(px(12.))
            .py(px(7.))
            .border_b_1()
            .border_color(p.surface2)
            .child(div().w(px(150.)).flex_shrink_0().truncate().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child(short_package(r, &prefix).to_owned()))
            .children(cols.iter().map(|c| {
                let (same, add, rem) = cell.get(&(*r, *c)).copied().unwrap_or_default();
                let text = if r == c {
                    "·".to_owned()
                } else if same + add + rem == 0 {
                    "—".to_owned()
                } else {
                    let mut parts = Vec::new();
                    if same > 0 {
                        parts.push(format!("{same}"));
                    }
                    if add > 0 {
                        parts.push(format!("+{add}"));
                    }
                    if rem > 0 {
                        parts.push(format!("−{rem}"));
                    }
                    parts.join(" ")
                };
                let color = if add > 0 && same == 0 && r != c {
                    p.green.text
                } else if rem > 0 && same == 0 && add == 0 {
                    p.red.text
                } else if same + add + rem == 0 || r == c {
                    p.faint
                } else {
                    p.fg
                };
                div().w(px(w)).flex_shrink_0().font_family(mono.clone()).text_size(px(12.)).text_color(color).when(add > 0 && same == 0 && r != c, |d| d.font_weight(FontWeight::SEMIBOLD)).child(text)
            }))
    });
    v_flex()
        .gap(px(10.))
        .child(
            h_flex()
                .mt(px(10.))
                .gap(px(10.))
                .items_baseline()
                .child(div().text_size(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(p.ink).child("Импорты между пакетами"))
                .child(div().font_family(mono.clone()).text_size(px(11.)).text_color(p.faint).child("строка импортирует столбец · число связей, +новые, −убранные")),
        )
        .child(div().id("contracts-matrix").overflow_x_scroll().child(ui::card(p).child(head).children(body)))
}
