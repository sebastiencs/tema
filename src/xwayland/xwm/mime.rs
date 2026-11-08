//! MIME types on the selection bridge.
//!
//! `UTF8_STRING` is `text/plain;charset=utf-8` and `STRING` is `text/plain`.
//! Any other target whose atom name contains `/` is that name. A UTF-8 offer
//! also advertises `TEXT`.

use std::collections::BTreeSet;

use crate::x11::{Atom, Atoms};

/// One X target derived from a Wayland MIME type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TargetName {
    Utf8,
    /// Advertised beside [`Self::Utf8`] and answered with the same bytes.
    Text,
    Latin1,
    Named(Vec<u8>),
}

/// The MIME type a Wayland client should see for this X target.
pub(super) fn mime_for_target(atom: Atom, name: Option<&[u8]>, atoms: &Atoms) -> Option<String> {
    if atom == atoms.UTF8_STRING {
        return Some("text/plain;charset=utf-8".to_string());
    }
    if atom == atoms.STRING {
        return Some("text/plain".to_string());
    }
    let name = name?;
    if !name.contains(&b'/') {
        return None;
    }
    String::from_utf8(name.to_vec()).ok()
}

/// MIME types for a `TARGETS` list. Duplicates are dropped and the order is kept.
pub(super) fn x_mime_types(targets: &[(Atom, Option<Vec<u8>>)], atoms: &Atoms) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut mime_types = Vec::new();
    for (atom, name) in targets {
        let Some(mime_type) = mime_for_target(*atom, name.as_deref(), atoms) else {
            continue;
        };
        if seen.insert(mime_type.clone()) {
            mime_types.push(mime_type);
        }
    }
    mime_types
}

/// X targets for the MIME types a Wayland source offered.
pub(super) fn target_names_for_mimes(mimes: &[String]) -> Vec<TargetName> {
    let mut seen = BTreeSet::new();
    let mut names = Vec::new();
    for mime in mimes {
        if mime == "text/plain;charset=utf-8" {
            push_unique(&mut names, &mut seen, TargetName::Utf8);
            push_unique(&mut names, &mut seen, TargetName::Text);
        } else if mime == "text/plain" {
            push_unique(&mut names, &mut seen, TargetName::Latin1);
        } else {
            push_unique(&mut names, &mut seen, TargetName::Named(mime.as_bytes().to_vec()));
        }
    }
    names
}

fn push_unique(names: &mut Vec<TargetName>, seen: &mut BTreeSet<String>, name: TargetName) {
    let key = match &name {
        TargetName::Utf8 => "utf8".to_string(),
        TargetName::Text => "text".to_string(),
        TargetName::Latin1 => "latin1".to_string(),
        TargetName::Named(bytes) => format!("named:{}", String::from_utf8_lossy(bytes)),
    };
    if seen.insert(key) {
        names.push(name);
    }
}

#[cfg(test)]
mod tests {
    use crate::x11::{Atom, Atoms, NAMES};

    use super::{mime_for_target, target_names_for_mimes, x_mime_types, TargetName};

    fn atoms() -> Atoms {
        let values: Vec<Atom> = (0..NAMES.len() as u32).map(Atom).collect();
        Atoms::from_interned(&values)
    }

    #[test]
    fn known_atoms_and_slash_names_become_mime_types() {
        let atoms = atoms();
        assert_eq!(
            mime_for_target(atoms.UTF8_STRING, None, &atoms).as_deref(),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(
            mime_for_target(atoms.STRING, Some(b"ignored"), &atoms).as_deref(),
            Some("text/plain")
        );
        assert_eq!(
            mime_for_target(Atom(400), Some(b"image/png"), &atoms).as_deref(),
            Some("image/png")
        );
        assert_eq!(mime_for_target(atoms.TARGETS, Some(b"TARGETS"), &atoms), None);
        assert_eq!(mime_for_target(Atom(401), None, &atoms), None);
        assert_eq!(mime_for_target(Atom(402), Some(b"image/\xff"), &atoms), None);
    }

    #[test]
    fn target_lists_keep_the_first_mime_type() {
        let atoms = atoms();
        let png = Atom(400);
        let listed = x_mime_types(
            &[
                (atoms.TARGETS, Some(b"TARGETS".to_vec())),
                (atoms.UTF8_STRING, None),
                (atoms.STRING, None),
                (png, Some(b"image/png".to_vec())),
                (atoms.UTF8_STRING, None),
                (Atom(401), Some(b"text/plain;charset=utf-8".to_vec())),
                (atoms.TIMESTAMP, None),
            ],
            &atoms,
        );
        assert_eq!(
            listed,
            vec![
                "text/plain;charset=utf-8".to_string(),
                "text/plain".to_string(),
                "image/png".to_string(),
            ]
        );
    }

    #[test]
    fn utf8_offers_text_beside_itself() {
        assert_eq!(
            target_names_for_mimes(&[
                "text/plain".to_string(),
                "text/plain;charset=utf-8".to_string(),
                "image/png".to_string(),
                "text/plain;charset=utf-8".to_string(),
                "image/png".to_string(),
            ]),
            vec![
                TargetName::Latin1,
                TargetName::Utf8,
                TargetName::Text,
                TargetName::Named(b"image/png".to_vec()),
            ]
        );
        assert_eq!(target_names_for_mimes(&["text/plain".to_string()]), vec![TargetName::Latin1]);
    }
}
