//! security-context-v1: metadata of sandboxed clients and the globals hidden from them.

/// Metadata of one committed `wp_security_context_v1`. Shared by every client
/// accepted on its listener. Strings are stored exactly as the client sent them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct SecurityContextMetadata {
    pub(crate) sandbox_engine: Option<String>,
    pub(crate) app_id: Option<String>,
    pub(crate) instance_id: Option<String>,
}

/// Live security-context listeners allowed in the compositor at once.
pub(crate) const MAX_SECURITY_LISTENERS: usize = 128;

/// Live clients one security-context listener may have at once.
pub(crate) const MAX_CLIENTS_PER_SECURITY_LISTENER: usize = 256;

/// The only globals a sandboxed client may see and bind. Everything else,
/// including globals added later, is hidden (fail closed). A new global is
/// hidden from sandboxed clients until its interface is added here.
pub(crate) const SANDBOX_ALLOWED_INTERFACES: &[&str] = &[
    // core
    "wl_compositor",
    "wl_subcompositor",
    "wl_shm",
    "wl_seat",
    "wl_output",
    "wl_data_device_manager",
    "wl_fixes",
    // shell
    "xdg_wm_base",
    "zxdg_decoration_manager_v1",
    "zxdg_output_manager_v1",
    "xdg_activation_v1",
    "xdg_wm_dialog_v1",
    "xdg_system_bell_v1",
    "xdg_toplevel_tag_manager_v1",
    "xdg_toplevel_icon_manager_v1",
    "xdg_toplevel_drag_manager_v1",
    "xdg_session_manager_v1",
    // buffers, presentation, color
    "zwp_linux_dmabuf_v1",
    "wp_linux_drm_syncobj_manager_v1",
    "wp_presentation",
    "wp_viewporter",
    "wp_fractional_scale_manager_v1",
    "wp_alpha_modifier_v1",
    "wp_color_manager_v1",
    "wp_color_representation_manager_v1",
    "wp_tearing_control_manager_v1",
    "wp_fifo_manager_v1",
    "wp_commit_timing_manager_v1",
    "ext_background_effect_manager_v1",
    // input
    "zwp_primary_selection_device_manager_v1",
    "wp_cursor_shape_manager_v1",
    "zwp_relative_pointer_manager_v1",
    "zwp_pointer_constraints_v1",
    "zwp_pointer_gestures_v1",
    "zwp_keyboard_shortcuts_inhibit_manager_v1",
    "zwp_text_input_manager_v3",
    "zwp_idle_inhibit_manager_v1",
];

/// `true` only when a sandboxed client may see a global with this interface.
pub(crate) fn sandbox_may_see(interface: &str) -> bool {
    SANDBOX_ALLOWED_INTERFACES.contains(&interface)
}

#[cfg(test)]
mod tests {
    use {super::*, crate::protocol_generated::Constants};

    #[test]
    fn the_allow_list_uses_generated_interface_names() {
        let generated = vec![
            <crate::wayland_objects::wl_compositor::WlCompositor as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wl_subcompositor::WlSubcompositor as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wl_shm::WlShm as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wl_seat::WlSeat as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wl_output::WlOutput as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wl_data_device_manager::WlDataDeviceManager as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wl_fixes::WlFixes as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::xdg_wm_base::XdgWmBase as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zxdg_output_manager_v1::ZxdgOutputManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::xdg_activation_v1::XdgActivationV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::xdg_wm_dialog_v1::XdgWmDialogV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::xdg_system_bell_v1::XdgSystemBellV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::xdg_toplevel_drag_manager_v1::XdgToplevelDragManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::xdg_session_manager_v1::XdgSessionManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_linux_drm_syncobj_manager_v1::WpLinuxDrmSyncobjManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_presentation::WpPresentation as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_viewporter::WpViewporter as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_alpha_modifier_v1::WpAlphaModifierV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_color_manager_v1::WpColorManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_color_representation_manager_v1::WpColorRepresentationManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_tearing_control_manager_v1::WpTearingControlManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_fifo_manager_v1::WpFifoManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_commit_timing_manager_v1::WpCommitTimingManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::ext_background_effect_manager_v1::ExtBackgroundEffectManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zwp_pointer_constraints_v1::ZwpPointerConstraintsV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zwp_pointer_gestures_v1::ZwpPointerGesturesV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zwp_text_input_manager_v3::ZwpTextInputManagerV3 as Constants>::INTERFACE_NAME,
            <crate::wayland_objects::zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1 as Constants>::INTERFACE_NAME,
        ];
        assert_eq!(generated, SANDBOX_ALLOWED_INTERFACES);
    }

    #[test]
    fn hidden_globals_from_the_decision_table_are_hidden() {
        for interface in [
            "ext_idle_notifier_v1",
            "ext_session_lock_manager_v1",
            "zwlr_layer_shell_v1",
            "zwlr_screencopy_manager_v1",
            "ext_image_copy_capture_manager_v1",
            "ext_output_image_capture_source_manager_v1",
            "ext_foreign_toplevel_image_capture_source_manager_v1",
            "ext_foreign_toplevel_list_v1",
            "zwlr_foreign_toplevel_manager_v1",
            "zwlr_data_control_manager_v1",
            "ext_data_control_manager_v1",
            "zwp_virtual_keyboard_manager_v1",
            "zwp_input_method_manager_v2",
            "zwlr_gamma_control_manager_v1",
            "hyprland_ctm_control_manager_v1",
            "ext_workspace_manager_v1",
            "xwayland_shell_v1",
            "wp_security_context_manager_v1",
        ] {
            assert!(!sandbox_may_see(interface), "{interface} must be hidden");
        }
    }

    #[test]
    fn an_unknown_interface_is_hidden() {
        assert!(!sandbox_may_see("zwlr_output_manager_v1"));
        assert!(!sandbox_may_see("wp_drm_lease_device_v1"));
        assert!(!sandbox_may_see("some_future_global_v1"));
    }

    #[test]
    fn allowed_globals_are_visible() {
        assert!(sandbox_may_see("wl_compositor"));
        assert!(sandbox_may_see("wl_output"));
        assert!(sandbox_may_see("xdg_wm_base"));
        assert!(sandbox_may_see("zwp_linux_dmabuf_v1"));
        assert!(sandbox_may_see("zxdg_output_manager_v1"));
        assert!(sandbox_may_see("zwp_keyboard_shortcuts_inhibit_manager_v1"));
    }
}
