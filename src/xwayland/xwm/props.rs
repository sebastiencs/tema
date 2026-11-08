//! Window properties read at pair time and on `PropertyNotify`.
//!
//! A UTF-8 title replaces `title` and later `WM_NAME` `STRING` values stay unused.
//! `COMPOUND_TEXT` leaves the title unchanged. A missing `WM_HINTS`, or one
//! without `InputHint`, sets `input_hint`. `_NET_WM_STATE` clears `fullscreen`,
//! then sets each flag whose atom is present.

use crate::{
    runner::Redispatcher,
    x11::{get_property, Atom, Atoms, Property, PropertyError, Window},
};

use super::{Wm, WmCtx, WmEvent, WmFailure, WmPhase, Xwayland, XwaylandAction};

const MAX_BYTES: usize = 4096;
const NORMAL_HINTS_LEN: usize = 18;
const INPUT_HINT: u32 = 1 << 0;
const MOTIF_DECORATIONS: usize = 2;

/// Properties M6 and M7 read. The flags are also written back as `_NET_WM_STATE`.
#[allow(dead_code)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WindowProps {
    pub(super) title: Option<String>,
    pub(super) utf8_title: bool,
    pub(super) class: Option<String>,
    pub(super) instance: Option<String>,
    pub(super) role: Option<String>,
    pub(super) transient_for: Option<Window>,
    pub(super) protocols: Vec<Atom>,
    pub(super) input_hint: bool,
    pub(super) normal_hints: Vec<u32>,
    pub(super) motif_decorations: Option<u32>,
    pub(super) window_types: Vec<Atom>,
    pub(super) startup_id: Option<String>,
    pub(super) modal: bool,
    pub(super) fullscreen: bool,
    pub(super) maximized_vert: bool,
    pub(super) maximized_horz: bool,
    pub(super) hidden: bool,
}

impl Default for WindowProps {
    fn default() -> Self {
        Self {
            title: None,
            utf8_title: false,
            class: None,
            instance: None,
            role: None,
            transient_for: None,
            protocols: Vec::new(),
            input_hint: true,
            normal_hints: Vec::new(),
            motif_decorations: None,
            window_types: Vec::new(),
            startup_id: None,
            modal: false,
            fullscreen: false,
            maximized_vert: false,
            maximized_horz: false,
            hidden: false,
        }
    }
}

/// The properties requested at pair time, in request order.
pub(super) fn loads(atoms: &Atoms) -> [(Atom, Atom); 12] {
    [
        (atoms.WM_CLASS, Atom::Any),
        (atoms.WM_NAME, Atom::Any),
        (atoms._NET_WM_NAME, Atom::Any),
        (atoms.WM_TRANSIENT_FOR, atoms.WINDOW),
        (atoms.WM_PROTOCOLS, atoms.ATOM),
        (atoms.WM_HINTS, Atom::Any),
        (atoms.WM_NORMAL_HINTS, atoms.WM_SIZE_HINTS),
        (atoms._MOTIF_WM_HINTS, Atom::Any),
        (atoms._NET_STARTUP_ID, Atom::Any),
        (atoms._NET_WM_STATE, Atom::Any),
        (atoms._NET_WM_WINDOW_TYPE, atoms.ATOM),
        (atoms.WM_WINDOW_ROLE, Atom::Any),
    ]
}

/// Whether this window floats. Decided once, when it becomes managed.
///
/// A window floats when it is modal, has type dialog, utility, toolbar or
/// splash, has a fixed size, or names `WM_TRANSIENT_FOR`.
pub(super) fn wants_floating(props: &WindowProps, atoms: &Atoms) -> bool {
    props.modal
        || props.window_types.iter().any(|ty| {
            *ty == atoms._NET_WM_WINDOW_TYPE_DIALOG
                || *ty == atoms._NET_WM_WINDOW_TYPE_UTILITY
                || *ty == atoms._NET_WM_WINDOW_TYPE_TOOLBAR
                || *ty == atoms._NET_WM_WINDOW_TYPE_SPLASH
        })
        || fixed_size(&props.normal_hints)
        || props.transient_for.is_some()
}

