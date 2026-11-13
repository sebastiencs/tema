use crate::{
    clients::{Client, ClientDispatcher, Resolved},
    protocol_generated::{NewId, WlRegistryId},
    state::{Grab, IndexMap, State},
    wayland_objects::{
        ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1Global,
        ext_data_control_manager_v1::ExtDataControlManagerV1Global,
        ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1Global,
        ext_foreign_toplevel_list_v1::ExtForeignToplevelListV1Global,
        ext_idle_notifier_v1::ExtIdleNotifierV1Global,
        ext_image_copy_capture_manager_v1::ExtImageCopyCaptureManagerV1Global,
        ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1Global,
        ext_session_lock_manager_v1::ExtSessionLockManagerV1Global,
        ext_workspace_manager_v1::ExtWorkspaceManagerV1Global, wl_compositor::WlCompositorGlobal,
        wl_data_device_manager::WlDataDeviceManagerGlobal, wl_fixes::WlFixesGlobal,
        wl_seat::WlSeatGlobal, wl_shm::WlShmGlobal, wl_subcompositor::WlSubcompositorGlobal,
        wp_alpha_modifier_v1::WpAlphaModifierV1Global, wp_color_manager_v1::WpColorManagerV1Global,
        wp_color_representation_manager_v1::WpColorRepresentationManagerV1Global,
        wp_commit_timing_manager_v1::WpCommitTimingManagerV1Global,
        wp_cursor_shape_manager_v1::WpCursorShapeManagerV1Global,
        wp_fifo_manager_v1::WpFifoManagerV1Global,
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1Global,
        wp_presentation::WpPresentationGlobal,
        wp_security_context_manager_v1::WpSecurityContextManagerV1Global,
        wp_tearing_control_manager_v1::WpTearingControlManagerV1Global,
        wp_viewporter::WpViewporterGlobal, xdg_activation_v1::XdgActivationV1Global,
        xdg_dialog_v1::XdgWmDialogV1Global, xdg_system_bell_v1::XdgSystemBellV1Global,
        xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1Global,
        xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1Global,
        xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1Global, xdg_wm_base::XdgWmBaseGlobal,
        xwayland_shell_v1::XwaylandShellV1Global,
        zwlr_data_control_manager_v1::ZwlrDataControlManagerV1Global,
        zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1Global,
        zwlr_layer_shell_v1::ZwlrLayerShellV1Global,
        zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1Global,
        zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1Global,
        zwp_input_method_manager_v2::ZwpInputMethodManagerV2Global,
        zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1Global,
        zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1Global,
        zwp_pointer_constraints_v1::ZwpPointerConstraintsV1Global,
        zwp_pointer_gestures_v1::ZwpPointerGesturesV1Global,
        zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1Global,
        zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1Global,
        zwp_text_input_manager_v3::ZwpTextInputManagerV3Global,
        zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1Global,
        zxdg_decoration_manager_v1::ZxdgDecorationManagerV1Global,
        zxdg_output_manager_v1::ZxdgOutputManagerV1Global,
    },
};

use std::collections::{BTreeMap, BTreeSet};

pub struct Globals {
    pub map: Box<IndexMap<GlobalSlotId, Box<dyn Global>>>,
    current_name: u32,
    names_by_slot: BTreeMap<GlobalSlotId, GlobalName>,
    slots_by_name: BTreeMap<GlobalName, GlobalSlotId>,
    /// Globals that have sent `wl_registry.global_remove` and stay bindable until every
    /// registry that was offered them acknowledges removal or is destroyed.
    removed: BTreeSet<GlobalName>,
    /// Registries that were sent `wl_registry.global` for a name. Matches libwayland's
    /// per-registry offer list: a client that never calls `ack_global_remove` pins the
    /// global until that registry object is destroyed.
    offers: BTreeMap<GlobalName, BTreeSet<Resolved<WlRegistryId>>>,
}

index_map_id!(GlobalSlotId);

#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlobalName(u32);

impl GlobalName {
    pub fn from_raw(name: u32) -> Self {
        Self(name)
    }

