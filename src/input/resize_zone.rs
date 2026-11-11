//! Floating-window resize zones. No [`crate::state::State`] access.

use crate::{
    config::RoundingPower, protocol_generated::xdg_toplevel::ResizeEdge, region::RectI,
    state::RootSurfaceClip,
};

/// Extra length added to `rounding + border` for a floating corner zone.
pub(crate) const RESIZE_CORNER_LEEWAY: i32 = 10;

/// Geometry for [`floating_resize_edges`]. Rects are in the same space as `pos`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameZoneParams {
    pub frame: RectI,
    pub content: RectI,
    pub extent: i32,
    pub corner_len: i32,
    pub content_clip: Option<RootSurfaceClip>,
}

pub(crate) fn corner_len(rounding: i32, border: i32) -> i32 {
    rounding
        .max(0)
        .saturating_add(border.max(0))
        .saturating_add(RESIZE_CORNER_LEEWAY)
}

/// Resize edges at `pos`, or `None` when the compositor should leave the point alone.
///
/// Comparisons stay in `f64`. The grab is `frame` inflated by `extent`. Inside the
/// content box, only a transparent rounded corner (outside `content_clip`) is a zone.
pub(crate) fn floating_resize_edges(params: &FrameZoneParams, pos: (f64, f64)) -> Option<u32> {
    let extent = f64::from(params.extent.max(0));
    let frame_left = f64::from(params.frame.x);
    let frame_top = f64::from(params.frame.y);
    let frame_right = frame_left + f64::from(params.frame.width.max(0));
    let frame_bottom = frame_top + f64::from(params.frame.height.max(0));
    if pos.0 < frame_left - extent
        || pos.0 >= frame_right + extent
        || pos.1 < frame_top - extent
        || pos.1 >= frame_bottom + extent
    {
        return None;
    }

    let content = params.content;
    let left = f64::from(content.x);
    let top = f64::from(content.y);
    let right = left + f64::from(content.width.max(0));
    let bottom = top + f64::from(content.height.max(0));
    let k = f64::from(params.corner_len.max(0))
        .min(f64::from(content.width.max(0)) / 2.0)
        .min(f64::from(content.height.max(0)) / 2.0);

    if pos.0 >= left && pos.0 < right && pos.1 >= top && pos.1 < bottom {
        let outside_clip = params
            .content_clip
            .is_some_and(|clip| !clip.contains_point(pos.0, pos.1));
        if !outside_clip {
            return None;
        }
        let on_left = pos.0 < left + (right - left) / 2.0;
        let on_top = pos.1 < top + (bottom - top) / 2.0;
        return Some(edge_bits(on_top, !on_top, on_left, !on_left));
    }

    if pos.1 < top {
        return Some(edge_bits(true, false, pos.0 < left + k, pos.0 >= right - k));
    }
    if pos.1 >= bottom {
        return Some(edge_bits(false, true, pos.0 < left + k, pos.0 >= right - k));
    }
    Some(edge_bits(pos.1 < top + k, pos.1 >= bottom - k, pos.0 < left, pos.0 >= left))
}