/// Min and max are hint indexes 5 to 8. Both axes count only when their flag is
/// set, both values are positive, and max equals min.
fn fixed_size(hints: &[u32]) -> bool {
    const FLAGS: usize = 0;
    const MIN_WIDTH: usize = 5;
    const MIN_HEIGHT: usize = 6;
    const MAX_WIDTH: usize = 7;
    const MAX_HEIGHT: usize = 8;
    const P_MIN_SIZE: u32 = 1 << 4;
    const P_MAX_SIZE: u32 = 1 << 5;
    let flags = hints.get(FLAGS).copied().unwrap_or(0);
    if flags & P_MIN_SIZE == 0 || flags & P_MAX_SIZE == 0 {
        return false;
    }
    let Some(min_width) = positive_hint(hints.get(MIN_WIDTH).copied()) else { return false };
    let Some(min_height) = positive_hint(hints.get(MIN_HEIGHT).copied()) else { return false };
    let Some(max_width) = positive_hint(hints.get(MAX_WIDTH).copied()) else { return false };
    let Some(max_height) = positive_hint(hints.get(MAX_HEIGHT).copied()) else { return false };
    min_width == max_width && min_height == max_height
}

fn positive_hint(value: Option<u32>) -> Option<i32> {
    let value = i32::try_from(value?).ok()?;
    (value > 0).then_some(value)
}

/// `_NET_WM_STATE` atoms for the flags that are set, in write order.
pub(super) fn net_wm_state(props: &WindowProps, atoms: &Atoms) -> Vec<Atom> {
    let mut state = Vec::new();
    if props.modal {
        state.push(atoms._NET_WM_STATE_MODAL);
    }
    if props.fullscreen {
        state.push(atoms._NET_WM_STATE_FULLSCREEN);
    }
    if props.maximized_vert {
        state.push(atoms._NET_WM_STATE_MAXIMIZED_VERT);
    }
    if props.maximized_horz {
        state.push(atoms._NET_WM_STATE_MAXIMIZED_HORZ);
    }
    if props.hidden {
        state.push(atoms._NET_WM_STATE_HIDDEN);
    }
    state
}

/// Folds one property reply into `props`. A reply error leaves the previous
/// value in place, except `WM_HINTS` (which sets `input_hint`) and
/// `_NET_WM_STATE` (which has already cleared `fullscreen`).
pub(super) fn apply(
    props: &mut WindowProps,
    property: Atom,
    atoms: &Atoms,
    result: Result<Property, PropertyError>,
) {
    if property == atoms.WM_NAME || property == atoms._NET_WM_NAME {
        apply_title(props, atoms, result);
    } else if property == atoms.WM_CLASS {
        apply_class(props, atoms, result);
    } else if property == atoms.WM_WINDOW_ROLE {
        apply_role(props, atoms, result);
    } else if property == atoms.WM_TRANSIENT_FOR {
        apply_transient(props, result);
    } else if property == atoms.WM_PROTOCOLS {
        apply_atoms(&mut props.protocols, result);
    } else if property == atoms.WM_HINTS {
        apply_hints(props, result);
    } else if property == atoms.WM_NORMAL_HINTS {
        apply_normal_hints(props, result);
    } else if property == atoms._MOTIF_WM_HINTS {
        apply_motif(props, result);
    } else if property == atoms._NET_STARTUP_ID {
        apply_startup_id(props, atoms, result);
    } else if property == atoms._NET_WM_STATE {
        apply_state(props, atoms, result);
    } else if property == atoms._NET_WM_WINDOW_TYPE {
        apply_atoms(&mut props.window_types, result);
    }
}

impl Wm {
    pub(super) fn load_properties(
        &mut self,
        window: Window,
        ctx: &WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
        let atoms = ready.atoms;
        for (property, ty) in loads(&atoms) {
            self.fetch(window, property, ty, ctx)?;
        }
        Ok(())
    }

