//! The title bar: app name, project, listener chip and my avatar.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, FontWeight, InteractiveElement as _, IntoElement, MouseButton, ParentElement as _,
    RenderOnce, SharedString, Styled as _, Window, WindowControlArea, div,
};

use super::components::{Avatar, Chip, Color, IconButton, IconButtonStyle, IconSize, Label};
use super::format::listener_label;
use super::tokens::{BORDER_WIDTH, SPACE_10, SPACE_12, TITLE_BAR_HEIGHT, TRAFFIC_LIGHTS_INSET};
use crate::backend::Snapshot;
use crate::icons::UiIcon;

/// Which platform's window chrome the title bar leaves room for or draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlatformStyle {
    /// Traffic lights at the left, drawn by macOS.
    Mac,
    /// Minimize, maximize and close at the right, drawn by agmen.
    Windows,
    /// As Windows, when the compositor leaves decorations to the app.
    Linux,
}

impl PlatformStyle {
    /// The style of the platform this build runs on.
    pub(crate) const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// Tests find the app name as `title-app`, the avatar as `title-avatar` and
/// the window buttons as `window-control:min`, `:max` and `:close`.
#[derive(Debug, Clone, IntoElement)]
pub(crate) struct TitleBar {
    project: SharedString,
    listener: SharedString,
    me: SharedString,
    platform: PlatformStyle,
}

impl TitleBar {
    #[must_use]
    pub(crate) fn new(snapshot: &Snapshot) -> Self {
        Self {
            project: snapshot.project.clone().into(),
            listener: listener_label(&snapshot.listener.agent, &snapshot.listener.state).into(),
            me: snapshot.me.clone().into(),
            platform: PlatformStyle::current(),
        }
    }

    /// Draws another platform's chrome, so tests on one OS can check all.
    #[cfg(test)]
    pub(crate) const fn platform(mut self, platform: PlatformStyle) -> Self {
        self.platform = platform;
        self
    }
}

impl RenderOnce for TitleBar {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mac = self.platform == PlatformStyle::Mac;
        // Linux draws buttons only when the compositor leaves decorations to
        // the app, as Zed does.
        let controls = match self.platform {
            PlatformStyle::Mac => false,
            PlatformStyle::Windows => true,
            // shortcut: no window asks for client decorations yet, so X11
            // keeps server ones and these buttons stay hidden; request them
            // with resize edges, as Zed does, when gpui_linux is added.
            PlatformStyle::Linux => {
                matches!(
                    window.window_decorations(),
                    gpui::Decorations::Client { .. }
                )
            }
        };
        let platform = self.platform;
        div()
            .debug_selector(|| "title-bar".into())
            .h(TITLE_BAR_HEIGHT)
            // The rule is inside the bar, as the mockup draws it (V61).
            .border_b(BORDER_WIDTH)
            .border_color(super::rule_color(cx))
            .w_full()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_10)
            .pl(if mac { TRAFFIC_LIGHTS_INSET } else { SPACE_12 })
            .when(!controls, |bar| bar.pr(SPACE_12))
            // Windows hit-tests the bar as its caption; elsewhere the app
            // moves the window, since the bar is transparent.
            .when(platform == PlatformStyle::Windows, |bar| {
                bar.window_control_area(WindowControlArea::Drag)
            })
            .when(platform != PlatformStyle::Windows, |bar| {
                bar.on_mouse_down(MouseButton::Left, |event, window, _| {
                    if event.click_count >= 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
            })
            .child(
                div()
                    .debug_selector(|| "title-app".into())
                    .child(Label::new("agmen").weight(FontWeight::SEMIBOLD)),
            )
            .child(Label::new("›").color(Color::Muted))
            .child(Label::new(self.project))
            .child(Chip::new(self.listener).dot(Color::Placeholder))
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(|| "title-avatar".into())
                    .child(Avatar::new(&self.me)),
            )
            .when(controls, |bar| {
                bar.child(div().w(SPACE_12))
                    .child(window_controls(platform, window))
            })
    }
}

/// A caption button: name, glyph, hit-test area, whether it shows, and
/// what a click does on Linux.
type WindowButton = (
    &'static str,
    UiIcon,
    WindowControlArea,
    bool,
    fn(&mut Window),
);

