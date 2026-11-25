//! Placeholders shown while a surface waits for data. Each one mirrors the
//! shape of the real content so nothing jumps when it arrives.

use gpui_kit::component::{
    Sizable as _, Theme,
    shimmer::ShimmerText,
    skeleton::Skeleton,
    spinner::Spinner,
    h_flex, v_flex,
};
use gpui_kit::*;
use crate::zoom::px;

/// Conversation shape: user bubble, answer lines, a tool card, repeated.
pub fn transcript(theme: &Theme) -> impl IntoElement {
    let answer = |widths: &[f32]| {
        v_flex()
            .gap_2()
            .children(widths.iter().map(|w| Skeleton::new().w(relative(*w)).h_4().rounded(theme.radius)))
    };
    let bubble = |w: f32| {
        h_flex()
            .justify_end()
            .child(Skeleton::new().secondary().w(relative(w)).h(px(44.)).rounded(theme.radius_lg))
    };
    let tool = || {
        h_flex()
            .px_2()
            .py_2()
            .gap_2()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .child(Skeleton::new().size_3().rounded_full())
            .child(Skeleton::new().secondary().w(relative(0.4)).h_3().rounded(theme.radius))
    };
    v_flex()
        .size_full()
        .px_4()
        .py_4()
        .gap_6()
        .child(bubble(0.45))
        .child(answer(&[0.92, 0.85, 0.6]))
        .child(tool())
        .child(answer(&[0.88, 0.7]))
        .child(bubble(0.3))
        .child(tool())
        .child(answer(&[0.9, 0.95, 0.8, 0.4]))
}

/// Project chips in the new-session form.
pub fn chips(theme: &Theme) -> impl IntoElement {
    const WIDTHS: [f32; 7] = [56., 96., 120., 72., 104., 64., 88.];
    div()
        .flex()
        .flex_wrap()
        .gap_2()
        .children(WIDTHS.iter().map(|w| Skeleton::new().w(px(*w)).h(px(28.)).rounded(theme.radius)))
}

/// "Agent is working" line: spinner, shimmering activity, elapsed time.
pub fn working(id: &'static str, activity: String, elapsed_secs: u64, theme: &Theme) -> impl IntoElement {
    h_flex()
        .gap_2()
        .items_center()
        .text_xs()
        .child(Spinner::new().xsmall().color(theme.muted_foreground))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(ShimmerText::new(activity).id(id).highlight_color(theme.foreground)),
        )
        .child(div().flex_shrink_0().text_color(theme.muted_foreground).child(elapsed(elapsed_secs)))
}

fn elapsed(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} с"),
        _ => format!("{}:{:02}", secs / 60, secs % 60),
    }
}

#[cfg(test)]
mod tests {
    #[core::prelude::v1::test]
    fn elapsed_format() {
        assert_eq!(super::elapsed(7), "7 с");
        assert_eq!(super::elapsed(125), "2:05");
    }
}