    pub fn as_u32(&self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalRemoveAckError {
    /// This registry was never told about the name.
    Unknown,
    /// The name exists but `wl_registry.global_remove` has not been sent.
    NotRemoved,
}

pub trait Global {
    /// Returns `(interface_name, interface_version)`
    fn interface(&self) -> (&'static str, u32);
    /// Only the Xwayland client may see or bind this global.
    fn xwayland_only(&self) -> bool {
        false
    }
    fn bind(
        &mut self,
        client: &mut Client,
        state: &mut State,
        id: NewId,
        version: u32,
        dispatcher: &mut ClientDispatcher,
    );
}

/// What decides which globals a client may see.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientClass {
    /// The Xwayland client.
    pub xwayland: bool,
    /// Accepted on a security-context listener.
    pub sandboxed: bool,
}

impl ClientClass {
    pub fn may_see(self, global: &dyn Global) -> bool {
        (self.xwayland || !global.xwayland_only())
            && (!self.sandboxed || crate::security_context::sandbox_may_see(global.interface().0))
    }
}

impl Globals {
    pub fn new() -> Self {
        Self {
            map: Box::default(),
            current_name: 1,
            names_by_slot: BTreeMap::new(),
            slots_by_name: BTreeMap::new(),
            removed: BTreeSet::new(),
            offers: BTreeMap::new(),
        }
    }

    pub fn initialize(&mut self) {
        for fun in [
            WlCompositorGlobal::new,
            WlDataDeviceManagerGlobal::new,
            ZwlrDataControlManagerV1Global::new,
            ExtDataControlManagerV1Global::new,
            ZwpPrimarySelectionDeviceManagerV1Global::new,
            WlFixesGlobal::new,
            WlShmGlobal::new,
            WlSubcompositorGlobal::new,
            WlSeatGlobal::new, // TODO: `WlSeatGlobal` should be added dynamically
            WpPresentationGlobal::new,
            WpFifoManagerV1Global::new,
            WpCommitTimingManagerV1Global::new,
            WpFractionalScaleManagerV1Global::new,
            WpAlphaModifierV1Global::new,
            WpColorManagerV1Global::new,
            WpColorRepresentationManagerV1Global::new,
            WpTearingControlManagerV1Global::new,
            WpViewporterGlobal::new,
            ZwpLinuxDmabufV1Global::new,
            WpCursorShapeManagerV1Global::new,
            ZwpRelativePointerManagerV1Global::new,
            ZwpPointerConstraintsV1Global::new,
            ZwpPointerGesturesV1Global::new,
            ExtIdleNotifierV1Global::new,
            ExtSessionLockManagerV1Global::new,
            ZwlrScreencopyManagerV1Global::new,
            ExtImageCopyCaptureManagerV1Global::new,
            ExtOutputImageCaptureSourceManagerV1Global::new,
            ExtForeignToplevelImageCaptureSourceManagerV1Global::new,
            ExtForeignToplevelListV1Global::new,
            ZwlrForeignToplevelManagerV1Global::new,
            ZwpIdleInhibitManagerV1Global::new,
            ZwpKeyboardShortcutsInhibitManagerV1Global::new,
            ZwpInputMethodManagerV2Global::new,
            ZwpTextInputManagerV3Global::new,
            ZwpVirtualKeyboardManagerV1Global::new,
            XdgActivationV1Global::new,
            XdgWmBaseGlobal::new,
            XdgSystemBellV1Global::new,
            XdgToplevelDragManagerV1Global::new,
            XdgToplevelIconManagerV1Global::new,
            XdgToplevelTagManagerV1Global::new,
            XdgWmDialogV1Global::new,
            ZxdgDecorationManagerV1Global::new,
            ZwlrLayerShellV1Global::new,
            ZxdgOutputManagerV1Global::new,
            ExtBackgroundEffectManagerV1Global::new,
            ExtWorkspaceManagerV1Global::new,
            XwaylandShellV1Global::new,
            WpSecurityContextManagerV1Global::new,
        ] {
            self.insert(fun());
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (GlobalName, &Box<dyn Global>)> {
        self.map.iter().filter_map(|(slot_id, global)| {
            let name = *self
                .names_by_slot
                .get(&slot_id)
                .expect("missing global wire name");
            (!self.removed.contains(&name)).then_some((name, global))
        })
    }

    /// Globals this client may hear about.
    pub fn iter_for(
        &self,
        class: ClientClass,
    ) -> impl Iterator<Item = (GlobalName, &Box<dyn Global>)> {
        self.iter()
            .filter(move |(_, global)| class.may_see(&***global))
    }

    /// `true` when `name` is a known global this client class may not see.
    /// Unknown names return `false`, like `is_xwayland_only` did.
    pub fn is_hidden_from(&self, name: GlobalName, class: ClientClass) -> bool {
        let Some(&slot_id) = self.slots_by_name.get(&name) else {
            return false;
        };
        self.map
            .try_get(slot_id)
            .is_some_and(|global| !class.may_see(&**global))
    }

    pub fn insert(&mut self, global: Box<dyn Global>) -> GlobalName {
        let slot_id = self.map.insert(global);
        let name = GlobalName(self.current_name);
        self.current_name = self
            .current_name
            .checked_add(1)
            .expect("global name space exhausted");
        assert!(self.names_by_slot.insert(slot_id, name).is_none());
        assert!(self.slots_by_name.insert(name, slot_id).is_none());
        name
    }

    /// Remove the live global for `interface`. Used before any client connects.
    pub fn remove_interface(&mut self, interface: &str) -> bool {
        let name = self
            .iter()
            .find(|(_, global)| global.interface().0 == interface)
            .map(|(name, _)| name);
        name.is_some_and(|name| self.remove(name).is_some())
    }

    pub fn remove(&mut self, name: GlobalName) -> Option<Box<dyn Global>> {
        let slot_id = self.slots_by_name.remove(&name)?;
        self.names_by_slot.remove(&slot_id);
        self.removed.remove(&name);
        self.offers.remove(&name);
        self.map.remove(slot_id)
    }

    /// Stop advertising `name` but keep it bindable until every registry that saw it
    /// acknowledges the removal or is destroyed. Returns the global only when nothing
    /// is left to wait for.
    pub fn withdraw(&mut self, name: GlobalName) -> Option<Box<dyn Global>> {
        if !self.slots_by_name.contains_key(&name) || !self.removed.insert(name) {
            return None;
        }
        self.finish_if_withdrawn(name)
    }

    pub fn note_offer(&mut self, name: GlobalName, registry: Resolved<WlRegistryId>) {
        if self.removed.contains(&name) || !self.slots_by_name.contains_key(&name) {
            return;
        }
        self.offers.entry(name).or_default().insert(registry);
    }

    pub fn was_offered(&self, name: GlobalName, registry: Resolved<WlRegistryId>) -> bool {
        self.offers
            .get(&name)
            .is_some_and(|registries| registries.contains(&registry))
    }

    pub fn ack_global_remove(
        &mut self,
        name: GlobalName,
        registry: Resolved<WlRegistryId>,
    ) -> Result<Option<Box<dyn Global>>, GlobalRemoveAckError> {
        if !self.was_offered(name, registry) {
            return Err(GlobalRemoveAckError::Unknown);
        }
        if !self.removed.contains(&name) {
            return Err(GlobalRemoveAckError::NotRemoved);
        }
        self.take_offer(name, registry);
        Ok(self.finish_if_withdrawn(name))
    }

    pub fn release_registry(&mut self, registry: Resolved<WlRegistryId>) {
        let names: Vec<GlobalName> = self.offers.keys().copied().collect();
        for name in names {
            if self.take_offer(name, registry) {
                let _ = self.finish_if_withdrawn(name);
            }
        }
    }

    fn take_offer(&mut self, name: GlobalName, registry: Resolved<WlRegistryId>) -> bool {
        let Some(registries) = self.offers.get_mut(&name) else {
            return false;
        };
        let removed = registries.remove(&registry);
        if registries.is_empty() {
            self.offers.remove(&name);
        }
        removed
    }

    fn finish_if_withdrawn(&mut self, name: GlobalName) -> Option<Box<dyn Global>> {
        if !self.removed.contains(&name) || self.offers.contains_key(&name) {
            return None;
        }
        self.remove(name)
    }

    pub fn contains_interface(&self, interface: &str) -> bool {
        self.iter()
            .any(|(_, global)| global.interface().0 == interface)
    }

    pub fn name_for_interface(&self, interface: &str) -> Option<GlobalName> {
        self.iter()
            .find_map(|(name, global)| (global.interface().0 == interface).then_some(name))
    }

    /// Take a global out of the registry for binding. Fails if `name` is not a live global slot.
    pub fn try_grab_global(&mut self, name: u32) -> Option<Grab<GlobalSlotId, Box<dyn Global>>> {
        let slot_id = *self.slots_by_name.get(&GlobalName(name))?;
        self.map.try_grab(slot_id)
    }
}
