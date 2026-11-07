//! The compositor-side role object for one managed X window.
//!
//! It lives in `state.objects` and implements [`Toplevel`](crate::window::Toplevel).
//! No client maps it, so its protocol id is unused.

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    object::{Object, RequestResult},
    protocol_generated::WaylandId,
    runner::Dispatcher,
    state::{GrabbedObject, State},
    window::{InteractiveSizeConstraints, Toplevel, ToplevelDecorationMode, WindowId},
    x11::Window,
    xwayland::XwaylandAction,
};

use super::{props::WindowProps, WmEvent};

/// ICCCM `WM_NORMAL_HINTS` flag for a usable minimum size.
const P_MIN_SIZE: u32 = 1 << 4;
/// ICCCM `WM_NORMAL_HINTS` flag for a usable maximum size.
const P_MAX_SIZE: u32 = 1 << 5;
const HINT_FLAGS: usize = 0;
const HINT_MIN_WIDTH: usize = 5;
const HINT_MIN_HEIGHT: usize = 6;
const HINT_MAX_WIDTH: usize = 7;
const HINT_MAX_HEIGHT: usize = 8;

/// Role object of one managed X window.
pub(crate) struct XToplevel {
    window: WindowId,
    x_window: Window,
    run: u64,
    title: String,
    app_id: String,
    parent: Option<WindowId>,
    min_size: Option<(i32, i32)>,
    max_size: Option<(i32, i32)>,
    maximized: bool,
    fullscreen_requested: bool,
}

impl XToplevel {
    pub(super) fn new(window: WindowId, x_window: Window, run: u64) -> Self {
        Self {
            window,
            x_window,
            run,
            title: String::new(),
            app_id: String::new(),
            parent: None,
            min_size: None,
            max_size: None,
            maximized: false,
            fullscreen_requested: false,
        }
    }

    /// Copies the fields the layout reads. Returns whether the minimum size changed.
    pub(super) fn refresh(&mut self, props: &WindowProps, parent: Option<WindowId>) -> bool {
        self.title = props.title.clone().unwrap_or_default();
        self.app_id = props.class.clone().unwrap_or_default();
        self.parent = parent;
        self.maximized = props.maximized_vert && props.maximized_horz;
        self.fullscreen_requested = props.fullscreen;
        let previous_min = self.min_size;
        self.min_size =
            hinted_size(&props.normal_hints, P_MIN_SIZE, HINT_MIN_WIDTH, HINT_MIN_HEIGHT);
        self.max_size =
            hinted_size(&props.normal_hints, P_MAX_SIZE, HINT_MAX_WIDTH, HINT_MAX_HEIGHT);
        previous_min != self.min_size
    }
}