    pub(super) fn on_property_notify(
        &mut self,
        event: crate::x11::PropertyNotify,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        if self.on_selection_property(event, ctx)? {
            return Ok(());
        }
        if self.on_outgoing_delete(event, ctx)? {
            return Ok(());
        }
        if !self.windows.contains_key(&event.window) {
            return Ok(());
        }
        let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
        let atoms = ready.atoms;
        let Some(&(_, ty)) = loads(&atoms)
            .iter()
            .find(|(property, _)| *property == event.atom)
        else {
            return Ok(());
        };
        self.fetch(event.window, event.atom, ty, ctx)
    }

    pub(super) fn on_property_reply(
        &mut self,
        window: Window,
        atom: crate::x11::Atom,
        result: Result<Property, PropertyError>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
        let atoms = ready.atoms;
        let Some(tracked) = self.windows.get_mut(&window) else { return Ok(()) };
        if let Ok(property) = &result {
            if property.truncated {
                log::debug!("X property {atom:?} on {window:?} is truncated");
            }
        }
        apply(&mut tracked.props, atom, &atoms, result);
        let finished = if tracked.props_outstanding == 0 {
            false
        } else {
            tracked.props_outstanding -= 1;
            tracked.props_outstanding == 0
        };
        drop(tracked);
        self.note_property_batch(window, finished, ctx);
        Ok(())
    }

    fn fetch(
        &mut self,
        window: Window,
        property: Atom,
        ty: Atom,
        ctx: &WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let run = ctx.run;
        let redispatch = Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
            run,
            event: WmEvent::Property { window, atom: property, result },
        });
        match get_property(&mut self.conn, window, property, ty, MAX_BYTES, redispatch) {
            Ok(_) => {
                if let Some(tracked) = self.windows.get_mut(&window) {
                    tracked.props_outstanding = tracked.props_outstanding.saturating_add(1);
                }
                Ok(())
            }
            Err(error) => {
                self.phase = WmPhase::Failed;
                Err(WmFailure { reason: error.to_string() })
            }
        }
    }
}

fn apply_title(props: &mut WindowProps, atoms: &Atoms, result: Result<Property, PropertyError>) {
    let Some(text) = accepted_text(atoms, result) else { return };
    match text {
        Accepted::String(title) => {
            if props.utf8_title {
                return;
            }
            props.title = Some(title);
        }
        Accepted::Utf8(title) => {
            props.utf8_title = true;
            props.title = Some(title);
        }
    }
}

fn apply_class(props: &mut WindowProps, atoms: &Atoms, result: Result<Property, PropertyError>) {
    match text_update(atoms, result) {
        TextUpdate::Missing => {
            props.instance = None;
            props.class = None;
        }
        TextUpdate::Value(Accepted::String(text) | Accepted::Utf8(text)) => {
            let mut parts = text.split('\0');
            props.instance = Some(parts.next().unwrap_or("").to_string());
            props.class = Some(parts.next().unwrap_or("").to_string());
        }
        TextUpdate::Keep => {}
    }
}

fn apply_role(props: &mut WindowProps, atoms: &Atoms, result: Result<Property, PropertyError>) {
    match text_update(atoms, result) {
        TextUpdate::Missing => props.role = None,
        TextUpdate::Value(Accepted::String(text) | Accepted::Utf8(text)) => {
            props.role = Some(text);
        }
        TextUpdate::Keep => {}
    }
}

fn apply_startup_id(
    props: &mut WindowProps,
    atoms: &Atoms,
    result: Result<Property, PropertyError>,
) {
    if let TextUpdate::Value(Accepted::String(text) | Accepted::Utf8(text)) =
        text_update(atoms, result)
    {
        props.startup_id = Some(text);
    }
}

fn apply_transient(props: &mut WindowProps, result: Result<Property, PropertyError>) {
    props.transient_for = match result {
        Ok(property) if property.format != 0 => property
            .as_windows()
            .ok()
            .and_then(|windows| windows.into_iter().next()),
        _ => None,
    };
}

fn apply_atoms(slot: &mut Vec<Atom>, result: Result<Property, PropertyError>) {
    let Ok(property) = result else { return };
    if property.format == 0 {
        return;
    }
    if let Ok(atoms) = property.as_atoms() {
        *slot = atoms;
    }
}