fn edge_bits(top: bool, bottom: bool, left: bool, right: bool) -> u32 {
    let mut edges = 0;
    if top {
        edges |= ResizeEdge::Top as u32;
    }
    if bottom {
        edges |= ResizeEdge::Bottom as u32;
    }
    if left {
        edges |= ResizeEdge::Left as u32;
    }
    if right {
        edges |= ResizeEdge::Right as u32;
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(extent: i32, corner_len: i32, clip: Option<RootSurfaceClip>) -> FrameZoneParams {
        FrameZoneParams {
            frame: RectI { x: 0, y: 0, width: 100, height: 80 },
            content: RectI { x: 10, y: 10, width: 80, height: 60 },
            extent,
            corner_len,
            content_clip: clip,
        }
    }

    fn edges(params: &FrameZoneParams, x: f64, y: f64) -> Option<u32> {
        floating_resize_edges(params, (x, y))
    }

    #[test]
    fn every_edge_and_corner_of_the_frame() {
        let zone = params(10, 20, None);
        assert_eq!(edges(&zone, 50.0, 5.0), Some(ResizeEdge::Top as u32));
        assert_eq!(edges(&zone, 50.0, 75.0), Some(ResizeEdge::Bottom as u32));
        assert_eq!(edges(&zone, 5.0, 40.0), Some(ResizeEdge::Left as u32));
        assert_eq!(edges(&zone, 95.0, 40.0), Some(ResizeEdge::Right as u32));
        assert_eq!(edges(&zone, 5.0, 5.0), Some(ResizeEdge::TopLeft as u32));
        assert_eq!(edges(&zone, 95.0, 5.0), Some(ResizeEdge::TopRight as u32));
        assert_eq!(edges(&zone, 5.0, 75.0), Some(ResizeEdge::BottomLeft as u32));
        assert_eq!(edges(&zone, 95.0, 75.0), Some(ResizeEdge::BottomRight as u32));
        assert_eq!(edges(&zone, 50.0, 40.0), None);
    }

    #[test]
    fn extent_outside_the_frame_is_a_zone_and_past_it_is_not() {
        let zone = params(10, 20, None);
        assert_eq!(edges(&zone, -5.0, 40.0), Some(ResizeEdge::Left as u32));
        assert_eq!(edges(&zone, 50.0, -5.0), Some(ResizeEdge::Top as u32));
        assert_eq!(edges(&zone, -11.0, 40.0), None);
        assert_eq!(edges(&zone, 50.0, 90.0), None);
    }

    #[test]
    fn zero_border_still_has_an_extent_zone() {
        let zone = FrameZoneParams {
            frame: RectI { x: 0, y: 0, width: 100, height: 80 },
            content: RectI { x: 0, y: 0, width: 100, height: 80 },
            extent: 10,
            corner_len: 10,
            content_clip: None,
        };
        assert_eq!(edges(&zone, -5.0, 40.0), Some(ResizeEdge::Left as u32));
        assert_eq!(edges(&zone, 50.0, 40.0), None);
    }

    #[test]
    fn curved_corner_inside_content_is_a_zone_and_the_interior_is_not() {
        let content = RectI { x: 0, y: 0, width: 100, height: 100 };
        let clip = RootSurfaceClip::new(content, 20, RoundingPower::DEFAULT);
        let zone = FrameZoneParams {
            frame: content,
            content,
            extent: 10,
            corner_len: 30,
            content_clip: Some(clip),
        };
        assert!(!clip.contains_point(1.0, 1.0));
        assert_eq!(edges(&zone, 1.0, 1.0), Some(ResizeEdge::TopLeft as u32));
        assert_eq!(edges(&zone, 50.0, 50.0), None);
    }

    #[test]
    fn a_small_window_clamps_the_corner_length() {
        let small = FrameZoneParams {
            frame: RectI { x: 0, y: 0, width: 30, height: 30 },
            content: RectI { x: 0, y: 0, width: 30, height: 30 },
            extent: 10,
            corner_len: 40,
            content_clip: None,
        };
        assert_eq!(edges(&small, 14.0, -1.0), Some(ResizeEdge::TopLeft as u32));
        assert_eq!(edges(&small, 15.0, -1.0), Some(ResizeEdge::TopRight as u32));

        let wide = FrameZoneParams {
            frame: RectI { x: 0, y: 0, width: 200, height: 80 },
            content: RectI { x: 0, y: 0, width: 200, height: 80 },
            extent: 10,
            corner_len: 40,
            content_clip: None,
        };
        assert_eq!(edges(&wide, 100.0, -1.0), Some(ResizeEdge::Top as u32));
    }

    #[test]
    fn corner_len_is_rounding_plus_border_plus_leeway() {
        assert_eq!(corner_len(20, 10), 40);
        assert_eq!(corner_len(-3, -4), RESIZE_CORNER_LEEWAY);
    }
}
