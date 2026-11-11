use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    object::{Object, Parser, RequestResult},
    protocol_generated::{
        wp_color_management_output_v1, wp_image_description_v1, WaylandId,
        WpColorManagementOutputV1Id, WpImageDescriptionV1Id,
    },
    state::{GrabbedObject, OutputId, State},
    wayland_objects::wl_display::WlDisplay,
};

/// One `wp_color_management_output_v1`. It follows the output global, not the
/// `wl_output` proxy: destroying the proxy leaves this object live, and removing
/// the output makes `get_image_description` fail with `no_output`.
#[derive(Debug)]
pub struct WpColorManagementOutputV1 {
    id: WpColorManagementOutputV1Id,
    version: u32,
    output_id: Option<OutputId>,
}

impl WpColorManagementOutputV1 {
    pub(crate) fn new(
        id: WpColorManagementOutputV1Id,
        version: u32,
        output_id: Option<OutputId>,
    ) -> Self {
        Self { id, version, output_id }
    }

    pub(crate) fn output_id(&self) -> Option<OutputId> {
        self.output_id
    }

    pub(crate) fn send_image_description_changed(&self, dispatcher: &mut ClientDispatcher) {
        self.send_event(wp_color_management_output_v1::Event::ImageDescriptionChanged, dispatcher);
    }

    fn publish_current(
        &self,
        client: &mut Client,
        state: &mut State,
        dispatcher: &mut ClientDispatcher,
        image_description: WpImageDescriptionV1Id,
    ) {
        let description = self
            .output_id
            .and_then(|output_id| state.outputs.try_get(output_id))
            .map(|output| std::rc::Rc::clone(&output.color_description));
        match description {
            Some(description) => super::publish_ready_description(
                client,
                state,
                dispatcher,
                image_description,
                self.version,
                description,
                true,
            ),
            None => super::publish_failed_description(
                client,
                state,
                dispatcher,
                image_description,
                self.version,
                wp_image_description_v1::Cause::NoOutput as u32,
                "the output no longer exists",
            ),
        }
    }
}

impl Object for WpColorManagementOutputV1 {
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
        use wp_color_management_output_v1::Request::*;
        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetImageDescription { image_description } => {
                self.publish_current(client, state, dispatcher, image_description);
            }
        }
        Ok(())
    }
}
