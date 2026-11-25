//! Images that take exactly their own space: the size comes from the file
//! header, so the box is laid out before the image decodes and never spills
//! over the content below it.

use std::path::{Path, PathBuf};

use gpui_kit::*;

/// Logical size of an image file. Screenshots and rendered diagrams are 2x.
pub fn logical_size(path: &Path) -> Option<(f32, f32)> {
    let s = imagesize::size(path).ok()?;
    (s.width > 0 && s.height > 0).then(|| (s.width as f32 / 2., s.height as f32 / 2.))
}

/// Widest the image may be: its own width, `max_w`, and the width at which it is `max_h` tall.
pub fn cap_width((w, h): (f32, f32), max_w: f32, max_h: f32) -> f32 {
    w.min(max_w).min(max_h * w / h)
}

/// An image box with the image's proportions, as wide as its cap (narrower if
/// the parent is); the image itself is laid absolutely inside it.
pub fn picture(id: impl Into<ElementId>, path: PathBuf, max_w: f32, max_h: f32) -> Stateful<Div> {
    let shell = div().id(id).relative().flex_none().overflow_hidden().max_w_full();
    let shell = match logical_size(&path) {
        Some(size) => shell.w(px(cap_width(size, max_w, max_h))).aspect_ratio(size.0 / size.1),
        None => shell.w(px(max_w.min(360.))).h(px(max_h.min(240.))),
    };
    shell.child(
        img(path)
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .object_fit(ObjectFit::Contain)
            .with_fallback(|| div().size_full().flex().items_center().justify_center().text_xs().child("нет файла").into_any_element()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[core::prelude::v1::test]
    fn caps_keep_proportions() {
        assert_eq!(cap_width((1200., 600.), 900., 900.), 900.);
        assert_eq!(cap_width((400., 1600.), 900., 400.), 100.);
        assert_eq!(cap_width((300., 200.), 900., 900.), 300.);
    }
}
