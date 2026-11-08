//! `RESOURCE_MANAGER` hints for downscaled X clients.
//!
//! In downscaled mode the root property gains `Xft.dpi` and `Xcursor.size` at
//! `96 * S` and `cursor_size * S`. Other lines stay. Default mode leaves the
//! property alone until a previous downscaled write has to be put back.

use crate::{
    config::XwaylandScalingMode,
    runner::Redispatcher,
    x11::{change_property_u8s, get_property, Atom, Property, PropertyError},
};

use super::{Wm, WmCtx, WmEvent, WmFailure, WmPhase, Xwayland, XwaylandAction};

const MAX_BYTES: usize = 65_536;

/// Replaces `Xft.dpi` and `Xcursor.size` and keeps every other line.
///
/// Comments and continuation lines stay. A missing key is appended. The result
/// ends with `\n`.
pub(super) fn merge_resources(existing: &str, set: &[(&str, String)]) -> String {
    let mut pending: Vec<(&str, &str)> = set
        .iter()
        .map(|(key, value)| (*key, value.as_str()))
        .collect();
    let mut merged = String::new();
    let mut continued = false;
    for line in resource_lines(existing) {
        if continued {
            push_line(&mut merged, line);
            continued = line_continues(line);
            continue;
        }
        if let Some(key) = assignment_key(line) {
            if let Some(index) = pending.iter().position(|(name, _)| *name == key) {
                let (name, value) = pending.remove(index);
                push_line(&mut merged, &format!("{name}:\t{value}"));
                continued = false;
                continue;
            }
        }
        push_line(&mut merged, line);
        continued = line_continues(line);
    }
    for (name, value) in pending {
        push_line(&mut merged, &format!("{name}:\t{value}"));
    }
    if !merged.ends_with('\n') {
        merged.push('\n');
    }
    merged
}

impl Wm {
    /// Read `RESOURCE_MANAGER` when the hints should change, then refresh the root cursor.
    ///
    /// The property read is queued before the cursor's picture-format query, so
    /// that query stays the last request of a ready batch.
    pub(super) fn refresh_hints(&mut self, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        self.request_resource_hints(ctx)?;
        self.begin_cursor(ctx)
    }

    fn request_resource_hints(&mut self, ctx: &mut WmCtx<'_>) -> Result<(), WmFailure> {
        let WmPhase::Ready(ready) = &mut self.phase else { return Ok(()) };
        ready.hints_fetch = ready.hints_fetch.saturating_add(1);
        let generation = ready.hints_fetch;
        let downscaled = ctx.state.config.xwayland.scaling_mode == XwaylandScalingMode::Downscaled;
        if !downscaled && !ready.hints_written {
            return Ok(());
        }
        let root = ready.root;
        let run = ctx.run;
        let redispatch = Redispatcher::to::<Xwayland>(move |result| XwaylandAction::Wm {
            run,
            event: WmEvent::Resources { generation, result },
        });
        match get_property(
            &mut self.conn,
            root,
            Atom::RESOURCE_MANAGER,
            Atom::STRING,
            MAX_BYTES,
            redispatch,
        ) {
            Ok(_) => Ok(()),
            Err(error) => {
                self.phase = WmPhase::Failed;
                Err(WmFailure { reason: error.to_string() })
            }
        }
    }

    pub(super) fn on_resources(
        &mut self,
        generation: u64,
        result: Result<Property, PropertyError>,
        ctx: &mut WmCtx<'_>,
    ) -> Result<(), WmFailure> {
        let WmPhase::Ready(ready) = &self.phase else { return Ok(()) };
        if generation != ready.hints_fetch {
            return Ok(());
        }
        let root = ready.root;
        let Some(existing) = resource_text(&result) else {
            log::debug!("leaving RESOURCE_MANAGER unchanged");
            return Ok(());
        };
        let downscaled = ctx.state.config.xwayland.scaling_mode == XwaylandScalingMode::Downscaled;
        let cursor = ctx.state.config.general.cursor_size.max(1);
        let scale = ctx.wire_scale().get();
        let (dpi, cursor_px) = if downscaled {
            (96u32.saturating_mul(scale), cursor.saturating_mul(scale))
        } else {
            (96, cursor)
        };
        let merged = merge_resources(
            &existing,
            &[("Xft.dpi", dpi.to_string()), ("Xcursor.size", cursor_px.to_string())],
        );
        let change =
            change_property_u8s(root, Atom::RESOURCE_MANAGER, Atom::STRING, merged.as_bytes());
        self.send_request(&change.request())?;
        if let WmPhase::Ready(ready) = &mut self.phase {
            ready.hints_written = downscaled;
        }
        Ok(())
    }
}

fn resource_text(result: &Result<Property, PropertyError>) -> Option<String> {
    let property = match result {
        Ok(property) => property,
        Err(error) => {
            log::debug!("RESOURCE_MANAGER read failed: {error}");
            return None;
        }
    };
    if property.format == 0 {
        return Some(String::new());
    }
    if property.truncated || property.format != 8 {
        return None;
    }
    let mut text = property.as_latin1().ok()?;
    while text.ends_with('\0') {
        text.pop();
    }
    Some(text)
}

fn resource_lines(existing: &str) -> Vec<&str> {
    if existing.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = existing.split('\n').collect();
    if existing.ends_with('\n') {
        lines.pop();
    }
    lines
}

fn assignment_key(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.starts_with('!') || trimmed.starts_with('#') {
        return None;
    }
    let (key, _) = trimmed.split_once(':')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

fn line_continues(line: &str) -> bool {
    line.trim_end().ends_with('\\')
}

fn push_line(merged: &mut String, line: &str) {
    merged.push_str(line);
    merged.push('\n');
}

#[cfg(test)]
mod tests {
    use super::merge_resources;

    fn scaled() -> [(&'static str, String); 2] {
        [("Xft.dpi", "192".to_string()), ("Xcursor.size", "48".to_string())]
    }

    #[test]
    fn an_empty_property_gains_both_keys() {
        assert_eq!(merge_resources("", &scaled()), "Xft.dpi:\t192\nXcursor.size:\t48\n");
    }

    #[test]
    fn other_keys_stay_and_missing_keys_are_appended() {
        let existing = "Xft.antialias:\t1\nXcursor.theme:\tAdwaita\n";
        assert_eq!(
            merge_resources(existing, &scaled()),
            "Xft.antialias:\t1\nXcursor.theme:\tAdwaita\nXft.dpi:\t192\nXcursor.size:\t48\n"
        );
    }

    #[test]
    fn an_existing_dpi_is_replaced_in_place() {
        let existing = "Xft.antialias:\t1\nXft.dpi:\t96\nXcursor.theme:\tAdwaita\n";
        assert_eq!(
            merge_resources(existing, &scaled()),
            "Xft.antialias:\t1\nXft.dpi:\t192\nXcursor.theme:\tAdwaita\nXcursor.size:\t48\n"
        );
    }

    #[test]
    fn comments_and_continuation_lines_stay() {
        let existing = "! dpi follows\nXft.antialias:\t1\\\n\t2\nXft.dpi:\t96\n";
        assert_eq!(
            merge_resources(existing, &scaled()),
            "! dpi follows\nXft.antialias:\t1\\\n\t2\nXft.dpi:\t192\nXcursor.size:\t48\n"
        );
    }
}
