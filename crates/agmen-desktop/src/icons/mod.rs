//! UI icons (Lucide) and file icons (the Zed icon theme, or the OS).

mod system;
mod zed_theme;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    AnyElement, Global, Hsla, Image, ImageFormat, IntoElement as _, Pixels, Styled as _, Svg, img,
    svg,
};

pub(crate) use zed_theme::IconTheme;

use crate::appearance::IconThemeChoice;

/// Where one file's icon comes from.
#[derive(Debug, Clone)]
pub(crate) enum FileIcon {
    /// An SVG from the person's Zed icon theme, drawn in its own colors.
    Svg(PathBuf),
    /// The operating system's icon.
    Image(Arc<Image>),
    /// agmen's Lucide fallback.
    Ui(UiIcon),
}

impl FileIcon {
    pub(crate) fn render(&self, color: Hsla, size: Pixels) -> AnyElement {
        match self {
            Self::Svg(path) => img(path.clone()).size(size).flex_none().into_any_element(),
            Self::Image(image) => img(image.clone()).size(size).flex_none().into_any_element(),
            Self::Ui(icon) => icon.render(color, size).into_any_element(),
        }
    }
}

/// Resolves file and folder icons: the Zed icon theme, then the OS, then
/// Lucide. Held as a GPUI global; OS icons are cached by file type.
#[derive(Debug)]
pub(crate) struct FileIcons {
    theme: Option<IconTheme>,
    system: bool,
    cache: RefCell<HashMap<String, Option<Arc<Image>>>>,
}

impl Global for FileIcons {}

/// OS icons are cached per file type; past this many types, they are not.
const MAX_CACHED_TYPES: usize = 512;

impl FileIcons {
    /// Builds the resolver for `choice`; a Zed theme that is not installed
    /// falls back to the OS icons and returns a warning.
    pub(crate) fn new(choice: &IconThemeChoice, extensions_dir: &Path) -> (Self, Option<String>) {
        let (theme, system, warning) = match choice {
            IconThemeChoice::Zed(name) => match IconTheme::load(extensions_dir, name) {
                Ok(theme) => (Some(theme), true, None),
                Err(err) => (
                    None,
                    true,
                    Some(format!("{err}; using the system's file icons")),
                ),
            },
            IconThemeChoice::System => (None, true, None),
            IconThemeChoice::Lucide => (None, false, None),
        };
        let icons = Self {
            theme,
            system,
            cache: RefCell::new(HashMap::new()),
        };
        (icons, warning)
    }

    /// The icon for the file or folder at `path`.
    pub(crate) fn icon(&self, path: &Path, is_dir: bool, open: bool) -> FileIcon {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let themed = self.theme.as_ref().and_then(|theme| {
            if is_dir {
                theme.folder(name, open)
            } else {
                theme.file(name)
            }
        });
        if let Some(svg) = themed {
            return FileIcon::Svg(svg);
        }
        if self.system
            && let Some(image) = self.system_icon(path, is_dir)
        {
            return FileIcon::Image(image);
        }
        FileIcon::Ui(match (is_dir, open) {
            (true, true) => UiIcon::FolderOpen,
            (true, false) => UiIcon::Folder,
            (false, _) => UiIcon::File,
        })
    }

    fn system_icon(&self, path: &Path, is_dir: bool) -> Option<Arc<Image>> {
        let key = if is_dir {
            "/folder".to_owned()
        } else {
            path.extension()
                .and_then(|e| e.to_str())
                .map_or_else(|| "/none".to_owned(), str::to_ascii_lowercase)
        };
        if let Some(hit) = self.cache.borrow().get(&key) {
            return hit.clone();
        }
        let image = system::icon_tiff(path)
            .map(|tiff| Arc::new(Image::from_bytes(ImageFormat::Tiff, tiff)));
        let mut cache = self.cache.borrow_mut();
        if cache.len() < MAX_CACHED_TYPES {
            cache.insert(key, image.clone());
        }
        image
    }
}

/// agmen's UI icons, all Lucide (ISC).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the views of Tasks 7 to 19 in the desktop plan use the rest"
    )
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum UiIcon {
    Agents,
    Project,
    Git,
    Roles,
    Search,
    Terminal,
    Chat,
    Bell,
    Settings,
    ChevronDown,
    ChevronRight,
    ChevronLeft,
    Sparkle,
    Diff,
    Pencil,
    Close,
    Check,
    Plus,
    Minus,
    Enter,
    Alert,
    PanelRight,
    File,
    Folder,
    FolderOpen,
}

impl UiIcon {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 25] = [
        Self::Agents,
        Self::Project,
        Self::Git,
        Self::Roles,
        Self::Search,
        Self::Terminal,
        Self::Chat,
        Self::Bell,
        Self::Settings,
        Self::ChevronDown,
        Self::ChevronRight,
        Self::ChevronLeft,
        Self::Sparkle,
        Self::Diff,
        Self::Pencil,
        Self::Close,
        Self::Check,
        Self::Plus,
        Self::Minus,
        Self::Enter,
        Self::Alert,
        Self::PanelRight,
        Self::File,
        Self::Folder,
        Self::FolderOpen,
    ];

    /// The asset path, under `assets/`.
    pub(crate) const fn path(self) -> &'static str {
        match self {
            Self::Agents => "icons/lucide/users.svg",
            Self::Project => "icons/lucide/folder-tree.svg",
            Self::Git => "icons/lucide/git-branch.svg",
            Self::Roles => "icons/lucide/user.svg",
            Self::Search => "icons/lucide/search.svg",
            Self::Terminal => "icons/lucide/square-terminal.svg",
            Self::Chat => "icons/lucide/messages-square.svg",
            Self::Bell => "icons/lucide/bell.svg",
            Self::Settings => "icons/lucide/settings.svg",
            Self::ChevronDown => "icons/lucide/chevron-down.svg",
            Self::ChevronRight => "icons/lucide/chevron-right.svg",
            Self::ChevronLeft => "icons/lucide/chevron-left.svg",
            Self::Sparkle => "icons/lucide/sparkle.svg",
            Self::Diff => "icons/lucide/diff.svg",
            Self::Pencil => "icons/lucide/pencil.svg",
            Self::Close => "icons/lucide/x.svg",
            Self::Check => "icons/lucide/check.svg",
            Self::Plus => "icons/lucide/plus.svg",
            Self::Minus => "icons/lucide/minus.svg",
            Self::Enter => "icons/lucide/corner-down-left.svg",
            Self::Alert => "icons/lucide/circle-alert.svg",
            Self::PanelRight => "icons/lucide/panel-right.svg",
            Self::File => "icons/lucide/file.svg",
            Self::Folder => "icons/lucide/folder.svg",
            Self::FolderOpen => "icons/lucide/folder-open.svg",
        }
    }

    /// The icon tinted `color` at `size` square.
    pub(crate) fn render(self, color: Hsla, size: Pixels) -> Svg {
        svg()
            .path(self.path())
            .size(size)
            .flex_none()
            .text_color(color)
    }
}

#[cfg(test)]
#[path = "zed_theme_tests.rs"]
mod tests;
