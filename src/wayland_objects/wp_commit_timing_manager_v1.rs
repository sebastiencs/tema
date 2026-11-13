use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    globals::Global,
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wp_commit_timing_manager_v1, Constants, NewId, WaylandId, WpCommitTimingManagerV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, wp_commit_timer_v1::WpCommitTimerV1},
};

#[derive(Debug)]
pub struct WpCommitTimingManagerV1Global;

impl WpCommitTimingManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for WpCommitTimingManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (WpCommitTimingManagerV1::INTERFACE_NAME, WpCommitTimingManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(state, WpCommitTimingManagerV1 { id: id.into() });
    }
}

#[derive(Debug)]
pub struct WpCommitTimingManagerV1 {
    id: WpCommitTimingManagerV1Id,
}

impl Object for WpCommitTimingManagerV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        mut self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use wp_commit_timing_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetTimer { id, surface } => {
                let surface = client.resolve_or_disconnect(surface, state, dispatcher)?;
                if surface.rref(state).has_commit_timer_object() {
                    client.protocol_violation_disconnect(
                        dispatcher,
                        self.id.into(),
                        wp_commit_timing_manager_v1::Error::CommitTimerExists as u32,
                        "wl_surface already has a wp_commit_timer_v1",
                    );
                    return Ok(());
                }
                let (timer, _) = client.insert_object(state, WpCommitTimerV1::new(id, surface));
                surface.ref_mut(state).set_commit_timer_object(timer);
            }
        }
        Ok(())
    }
}
