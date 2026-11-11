use std::rc::Rc;

use crate::{
    clients::{Client, ClientDispatcher, EventWriter, RawRequest},
    color::ImageDescription,
    object::{Object, RequestResult},
    protocol_generated::{wp_image_description_v1, WaylandId, WpImageDescriptionV1Id},
    state::{GrabbedObject, State},
    wayland_objects::{
        color_management::wp_image_description_info_v1::WpImageDescriptionInfoV1,
        wl_display::WlDisplay,
    },
};

/// One image description. `description == None` means creation failed.
#[derive(Debug)]
pub struct WpImageDescriptionV1 {
    id: WpImageDescriptionV1Id,
    version: u32,
    description: Option<Rc<ImageDescription>>,
    information_allowed: bool,
    ready: bool,
    failure: Option<(u32, String)>,
}

impl WpImageDescriptionV1 {
    pub(crate) fn ready(
        id: WpImageDescriptionV1Id,
        version: u32,
        description: Rc<ImageDescription>,
        information_allowed: bool,
    ) -> Self {
        Self {
            id,
            version,
            description: Some(description),
            information_allowed,
            ready: false,
            failure: None,
        }
    }

    pub(crate) fn failed(
        id: WpImageDescriptionV1Id,
        version: u32,
        cause: u32,
        message: &str,
    ) -> Self {
        Self {
            id,
            version,
            description: None,
            information_allowed: false,
            ready: false,
            failure: Some((cause, message.to_string())),
        }
    }

    pub(crate) fn description(&self) -> Option<&Rc<ImageDescription>> {
        self.description.as_ref()
    }

    /// A description a surface may copy. Failed and not-yet-ready objects are `None`.
    pub(crate) fn ready_description(&self) -> Option<Rc<ImageDescription>> {
        self.ready.then(|| self.description.clone()).flatten()
    }

    pub(crate) fn information_allowed(&self) -> bool {
        self.information_allowed
    }

    /// Send `ready`, `ready2`, or `failed`. A v1 id that does not fit in 32 bits fails.
    pub(crate) fn publish(&mut self, dispatcher: &mut ClientDispatcher) {
        let Some(description) = self.description.clone() else {
            let (cause, message) = self.failure.clone().unwrap_or((
                wp_image_description_v1::Cause::Unsupported as u32,
                "failed".to_string(),
            ));
            self.send_event(
                wp_image_description_v1::Event::Failed { cause, msg: message },
                dispatcher,
            );
            return;
        };
        match ready_wire(self.version, description.id()) {
            ReadyWire::LowVersion => {
                self.description = None;
                self.ready = false;
                self.send_event(
                    wp_image_description_v1::Event::Failed {
                        cause: wp_image_description_v1::Cause::LowVersion as u32,
                        msg: "image description id does not fit in 32 bits".to_string(),
                    },
                    dispatcher,
                );
            }
            ReadyWire::Ready(identity) => {
                self.ready = true;
                self.send_event(wp_image_description_v1::Event::Ready { identity }, dispatcher);
            }
            ReadyWire::Ready2 { hi, lo } => {
                self.ready = true;
                self.send_event(
                    wp_image_description_v1::Event::Ready2 { identity_hi: hi, identity_lo: lo },
                    dispatcher,
                );
            }
        }
    }
}

enum ReadyWire {
    Ready(u32),
    Ready2 { hi: u32, lo: u32 },
    LowVersion,
}

/// v1 sends `ready`. An id that does not fit in 32 bits fails with `low_version`.
/// v2 and later send `ready2`.
fn ready_wire(version: u32, identity: u64) -> ReadyWire {
    if version < wp_image_description_v1::READY2_SINCE {
        if identity > u64::from(u32::MAX) {
            ReadyWire::LowVersion
        } else {
            ReadyWire::Ready(identity as u32)
        }
    } else {
        ReadyWire::Ready2 { hi: (identity >> 32) as u32, lo: identity as u32 }
    }
}

impl Object for WpImageDescriptionV1 {
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
        use wp_image_description_v1::Request::*;
        match client.parse::<Self>(req, dispatcher)? {
            Destroy => {
                client.delete_id(&WlDisplay::default(), state, dispatcher, self.id.into());
            }
            GetInformation { information } => {
                if !self.ready || self.description.is_none() {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_image_description_v1::Error::NotReady as u32,
                        "wp_image_description_v1 is not ready",
                    );
                    return Ok(());
                }
                if !self.information_allowed {
                    client.send_protocol_error(
                        dispatcher,
                        self.id.into(),
                        wp_image_description_v1::Error::NoInformation as u32,
                        "get_information is not allowed for this image description",
                    );
                    return Ok(());
                }
                let description = self.description.clone().expect("ready descriptions exist");
                let info = WpImageDescriptionInfoV1::new(information, self.version);
                info.send_description(&description, dispatcher);
                client.insert_object(state, info);
                client.delete_id(&WlDisplay::default(), state, dispatcher, information.into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ready_wire, ReadyWire};

    #[test]
    fn ready_event_shape_follows_the_bind_version() {
        assert!(matches!(ready_wire(1, 7), ReadyWire::Ready(7)));
        assert!(matches!(ready_wire(1, u64::from(u32::MAX)), ReadyWire::Ready(u32::MAX)));
        assert!(matches!(ready_wire(1, u64::from(u32::MAX) + 1), ReadyWire::LowVersion));
        assert!(matches!(ready_wire(2, (1u64 << 32) | 9), ReadyWire::Ready2 { hi: 1, lo: 9 }));
        assert!(matches!(ready_wire(3, 4), ReadyWire::Ready2 { hi: 0, lo: 4 }));
    }
}