/// A size pair from `WM_NORMAL_HINTS` when the flag is set and both values are positive.
fn hinted_size(
    hints: &[u32],
    flag: u32,
    width_index: usize,
    height_index: usize,
) -> Option<(i32, i32)> {
    if hints.get(HINT_FLAGS).copied().unwrap_or(0) & flag == 0 {
        return None;
    }
    let width = i32::try_from(*hints.get(width_index)?).ok()?;
    let height = i32::try_from(*hints.get(height_index)?).ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

impl Toplevel for XToplevel {
    fn window(&self) -> WindowId {
        self.window
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn parent_window(&self) -> Option<WindowId> {
        self.parent
    }

    fn size_constraints(&self) -> InteractiveSizeConstraints {
        InteractiveSizeConstraints::new(self.min_size, self.max_size)
    }

    fn minimum_size(&self) -> Option<(i32, i32)> {
        self.min_size
    }

    fn is_maximized(&self) -> bool {
        self.maximized
    }

    fn is_fullscreen_requested(&self) -> bool {
        self.fullscreen_requested
    }

    fn decoration_mode(&self) -> ToplevelDecorationMode {
        ToplevelDecorationMode::ServerSide
    }

    fn request_close(&self, dispatcher: &mut Dispatcher) {
        dispatcher.dispatch(XwaylandAction::Wm {
            run: self.run,
            event: WmEvent::Close { window: self.x_window },
        });
    }

    fn interactive_resize(&self, dispatcher: &mut Dispatcher, size: (i32, i32), _resizing: bool) {
        dispatcher.dispatch(XwaylandAction::Wm {
            run: self.run,
            event: WmEvent::Resize { window: self.x_window, width: size.0, height: size.1 },
        });
    }

    fn position_changed(&self, dispatcher: &mut Dispatcher) {
        dispatcher.dispatch(XwaylandAction::Wm { run: self.run, event: WmEvent::Reconfigure });
    }

    fn set_requested_fullscreen(&mut self, _fullscreen: bool) {}
}

impl Object for XToplevel {
    fn id(&self) -> WaylandId {
        WaylandId::from(0)
    }

    fn on(
        self: GrabbedObject<Self>,
        _client: &mut Client,
        _state: &mut State,
        _request: RawRequest,
        _dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        Ok(())
    }

    fn as_toplevel(&self) -> Option<&dyn Toplevel> {
        Some(self)
    }

    fn as_toplevel_mut(&mut self) -> Option<&mut dyn Toplevel> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{runner::Dispatcher, test_support::state_for_tests},
    };

    fn sized_hints(flags: u32, min: (u32, u32), max: (u32, u32)) -> Vec<u32> {
        vec![flags, 0, 0, 0, 0, min.0, min.1, max.0, max.1]
    }

    #[test]
    fn refresh_maps_title_class_state_and_size_hints() {
        let mut toplevel = XToplevel::new(WindowId::for_tests(3), Window(9), 4);
        let parent = WindowId::for_tests(2);
        let mut props = WindowProps::default();
        props.title = Some("Term".to_string());
        props.class = Some("XTerm".to_string());
        props.maximized_vert = true;
        props.maximized_horz = true;
        props.fullscreen = true;
        props.normal_hints = sized_hints(P_MIN_SIZE | P_MAX_SIZE, (80, 40), (800, 600));

        assert!(toplevel.refresh(&props, Some(parent)));
        assert!(!toplevel.refresh(&props, Some(parent)));
        assert_eq!(toplevel.window(), WindowId::for_tests(3));
        assert_eq!(toplevel.title(), "Term");
        assert_eq!(toplevel.app_id(), "XTerm");
        assert_eq!(toplevel.parent_window(), Some(parent));
        assert!(toplevel.is_maximized());
        assert!(toplevel.is_fullscreen_requested());
        assert_eq!(toplevel.minimum_size(), Some((80, 40)));
        assert_eq!(toplevel.size_constraints().clamp(10, 10), (80, 40));
        assert_eq!(toplevel.size_constraints().clamp(900, 700), (800, 600));
        assert_eq!(toplevel.decoration_mode(), ToplevelDecorationMode::ServerSide);
    }

    #[test]
    fn refresh_drops_hints_without_flags_or_a_positive_pair() {
        let mut toplevel = XToplevel::new(WindowId::for_tests(1), Window(1), 1);
        let mut props = WindowProps::default();
        props.maximized_vert = true;
        props.normal_hints = sized_hints(0, (80, 40), (800, 600));

        assert!(!toplevel.refresh(&props, None));
        assert_eq!(toplevel.title(), "");
        assert_eq!(toplevel.app_id(), "");
        assert!(toplevel.parent_window().is_none());
        assert!(!toplevel.is_maximized());
        assert!(!toplevel.is_fullscreen_requested());
        assert_eq!(toplevel.minimum_size(), None);
        assert_eq!(toplevel.size_constraints().clamp(900, 700), (900, 700));

        props.normal_hints = vec![P_MIN_SIZE | P_MAX_SIZE, 1, 2];
        assert!(!toplevel.refresh(&props, None));
        assert_eq!(toplevel.minimum_size(), None);
        assert_eq!(toplevel.size_constraints().clamp(900, 700), (900, 700));

        props.normal_hints = sized_hints(P_MIN_SIZE | P_MAX_SIZE, (0, 40), (800, 0));
        assert!(!toplevel.refresh(&props, None));
        assert_eq!(toplevel.minimum_size(), None);
        assert_eq!(toplevel.size_constraints().clamp(900, 700), (900, 700));
    }

    #[test]
    fn request_close_queues_the_wm_close() {
        let toplevel = XToplevel::new(WindowId::for_tests(1), Window(12), 7);
        let mut dispatcher = Dispatcher::default();
        toplevel.request_close(&mut dispatcher);

        assert!(dispatcher.test_has_queued_action(|action: &XwaylandAction| {
            matches!(
                action,
                XwaylandAction::Wm { run: 7, event: WmEvent::Close { window } } if *window == Window(12)
            )
        }));
    }

    #[test]
    fn interactive_resize_queues_the_wm_resize() {
        let toplevel = XToplevel::new(WindowId::for_tests(1), Window(12), 7);
        let mut dispatcher = Dispatcher::default();
        toplevel.interactive_resize(&mut dispatcher, (320, 200), true);
        toplevel.interactive_resize(&mut dispatcher, (100, 80), false);

        let mut sizes = Vec::new();
        dispatcher.test_for_each_queued_action(|action: &XwaylandAction| {
            if let XwaylandAction::Wm { run: 7, event: WmEvent::Resize { window, width, height } } =
                action
            {
                assert_eq!(*window, Window(12));
                sizes.push((*width, *height));
            }
        });
        assert_eq!(sizes, vec![(320, 200), (100, 80)]);
    }

    #[test]
    fn a_compositor_fullscreen_toggle_leaves_the_client_request() {
        let mut toplevel = XToplevel::new(WindowId::for_tests(1), Window(1), 1);
        let mut props = WindowProps::default();
        props.fullscreen = true;
        toplevel.refresh(&props, None);

        toplevel.set_requested_fullscreen(false);
        assert!(toplevel.is_fullscreen_requested());
        toplevel.set_requested_fullscreen(true);
        assert!(toplevel.is_fullscreen_requested());
    }

    #[test]
    fn an_inserted_xtoplevel_is_reached_as_a_toplevel() {
        let mut state = state_for_tests();
        let window = WindowId::for_tests(5);
        let (_, object_id) = state.insert_object(XToplevel::new(window, Window(1), 2));

        let toplevel = state
            .objects
            .try_get(object_id)
            .and_then(|object| object.as_toplevel())
            .expect("the inserted object is a toplevel");
        assert_eq!(toplevel.window(), window);
        assert_eq!(toplevel.decoration_mode(), ToplevelDecorationMode::ServerSide);
    }
}
