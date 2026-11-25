//! Lucide icons beyond gpui-kit's default bundle, served next to it.

use std::borrow::Cow;

use gpui_kit::*;

gpui_kit::assets::icon_assets!(ExtraIcons, [CircleQuestionMark, ShieldAlert, MessageCircle, Paperclip, Link, Zap, Ellipsis, ListChecks, Pin]);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = ExtraIcons.list(path)?;
        all.extend(assets::Assets.list(path)?);
        Ok(all)
    }
}
