use std::rc::Rc;

use crate::{
    clients::{Client, ClientDispatcher, RawRequest},
    color::ImageDescription,
    object::{Object, RequestResult},
    protocol_generated::{WaylandId, WpImageDescriptionReferenceV1Id},
    state::{GrabbedObject, State},
    wayland_objects::wl_display::WlDisplay,
};

/// A copyable handle. `get_image_description` copies the description and the
/// information-allowed flag.
#[derive(Debug)]
pub struct WpImageDescriptionReferenceV1 {
    id: WpImageDescriptionReferenceV1Id,
    description: Rc<ImageDescription>,
    information_allowed: bool,
}

impl WpImageDescriptionReferenceV1 {
    pub(crate) fn new(
        id: WpImageDescriptionReferenceV1Id,
        description: Rc<ImageDescription>,
        information_allowed: bool,
    ) -> Self {
        Self { id, description, information_allowed }
    }

    pub(crate) fn description(&self) -> &Rc<ImageDescription> {
        &self.description
    }

    pub(crate) fn information_allowed(&self) -> bool {
        self.information_allowed
    }
}

impl Object for WpImageDescriptionReferenceV1 {
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
        use crate::protocol_generated::wp_image_description_reference_v1::Request::*;
        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
        }
        Ok(())
    }
}
