//! Component previews (dev only). Each component module declares named example states next
//! to its code in a `previews()` function; [`registry`] lists those functions. The gallery
//! renders every example through the component's real code with fixture data.
//!
//! Adding a component: write `#[cfg(feature = "dev")] pub fn previews() -> Vec<ComponentPreview>`
//! in its module and add the call to [`registry`].

use std::rc::Rc;

use gpui::{AnyElement, App, Pixels, SharedString, Window};

/// Gallery sidebar groups, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Area {
    Shell,
    Sidebar,
    Composer,
    Transcript,
    Trajectory,
    Settings,
    Indicators,
}

impl Area {
    pub const ALL: [Area; 7] =
        [Area::Shell, Area::Sidebar, Area::Composer, Area::Transcript, Area::Trajectory, Area::Settings, Area::Indicators];

    pub fn label(self) -> &'static str {
        match self {
            Area::Shell => "Shell",
            Area::Sidebar => "Sidebar",
            Area::Composer => "Composer",
            Area::Transcript => "Transcript",
            Area::Trajectory => "Trajectory",
            Area::Settings => "Settings",
            Area::Indicators => "Indicators",
        }
    }
}

pub type RenderFn = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

/// One named state of a component.
#[derive(Clone)]
pub struct Example {
    pub name: SharedString,
    /// Frame width in the gallery (`None`: as wide as the content wants).
    pub width: Option<Pixels>,
    pub render: RenderFn,
}

impl Example {
    pub fn new(name: impl Into<SharedString>, render: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self { name: name.into(), width: None, render: Rc::new(render) }
    }

    pub fn width(mut self, width: Pixels) -> Self {
        self.width = Some(width);
        self
    }
}

/// How the gallery arranges a component's examples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Side by side, wrapping (small components).
    Grid,
    /// One under another (wide components).
    Stack,
}

#[derive(Clone)]
pub struct ComponentPreview {
    pub area: Area,
    pub name: &'static str,
    pub description: &'static str,
    pub layout: Layout,
    pub examples: Vec<Example>,
}

impl ComponentPreview {
    pub fn new(area: Area, name: &'static str, description: &'static str) -> Self {
        Self { area, name, description, layout: Layout::Grid, examples: Vec::new() }
    }

    pub fn stacked(mut self) -> Self {
        self.layout = Layout::Stack;
        self
    }

    pub fn example(mut self, example: Example) -> Self {
        self.examples.push(example);
        self
    }

    pub fn examples(mut self, examples: impl IntoIterator<Item = Example>) -> Self {
        self.examples.extend(examples);
        self
    }
}

/// Every component preview, grouped by [`Area`] (stable order within an area).
pub fn registry() -> Vec<ComponentPreview> {
    use crate::components::*;
    let mut all: Vec<ComponentPreview> = [
        title_bar::previews(),
        shell::previews(),
        terminal::previews(),
        tip::previews(),
        settings::previews(),
        thread_row::previews(),
        composer::previews(),
        model_picker::previews(),
        transcript::previews(),
        images::previews(),
        message_actions::previews(),
        trajectory::previews(),
        follow_scroll::previews(),
        loaders::previews(),
    ]
    .concat();
    all.sort_by_key(|p| p.area);
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_well_formed() {
        let all = registry();
        assert!(all.len() >= 10, "{} previews", all.len());
        let mut seen = std::collections::HashSet::new();
        for p in &all {
            assert!(seen.insert((p.area, p.name)), "duplicate preview {}", p.name);
            assert!(!p.examples.is_empty(), "{} has no examples", p.name);
            let mut names = std::collections::HashSet::new();
            for e in &p.examples {
                assert!(names.insert(e.name.clone()), "{}: duplicate example {}", p.name, e.name);
            }
        }
        for area in Area::ALL {
            assert!(all.iter().any(|p| p.area == area), "no previews in {}", area.label());
        }
        assert!(all.windows(2).all(|w| w[0].area <= w[1].area), "sorted by area");
    }
}