fn apply_hints(props: &mut WindowProps, result: Result<Property, PropertyError>) {
    let Ok(property) = result else {
        props.input_hint = true;
        return;
    };
    if property.format == 0 {
        props.input_hint = true;
        return;
    }
    let Ok(words) = property.as_u32s() else {
        props.input_hint = true;
        return;
    };
    let flags = words.first().copied().unwrap_or(0);
    if flags & INPUT_HINT == 0 {
        props.input_hint = true;
        return;
    }
    props.input_hint = words.get(1).copied().unwrap_or(0) != 0;
}

fn apply_normal_hints(props: &mut WindowProps, result: Result<Property, PropertyError>) {
    let Ok(property) = result else { return };
    if property.format == 0 {
        return;
    }
    let Ok(mut words) = property.as_u32s() else { return };
    words.truncate(NORMAL_HINTS_LEN);
    props.normal_hints = words;
}

fn apply_motif(props: &mut WindowProps, result: Result<Property, PropertyError>) {
    let Ok(property) = result else { return };
    if property.format == 0 {
        return;
    }
    let Ok(words) = property.as_u32s() else { return };
    props.motif_decorations = Some(words.get(MOTIF_DECORATIONS).copied().unwrap_or(0));
}

fn apply_state(props: &mut WindowProps, atoms: &Atoms, result: Result<Property, PropertyError>) {
    props.fullscreen = false;
    let Ok(property) = result else { return };
    if property.format == 0 {
        return;
    }
    let Ok(words) = property.as_atoms() else { return };
    for atom in words {
        if atom == atoms._NET_WM_STATE_MODAL {
            props.modal = true;
        } else if atom == atoms._NET_WM_STATE_FULLSCREEN {
            props.fullscreen = true;
        } else if atom == atoms._NET_WM_STATE_MAXIMIZED_VERT {
            props.maximized_vert = true;
        } else if atom == atoms._NET_WM_STATE_MAXIMIZED_HORZ {
            props.maximized_horz = true;
        } else if atom == atoms._NET_WM_STATE_HIDDEN {
            props.hidden = true;
        }
    }
}

enum Accepted {
    String(String),
    Utf8(String),
}

enum TextUpdate {
    Missing,
    Keep,
    Value(Accepted),
}

fn accepted_text(atoms: &Atoms, result: Result<Property, PropertyError>) -> Option<Accepted> {
    match text_update(atoms, result) {
        TextUpdate::Value(text) => Some(text),
        TextUpdate::Missing | TextUpdate::Keep => None,
    }
}

fn text_update(atoms: &Atoms, result: Result<Property, PropertyError>) -> TextUpdate {
    let Ok(property) = result else { return TextUpdate::Keep };
    if property.format == 0 {
        return TextUpdate::Missing;
    }
    if property.r#type == atoms.COMPOUND_TEXT {
        return TextUpdate::Keep;
    }
    if property.r#type == atoms.STRING {
        return match property.as_latin1() {
            Ok(text) => TextUpdate::Value(Accepted::String(text)),
            Err(_) => TextUpdate::Keep,
        };
    }
    if property.r#type == atoms.UTF8_STRING {
        return match property.as_utf8() {
            Ok(text) => TextUpdate::Value(Accepted::Utf8(text)),
            Err(_) => TextUpdate::Keep,
        };
    }
    TextUpdate::Keep
}

#[cfg(test)]
mod tests {
    use crate::x11::{Atom, Atoms, Property, PropertyError, Window, XReplyError, NAMES};

    use super::{apply, WindowProps};

    fn atoms() -> Atoms {
        let values: Vec<Atom> = (0..NAMES.len() as u32)
            .map(|index| Atom(1000 + index))
            .collect();
        Atoms::from_interned(&values)
    }

    fn words(values: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in values {
            bytes.extend(value.to_ne_bytes());
        }
        bytes
    }

