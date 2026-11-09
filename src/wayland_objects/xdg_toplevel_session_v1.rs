//! `xdg_toplevel_session_v1`. Rename and destroy one toplevel inside a session.

use crate::{
    clients::{Client, ClientDispatcher, RawRequest, Resolved},
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_session_v1, xdg_toplevel_session_v1, WaylandId, XdgSessionV1Id, XdgToplevelId,
        XdgToplevelSessionV1Id,
    },
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

#[derive(Debug)]
pub struct XdgToplevelSessionV1 {
    id: XdgToplevelSessionV1Id,
    session_id: String,
    name: String,
    toplevel: Option<Resolved<XdgToplevelId>>,
    inert: bool,
    parent: Resolved<XdgSessionV1Id>,
}

impl XdgToplevelSessionV1 {
    pub(crate) fn new(
        id: XdgToplevelSessionV1Id,
        session_id: String,
        name: String,
        toplevel: Option<Resolved<XdgToplevelId>>,
        inert: bool,
        parent: Resolved<XdgSessionV1Id>,
    ) -> Self {
        Self { id, session_id, name, toplevel, inert, parent }
    }

    pub(crate) fn is_inert(&self) -> bool {
        self.inert
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Drop the link to the toplevel. The caller clears `XdgToplevel.session` when it still can.
    pub(crate) fn make_inert(&mut self) -> Option<Resolved<XdgToplevelId>> {
        self.inert = true;
        self.toplevel.take()
    }

    fn protocol_error(
        &self,
        client: &mut Client,
        dispatcher: &mut ClientDispatcher,
        code: xdg_session_v1::Error,
        message: &str,
    ) {
        client.protocol_violation_disconnect(
            dispatcher,
            self.parent.wayland_id().into(),
            code as u32,
            message,
        );
    }
}

impl Object for XdgToplevelSessionV1 {
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
        use xdg_toplevel_session_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                if !self.inert {
                    let session_id = self.session_id.clone();
                    let name = self.name.clone();
                    if let Some(window) = self
                        .toplevel
                        .as_ref()
                        .map(|toplevel| toplevel.rref(state).window())
                    {
                        crate::session::sample_into_store(state, &session_id, &name, window);
                    }
                    if let Some(live) = state.live_sessions.get_mut(&session_id) {
                        live.toplevels.remove(&name);
                    }
                    if let Some(toplevel) = self.toplevel.take() {
                        if let Some(toplevel) = toplevel.try_ref_mut(state) {
                            toplevel.set_session(None);
                        }
                    }
                    self.inert = true;
                }
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            Rename { name } => {
                if self.inert {
                    return Ok(());
                }
                if name.is_empty() {
                    self.protocol_error(
                        client,
                        dispatcher,
                        xdg_session_v1::Error::InvalidName,
                        "invalid toplevel name",
                    );
                    return Ok(());
                }
                if name == self.name {
                    return Ok(());
                }
                if state
                    .live_sessions
                    .get(&self.session_id)
                    .is_some_and(|live| live.toplevels.contains_key(&name))
                {
                    self.protocol_error(
                        client,
                        dispatcher,
                        xdg_session_v1::Error::NameInUse,
                        "toplevel name in use",
                    );
                    return Ok(());
                }
                if let Some(store) = state.session_store.as_mut() {
                    store.rename_toplevel(&self.session_id, &self.name, &name);
                }
                if let Some(live) = state.live_sessions.get_mut(&self.session_id) {
                    if let Some(toplevel) = live.toplevels.remove(&self.name) {
                        live.toplevels.insert(name.clone(), toplevel);
                    }
                }
                self.name = name;
                crate::session_store::model::note_dirty(state, dispatcher);
            }
        }
        Ok(())
    }
}
