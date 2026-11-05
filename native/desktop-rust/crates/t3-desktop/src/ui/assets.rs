//! Icons, embedded in the binary. They are Lucide icons (ISC license), drawn as masks in the
//! current text color.

use std::borrow::Cow;

use gpui::{AssetSource, SharedString};

macro_rules! icons {
    ($($variant:ident => $name:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Icon {
            $($variant,)*
        }

        impl Icon {
            pub const ALL: &[Icon] = &[$(Icon::$variant,)*];

            pub fn path(self) -> &'static str {
                match self {
                    $(Icon::$variant => concat!("icons/", $name, ".svg"),)*
                }
            }

            fn bytes(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => include_bytes!(concat!("../../assets/icons/", $name, ".svg")),)*
                }
            }
        }
    };
}

icons! {
    ArrowLeft => "arrow-left",
    ArrowRight => "arrow-right",
    ArrowUp => "arrow-up",
    ChartColumn => "chart-column",
    Check => "check",
    ChevronDown => "chevron-down",
    ChevronRight => "chevron-right",
    Circle => "circle",
    CircleAlert => "circle-alert",
    CircleCheck => "circle-check",
    CircleHelp => "circle-help",
    FolderPlus => "folder-plus",
    GitBranch => "git-branch",
    GitPullRequest => "git-pull-request",
    Hammer => "hammer",
    Lightbulb => "lightbulb",
    ListFilter => "list-filter",
    ListTodo => "list-todo",
    LoaderCircle => "loader-circle",
    PanelLeft => "panel-left",
    Pencil => "pencil",
    Plus => "plus",
    Search => "search",
    Server => "server",
    Settings => "settings",
    ShieldAlert => "shield-alert",
    Square => "square",
    SquarePen => "square-pen",
    Terminal => "terminal",
    X => "x",
}

/// Serves the embedded icons to GPUI.
#[derive(Debug)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Icon::ALL
            .iter()
            .find(|icon| icon.path() == path)
            .map(|icon| Cow::Borrowed(icon.bytes())))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .iter()
            .map(|icon| icon.path())
            .filter(|candidate| candidate.starts_with(path))
            .map(SharedString::from)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_loadable_svg() {
        for icon in Icon::ALL {
            let bytes = Assets.load(icon.path()).unwrap().unwrap();
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(
                text.starts_with("<svg") && text.trim_end().ends_with("</svg>"),
                "{icon:?}"
            );
        }
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
        assert_eq!(Assets.list("icons/").unwrap().len(), Icon::ALL.len());
    }
}