    fn property(ty: Atom, format: u8, data: Vec<u8>) -> Result<Property, PropertyError> {
        Ok(Property { r#type: ty, format, data, truncated: false })
    }

    fn text_property(ty: Atom, bytes: &[u8]) -> Result<Property, PropertyError> {
        property(ty, 8, bytes.to_vec())
    }

    #[test]
    fn titles_keep_utf8_and_ignore_compound_text() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        apply(&mut props, atoms.WM_NAME, &atoms, text_property(atoms.STRING, &[0xe9]));
        assert_eq!(props.title.as_deref(), Some("é"));
        assert!(!props.utf8_title);

        apply(&mut props, atoms._NET_WM_NAME, &atoms, text_property(atoms.UTF8_STRING, b"utf"));
        assert_eq!(props.title.as_deref(), Some("utf"));
        assert!(props.utf8_title);

        apply(&mut props, atoms.WM_NAME, &atoms, text_property(atoms.STRING, b"latin"));
        assert_eq!(props.title.as_deref(), Some("utf"));

        apply(
            &mut props,
            atoms._NET_WM_NAME,
            &atoms,
            text_property(atoms.COMPOUND_TEXT, b"compound"),
        );
        assert_eq!(props.title.as_deref(), Some("utf"));

        apply(&mut props, atoms.WM_NAME, &atoms, property(atoms.CARDINAL, 32, words(&[1])));
        assert_eq!(props.title.as_deref(), Some("utf"));

        apply(&mut props, atoms.WM_NAME, &atoms, property(Atom::Any, 0, Vec::new()));
        assert_eq!(props.title.as_deref(), Some("utf"));
        assert!(props.utf8_title);
    }

    #[test]
    fn class_splits_on_nul_and_a_missing_property_clears_it() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        apply(&mut props, atoms.WM_CLASS, &atoms, text_property(atoms.UTF8_STRING, b"inst\0App\0"));
        assert_eq!(props.instance.as_deref(), Some("inst"));
        assert_eq!(props.class.as_deref(), Some("App"));

        apply(&mut props, atoms.WM_CLASS, &atoms, Err(PropertyError::Reply(XReplyError::Dead)));
        assert_eq!(props.instance.as_deref(), Some("inst"));

        apply(&mut props, atoms.WM_CLASS, &atoms, property(Atom::Any, 0, Vec::new()));
        assert!(props.instance.is_none());
        assert!(props.class.is_none());
    }

