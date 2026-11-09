//! `xdg_toplevel_drag_manager_v1`. One drag object per unused `wl_data_source`.

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    globals::Global,
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_toplevel_drag_manager_v1, Constants, NewId, WaylandId, WlDataSourceId,
        XdgToplevelDragManagerV1Id, XdgToplevelDragV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::{
        wl_data_source::SourceUse, wl_display::WlDisplay, xdg_toplevel_drag_v1::XdgToplevelDragV1,
    },
};

/// Advertised at version 1.
#[derive(Debug)]
pub struct XdgToplevelDragManagerV1Global;

impl XdgToplevelDragManagerV1Global {
    pub fn new() -> Box<dyn Global> {
        Box::new(Self)
    }
}

impl Global for XdgToplevelDragManagerV1Global {
    fn interface(&self) -> (&'static str, u32) {
        (XdgToplevelDragManagerV1::INTERFACE_NAME, XdgToplevelDragManagerV1::INTERFACE_VERSION)
    }

    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        _version: u32,
        _dispatcher: &mut ClientDispatcher,
    ) {
        client.insert_object(
            state,
            XdgToplevelDragManagerV1 { id: XdgToplevelDragManagerV1Id::from(id) },
        );
    }
}

#[derive(Debug)]
pub struct XdgToplevelDragManagerV1 {
    id: XdgToplevelDragManagerV1Id,
}

impl XdgToplevelDragManagerV1 {
    pub(crate) fn new(id: XdgToplevelDragManagerV1Id) -> Self {
        Self { id }
    }
}

impl Object for XdgToplevelDragManagerV1 {
    fn id(&self) -> WaylandId {
        self.id.into()
    }

    fn on(
        self: GrabbedObject<Self>,
        client: &mut Client,
        state: &mut State,
        req: RawRequest,
        dispatcher: &mut ClientDispatcher,
    ) -> RequestResult {
        use xdg_toplevel_drag_manager_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetXdgToplevelDrag { id, data_source } => {
                if !get_drag(self.id, client, state, dispatcher, id, data_source) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

fn get_drag(
    manager: XdgToplevelDragManagerV1Id,
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    id: XdgToplevelDragV1Id,
    data_source: WlDataSourceId,
) -> bool {
    let source = match client.resolve_or_disconnect(data_source, state, dispatcher) {
        Ok(source) => source,
        Err(_) => return false,
    };
    let usable = {
        let Some(source) = source.try_ref(state) else {
            return false;
        };
        source.used() == SourceUse::Unused && source.toplevel_drag().is_none()
    };
    if !usable {
        client.protocol_violation_disconnect(
            dispatcher,
            manager.into(),
            xdg_toplevel_drag_manager_v1::Error::InvalidSource as u32,
            "data source cannot be used for xdg_toplevel_drag",
        );
        return false;
    }
    let resolved = {
        let (resolved, drag) =
            client.insert_object(state, XdgToplevelDragV1::new(id, client.client_id(), source));
        drag.bind(resolved.object_id());
        resolved
    };
    if let Some(mut source) = source.try_ref_mut(state) {
        source.set_toplevel_drag(Some(resolved));
    }
    true
}