/// Minimize, maximize (or restore) and close, at the bar's right edge, as
/// Zed draws them, with Lucide glyphs in place of Segoe's.
fn window_controls(platform: PlatformStyle, window: &Window) -> impl IntoElement {
    // What Linux's compositor supports; Windows always shows all three, as
    // Zed does.
    let supported = window.window_controls();
    let linux = platform == PlatformStyle::Linux;
    let buttons: [WindowButton; 3] = [
        (
            "min",
            UiIcon::Minus,
            WindowControlArea::Min,
            !linux || supported.minimize,
            |w| w.minimize_window(),
        ),
        (
            "max",
            max_glyph(window.is_maximized()),
            WindowControlArea::Max,
            !linux || supported.maximize,
            |w| w.zoom_window(),
        ),
        (
            "close",
            UiIcon::Close,
            WindowControlArea::Close,
            true,
            |w| {
                w.remove_window();
            },
        ),
    ];
    div()
        .h_full()
        .flex()
        .children(buttons.into_iter().filter(|(.., shown, _)| *shown).map(
            move |(name, glyph, area, _, act)| {
                let style = if area == WindowControlArea::Close {
                    IconButtonStyle::CaptionClose
                } else {
                    IconButtonStyle::Caption
                };
                let button =
                    IconButton::new(SharedString::from(format!("window-control:{name}")), glyph)
                        .style(style)
                        .color(Color::Default)
                        .size(IconSize::Small);
                // Windows acts on the hit test; Linux needs the click.
                if linux {
                    button.on_click(move |_, window, _| act(window))
                } else {
                    button.window_control_area(area)
                }
            },
        ))
}

/// Maximize's glyph, or restore's when the window is maximized.
const fn max_glyph(maximized: bool) -> UiIcon {
    if maximized {
        UiIcon::Copy
    } else {
        UiIcon::Square
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::chat_snapshot;
    use crate::ui::test_support::rendered_size;
    use gpui::TestAppContext;

    #[test]
    fn title_bar_names_the_listener_with_a_colon() {
        let bar = TitleBar::new(&chat_snapshot());
        assert_eq!(bar.listener.as_ref(), "listener: codex · idle");
        assert_eq!(bar.project.as_ref(), "pr-3331");
    }

    /// Draws a title bar in the given platform's style across the window.
    struct Host(PlatformStyle);

    impl gpui::Render for Host {
        fn render(
            &mut self,
            _: &mut gpui::Window,
            _: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            div()
                .size_full()
                .child(TitleBar::new(&chat_snapshot()).platform(self.0))
        }
    }

    fn draw(cx: &mut TestAppContext, style: PlatformStyle) -> &mut gpui::VisualTestContext {
        crate::ui::test_support::init(cx);
        let (_, cx) = cx.add_window_view(|_, _| Host(style));
        crate::ui::test_support::redraw(cx);
        cx
    }

    #[gpui::test]
    fn mac_title_bar_leaves_room_for_the_traffic_lights(cx: &mut TestAppContext) {
        let cx = draw(cx, PlatformStyle::Mac);
        let app = cx.debug_bounds("title-app").unwrap();
        assert!(app.left() >= TRAFFIC_LIGHTS_INSET, "{app:?}");
        for sel in [
            "window-control:min",
            "window-control:max",
            "window-control:close",
        ] {
            assert!(cx.debug_bounds(sel).is_none(), "{sel}");
        }
    }

    #[gpui::test]
    fn windows_title_bar_draws_min_max_close_at_the_right(cx: &mut TestAppContext) {
        let cx = draw(cx, PlatformStyle::Windows);
        let app = cx.debug_bounds("title-app").unwrap();
        assert!(
            app.left() < TRAFFIC_LIGHTS_INSET,
            "no traffic lights: {app:?}"
        );
        let bar = cx.debug_bounds("title-bar").unwrap();
        let min = cx.debug_bounds("window-control:min").unwrap();
        let max = cx.debug_bounds("window-control:max").unwrap();
        let close = cx.debug_bounds("window-control:close").unwrap();
        assert!(min.right() <= max.left() && max.right() <= close.left());
        assert_eq!(close.right(), bar.right(), "close sits at the edge");
        assert_eq!(close.size.height, TITLE_BAR_HEIGHT - BORDER_WIDTH);
        let avatar = cx.debug_bounds("title-avatar").unwrap();
        assert!(avatar.right() <= min.left(), "{avatar:?} {min:?}");
    }

    #[gpui::test]
    fn linux_title_bar_draws_no_buttons_under_server_decorations(cx: &mut TestAppContext) {
        // Review C175: the test window, like X11 by default, has server
        // decorations, so the compositor draws the buttons.
        let cx = draw(cx, PlatformStyle::Linux);
        assert!(cx.debug_bounds("title-app").is_some());
        assert!(cx.debug_bounds("window-control:close").is_none());
    }

    #[test]
    fn maximize_turns_into_restore_when_maximized() {
        // Review C175.
        assert_eq!(max_glyph(false), UiIcon::Square);
        assert_eq!(max_glyph(true), UiIcon::Copy);
    }

    #[gpui::test]
    fn title_bar_renders_at_its_height(cx: &mut TestAppContext) {
        let size = rendered_size(cx, || TitleBar::new(&chat_snapshot()).into_any_element());
        assert_eq!(size.height, TITLE_BAR_HEIGHT);
    }
}