    #[test]
    fn role_accepts_string_and_a_missing_property_clears_it() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        apply(&mut props, atoms.WM_WINDOW_ROLE, &atoms, text_property(atoms.STRING, b"pop"));
        assert_eq!(props.role.as_deref(), Some("pop"));
        apply(&mut props, atoms.WM_WINDOW_ROLE, &atoms, property(Atom::Any, 0, Vec::new()));
        assert!(props.role.is_none());
    }

    #[test]
    fn transient_for_keeps_the_first_window() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        let parent = Window(7);
        let other = Window(8);
        apply(
            &mut props,
            atoms.WM_TRANSIENT_FOR,
            &atoms,
            property(atoms.WINDOW, 32, words(&[parent.0, other.0])),
        );
        assert_eq!(props.transient_for, Some(parent));
        apply(
            &mut props,
            atoms.WM_TRANSIENT_FOR,
            &atoms,
            Err(PropertyError::Reply(XReplyError::Dead)),
        );
        assert!(props.transient_for.is_none());
    }

    #[test]
    fn protocols_and_window_types_replace_the_atom_list() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        props.protocols = vec![atoms.WM_TAKE_FOCUS];
        apply(&mut props, atoms.WM_PROTOCOLS, &atoms, property(Atom::Any, 0, Vec::new()));
        assert_eq!(props.protocols, vec![atoms.WM_TAKE_FOCUS]);
        apply(
            &mut props,
            atoms.WM_PROTOCOLS,
            &atoms,
            property(atoms.ATOM, 32, words(&[atoms.WM_DELETE_WINDOW.0, atoms.WM_TAKE_FOCUS.0])),
        );
        assert_eq!(props.protocols, vec![atoms.WM_DELETE_WINDOW, atoms.WM_TAKE_FOCUS]);
        apply(
            &mut props,
            atoms._NET_WM_WINDOW_TYPE,
            &atoms,
            property(atoms.ATOM, 32, words(&[atoms._NET_WM_WINDOW_TYPE_DIALOG.0])),
        );
        assert_eq!(props.window_types, vec![atoms._NET_WM_WINDOW_TYPE_DIALOG]);
    }

    #[test]
    fn hints_default_to_input_when_the_flag_or_the_property_is_missing() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        apply(&mut props, atoms.WM_HINTS, &atoms, property(atoms.WM_HINTS, 32, words(&[1, 0])));
        assert!(!props.input_hint);
        apply(&mut props, atoms.WM_HINTS, &atoms, property(atoms.WM_HINTS, 32, words(&[0, 0])));
        assert!(props.input_hint);
        apply(&mut props, atoms.WM_HINTS, &atoms, property(Atom::Any, 0, Vec::new()));
        assert!(props.input_hint);
        props.input_hint = false;
        apply(&mut props, atoms.WM_HINTS, &atoms, Err(PropertyError::Reply(XReplyError::Dead)));
        assert!(props.input_hint);
    }

    #[test]
    fn normal_hints_keep_at_most_eighteen_words() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        let mut long = vec![1; 20];
        long[0] = 9;
        apply(
            &mut props,
            atoms.WM_NORMAL_HINTS,
            &atoms,
            property(atoms.WM_SIZE_HINTS, 32, words(&long)),
        );
        assert_eq!(props.normal_hints.len(), 18);
        assert_eq!(props.normal_hints[0], 9);
        apply(&mut props, atoms.WM_NORMAL_HINTS, &atoms, property(Atom::Any, 0, Vec::new()));
        assert_eq!(props.normal_hints.len(), 18);
        apply(
            &mut props,
            atoms.WM_NORMAL_HINTS,
            &atoms,
            property(atoms.WM_SIZE_HINTS, 32, words(&[4, 5])),
        );
        assert_eq!(props.normal_hints, vec![4, 5]);
    }

    #[test]
    fn motif_decorations_are_the_third_word() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        apply(
            &mut props,
            atoms._MOTIF_WM_HINTS,
            &atoms,
            property(Atom::Any, 32, words(&[2, 0, 8, 0, 0])),
        );
        assert_eq!(props.motif_decorations, Some(8));
        apply(&mut props, atoms._MOTIF_WM_HINTS, &atoms, property(Atom::Any, 0, Vec::new()));
        assert_eq!(props.motif_decorations, Some(8));
        apply(&mut props, atoms._MOTIF_WM_HINTS, &atoms, property(Atom::Any, 32, words(&[1])));
        assert_eq!(props.motif_decorations, Some(0));
    }

    #[test]
    fn startup_id_keeps_string_and_utf8() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        apply(&mut props, atoms._NET_STARTUP_ID, &atoms, text_property(atoms.STRING, b"id"));
        assert_eq!(props.startup_id.as_deref(), Some("id"));
        apply(&mut props, atoms._NET_STARTUP_ID, &atoms, property(Atom::Any, 0, Vec::new()));
        assert_eq!(props.startup_id.as_deref(), Some("id"));
    }

    #[test]
    fn net_wm_state_clears_fullscreen_before_applying_the_atoms() {
        let atoms = atoms();
        let mut props = WindowProps::default();
        props.modal = true;
        props.fullscreen = true;
        props.maximized_horz = true;
        apply(
            &mut props,
            atoms._NET_WM_STATE,
            &atoms,
            property(atoms.ATOM, 32, words(&[atoms._NET_WM_STATE_MODAL.0])),
        );
        assert!(props.modal);
        assert!(!props.fullscreen);
        assert!(props.maximized_horz);
        apply(&mut props, atoms._NET_WM_STATE, &atoms, property(Atom::Any, 0, Vec::new()));
        assert!(props.modal);
        assert!(!props.fullscreen);
        props.fullscreen = true;
        apply(
            &mut props,
            atoms._NET_WM_STATE,
            &atoms,
            Err(PropertyError::Reply(XReplyError::Dead)),
        );
        assert!(!props.fullscreen);
        assert!(props.modal);
    }
}
