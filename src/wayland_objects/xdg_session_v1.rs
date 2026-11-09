//! `xdg_session_v1`. One application session and the toplevels registered in it.

use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest, Resolved},
    object::{Object, RequestResult},
    protocol_generated::{
        xdg_session_v1, WaylandId, XdgSessionV1Id, XdgToplevelId, XdgToplevelSessionV1Id,
    },
    session::LiveToplevel,
    state::{GrabbedObject, State},
    wayland_objects::{wl_display::WlDisplay, xdg_toplevel_session_v1::XdgToplevelSessionV1},
};

#[derive(Debug)]
pub struct XdgSessionV1 {
    id: XdgSessionV1Id,
    session_id: String,
    inert: bool,
}

impl XdgSessionV1 {
    pub(crate) fn is_inert(&self) -> bool {
        self.inert
    }

    pub(crate) fn set_inert(&mut self) {
        self.inert = true;
    }

    fn protocol_error(
        &self,
        client: &mut Client,
        dispatcher: &mut ClientDispatcher,
        code: xdg_session_v1::Error,
        message: &str,
    ) {
        client.protocol_violation_disconnect(dispatcher, self.id.into(), code as u32, message);
    }

    fn link_toplevel(
        &mut self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        id: XdgToplevelSessionV1Id,
        toplevel_id: XdgToplevelId,
        name: String,
        restore: bool,
    ) -> RequestResult {
        let toplevel = client.resolve_or_disconnect(toplevel_id, state, dispatcher)?;
        let parent = client
            .resolve(self.id, state)
            .expect("grabbed session stays resolvable");
        if self.inert {
            client.insert_object(
                state,
                XdgToplevelSessionV1::new(id, self.session_id.clone(), name, None, true, parent),
            );
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
        if toplevel.rref(state).session().is_some() {
            self.protocol_error(
                client,
                dispatcher,
                xdg_session_v1::Error::AlreadyAdded,
                "toplevel already added",
            );
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
        if restore {
            let xdg_surface = toplevel.rref(state).xdg_surface();
            if xdg_surface.rref(state).initial_commit_complete() {
                self.protocol_error(
                    client,
                    dispatcher,
                    xdg_session_v1::Error::AlreadyMapped,
                    "toplevel already mapped",
                );
                return Ok(());
            }
        }

        let window = toplevel.rref(state).window();
        let session_id = self.session_id.clone();
        let object = {
            let (object, _) = client.insert_object(
                state,
                XdgToplevelSessionV1::new(
                    id,
                    session_id.clone(),
                    name.clone(),
                    Some(toplevel),
                    false,
                    parent,
                ),
            );
            object
        };
        if let Some(toplevel) = toplevel.try_ref_mut(state) {
            toplevel.set_session(Some(object));
        }
        if let Some(live) = state.live_sessions.get_mut(&session_id) {
            live.toplevels
                .insert(name.clone(), LiveToplevel { object, window });
        }

        let stored = if restore {
            state
                .session_store
                .as_ref()
                .and_then(|store| store.toplevel(&session_id, &name).cloned())
        } else {
            None
        };
        if let Some(record) = stored {
            if let Some(toplevel) = toplevel.try_ref_mut(state) {
                toplevel.set_pending_session_restore(Some((session_id, record)));
            }
        } else if state
            .managed_window_state(window)
            .is_some_and(|managed| managed.mapped)
        {
            crate::session::sample_into_store(state, &session_id, &name, window);
        }
        crate::session_store::model::note_dirty(state, dispatcher);
        Ok(())
    }
}

pub(crate) fn open(
    client: &mut Client,
    state: &mut State,
    dispatcher: &mut ClientDispatcher,
    id: XdgSessionV1Id,
    session_id: String,
    created: bool,
) -> Resolved<XdgSessionV1Id> {
    let (resolved, session) = client
        .insert_object(state, XdgSessionV1 { id, session_id: session_id.clone(), inert: false });
    if created {
        session.send_event(xdg_session_v1::Event::Created { session_id }, dispatcher);
    } else {
        session.send_event(xdg_session_v1::Event::Restored, dispatcher);
    }
    resolved
}

pub(crate) fn send_replaced(
    session: Resolved<XdgSessionV1Id>,
    state: &State,
    dispatcher: &mut ClientDispatcher,
) {
    let Some(session) = session.try_ref(state) else {
        return;
    };
    session.send_event(xdg_session_v1::Event::Replaced, dispatcher);
}

impl Object for XdgSessionV1 {
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
        use xdg_session_v1::Request::*;

        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                if !self.inert {
                    self.inert = true;
                    crate::session::release_live_session(state, &self.session_id, true);
                }
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            Remove => {
                if self.inert {
                    client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
                    return Ok(());
                }
                self.inert = true;
                let session_id = self.session_id.clone();
                crate::session::release_live_session(state, &session_id, false);
                if let Some(store) = state.session_store.as_mut() {
                    store.remove_session(&session_id);
                }
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
                crate::session_store::model::note_dirty(state, dispatcher);
            }
            AddToplevel { id, toplevel, name } => {
                return self.link_toplevel(client, state, dispatcher, id, toplevel, name, false);
            }
            RestoreToplevel { id, toplevel, name } => {
                return self.link_toplevel(client, state, dispatcher, id, toplevel, name, true);
            }
            RemoveToplevel { name } => {
                if self.inert {
                    return Ok(());
                }
                let Some(toplevel) = state
                    .live_sessions
                    .get_mut(&self.session_id)
                    .and_then(|live| live.toplevels.remove(&name))
                else {
                    return Ok(());
                };
                crate::session::detach_toplevel_session(state, toplevel.object);
                if let Some(store) = state.session_store.as_mut() {
                    store.remove_toplevel(&self.session_id, &name);
                }
                crate::session_store::model::note_dirty(state, dispatcher);
            }
        }
        Ok(())
    }

    fn on_client_disconnect(
        &mut self,
        _client: &mut Client,
        state: &mut State,
        _dispatcher: &mut ClientDispatcher,
    ) {
        if self.inert {
            return;
        }
        self.inert = true;
        crate::session::release_live_session(state, &self.session_id, true);
    }
}
