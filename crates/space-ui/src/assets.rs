//! Icon assets: gpui-component's default icon set plus the few extra Lucide icons Space uses.

use std::borrow::Cow;

use gpui::assets::{Assets, icon_assets};
use gpui::{AssetSource, Result, SharedString};

icon_assets!(
    ExtraIcons,
    [GitBranch, CircleStop, Brain, Wrench, Pencil, ListChecks, Terminal, SquarePen, Shield, ShieldCheck, ShieldHalf, ShieldAlert, KeyRound, CirclePause, Database, Copy, Check, Clock, SquarePlus, SquareMinus, Search, Gauge]
);

pub struct SpaceAssets;

impl AssetSource for SpaceAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match ExtraIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all = Assets.list(path)?;
        all.extend(ExtraIcons.list(path)?);
        all.sort();
        all.dedup();
        Ok(all)
    }
}
