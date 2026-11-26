//! Lucide icons beyond gpui-kit's default bundle, served next to it.

use std::borrow::Cow;

use gpui_kit::*;

gpui_kit::assets::icon_assets!(ExtraIcons, [CircleQuestionMark, ShieldAlert, MessageCircle, Paperclip, Link, Zap, Ellipsis, ListChecks, Pin, Archive, ArchiveRestore, Component, SquareFunction, Anchor, Braces, Settings2, FlaskConical, Globe, Database, Palette, Terminal, Shapes, Scan, Route, Footprints, Image, Video, Hand, Brain, Trash, Pencil, ScrollText, Rocket, AppWindow, MessageSquare, PanelBottom, PanelsTopLeft, PanelRight, ExternalLink, FileCode, GitBranch, MessageSquarePlus, Reply, Send, ChartColumn]);

/// The app's own picture (sidebar mark); the Dock icon is set in `main`.
pub const LOGO: &str = "app/logo.png";
pub const ICON_512: &[u8] = include_bytes!("../assets/icon/icon-512.png");

/// Instrument Sans, the interface face, embedded so every machine has it.
pub const FONTS: [&[u8]; 8] = [
    include_bytes!("../assets/fonts/InstrumentSans-Regular.ttf"),
    include_bytes!("../assets/fonts/InstrumentSans-Italic.ttf"),
    include_bytes!("../assets/fonts/InstrumentSans-Medium.ttf"),
    include_bytes!("../assets/fonts/InstrumentSans-MediumItalic.ttf"),
    include_bytes!("../assets/fonts/InstrumentSans-SemiBold.ttf"),
    include_bytes!("../assets/fonts/InstrumentSans-SemiBoldItalic.ttf"),
    include_bytes!("../assets/fonts/InstrumentSans-Bold.ttf"),
    include_bytes!("../assets/fonts/InstrumentSans-BoldItalic.ttf"),
];

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(include_bytes!("../assets/icon/logo.png"))));
        }
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
