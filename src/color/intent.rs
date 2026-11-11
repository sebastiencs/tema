/// Render intents this compositor will advertise.
///
/// `saturation` and `absolute` are protocol values that are not advertised.
/// `from_wayland` returns `None` for them. Perceptual uses the same matrix as
/// `relative_bpc`, then the ICtCp shoulder when the source peak exceeds the
/// destination. The other intents clip.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum RenderIntent {
    Perceptual,
    Relative,
    RelativeBpc,
    AbsoluteNoAdaptation,
}

impl RenderIntent {
    pub const ABSOLUTE_NO_ADAPTATION_SINCE: u32 = 2;

    pub fn from_wayland(value: u32, version: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Perceptual),
            1 => Some(Self::Relative),
            4 => Some(Self::RelativeBpc),
            5 if version >= Self::ABSOLUTE_NO_ADAPTATION_SINCE => Some(Self::AbsoluteNoAdaptation),
            _ => None,
        }
    }

    pub fn compensates_black_point(self) -> bool {
        matches!(self, Self::Perceptual | Self::RelativeBpc)
    }

    pub fn adapts_white_point(self) -> bool {
        !matches!(self, Self::AbsoluteNoAdaptation)
    }
}

#[cfg(test)]
mod tests {
    use super::RenderIntent;

    #[test]
    fn from_wayland_accepts_advertised_intents_only() {
        assert_eq!(RenderIntent::from_wayland(0, 1), Some(RenderIntent::Perceptual));
        assert_eq!(RenderIntent::from_wayland(1, 1), Some(RenderIntent::Relative));
        assert_eq!(RenderIntent::from_wayland(4, 1), Some(RenderIntent::RelativeBpc));
        assert_eq!(RenderIntent::from_wayland(2, 3), None);
        assert_eq!(RenderIntent::from_wayland(3, 3), None);
        assert_eq!(RenderIntent::from_wayland(5, 1), None);
        assert_eq!(RenderIntent::from_wayland(5, 2), Some(RenderIntent::AbsoluteNoAdaptation));
        assert_eq!(RenderIntent::from_wayland(9, 3), None);
    }

    #[test]
    fn perceptual_matches_relative_bpc_flags() {
        for intent in [RenderIntent::Perceptual, RenderIntent::RelativeBpc] {
            assert!(intent.compensates_black_point());
            assert!(intent.adapts_white_point());
        }
        assert!(!RenderIntent::Relative.compensates_black_point());
        assert!(RenderIntent::Relative.adapts_white_point());
        assert!(!RenderIntent::AbsoluteNoAdaptation.compensates_black_point());
        assert!(!RenderIntent::AbsoluteNoAdaptation.adapts_white_point());
    }
}
