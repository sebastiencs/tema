//! Serials committed for the Xwayland client's surfaces.
//!
//! The Xwayland model owns this map for the current run. M5 reads it when an
//! X window announces `WL_SURFACE_SERIAL`.

use std::collections::BTreeMap;

use crate::{clients::Resolved, protocol_generated::WlSurfaceId};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum SurfaceEvent {
    SerialAssigned { serial: u64, surface: Resolved<WlSurfaceId> },
    Destroyed { serial: u64, surface: Resolved<WlSurfaceId> },
}

#[derive(Debug, Default)]
pub(crate) struct SurfaceSerials {
    by_serial: BTreeMap<u64, Resolved<WlSurfaceId>>,
}

impl SurfaceSerials {
    pub(crate) fn apply(&mut self, event: SurfaceEvent) {
        match event {
            SurfaceEvent::SerialAssigned { serial, surface } => {
                self.by_serial.insert(serial, surface);
            }
            SurfaceEvent::Destroyed { serial, surface } => {
                if self
                    .by_serial
                    .get(&serial)
                    .is_some_and(|current| *current == surface)
                {
                    self.by_serial.remove(&serial);
                }
            }
        }
    }

    pub(crate) fn get(&self, serial: u64) -> Option<Resolved<WlSurfaceId>> {
        self.by_serial.get(&serial).copied()
    }

    pub(crate) fn clear(&mut self) {
        self.by_serial.clear();
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{clients::ClientId, protocol_generated::WlSurfaceId, state::ObjectId},
    };

    fn surface(raw: u32) -> Resolved<WlSurfaceId> {
        Resolved::new(ClientId::from_raw(1), WlSurfaceId::from(raw), ObjectId::from(raw as usize))
    }

    #[test]
    fn assign_get_and_destroy_match_the_surface() {
        let mut serials = SurfaceSerials::default();
        let first = surface(4);
        let other = surface(8);
        serials.apply(SurfaceEvent::SerialAssigned { serial: 1, surface: first });

        assert_eq!(serials.get(1), Some(first));
        assert!(serials.get(2).is_none());

        serials.apply(SurfaceEvent::Destroyed { serial: 1, surface: other });
        assert_eq!(serials.get(1), Some(first));

        serials.apply(SurfaceEvent::Destroyed { serial: 1, surface: first });
        assert!(serials.get(1).is_none());

        serials.apply(SurfaceEvent::SerialAssigned { serial: 2, surface: other });
        serials.clear();
        assert!(serials.get(2).is_none());
    }
}
