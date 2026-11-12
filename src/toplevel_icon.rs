//! Icon pixels and names for one `xdg_toplevel`. Phase 4 reads them over IPC.

use crate::capture::IconSummary;

/// One square icon image. Bytes are premultiplied little-endian BGRA, tightly packed.
#[derive(Debug)]
pub struct IconPixmap {
    pub size: u32,
    pub scale: u32,
    pub argb8888: Box<[u8]>,
}

/// Name plus the buffers a client has supplied. Pixmaps stay sorted by `(size * scale, scale)`.
#[derive(Debug, Default)]
pub struct ToplevelIcon {
    pub name: Option<String>,
    pub pixmaps: Vec<IconPixmap>,
}

impl ToplevelIcon {
    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.pixmaps.is_empty()
    }

    pub(crate) fn summary(&self) -> IconSummary {
        IconSummary {
            name: self.name.clone(),
            sizes: self
                .pixmaps
                .iter()
                .map(|pixmap| (pixmap.size, pixmap.scale))
                .collect(),
        }
    }

    /// Exact `size * scale` match, else the smallest larger, else the largest smaller.
    ///
    /// Ties take the pixmap whose `scale` is closest to `scale`, then the smaller scale.
    pub fn best(&self, size: u32, scale: u32) -> Option<&IconPixmap> {
        let want = u64::from(size).saturating_mul(u64::from(scale));
        let product =
            |pixmap: &IconPixmap| u64::from(pixmap.size).saturating_mul(u64::from(pixmap.scale));
        let target = self
            .pixmaps
            .iter()
            .map(product)
            .find(|product| *product == want)
            .or_else(|| {
                self.pixmaps
                    .iter()
                    .map(product)
                    .filter(|product| *product > want)
                    .min()
            })
            .or_else(|| {
                self.pixmaps
                    .iter()
                    .map(product)
                    .filter(|product| *product < want)
                    .max()
            })?;
        self.pixmaps
            .iter()
            .filter(|pixmap| product(pixmap) == target)
            .min_by_key(|pixmap| (pixmap.scale.abs_diff(scale), pixmap.scale))
    }
}

#[cfg(test)]
mod tests {
    use super::{IconPixmap, ToplevelIcon};

    fn pixmap(size: u32, scale: u32) -> IconPixmap {
        IconPixmap { size, scale, argb8888: Box::new([]) }
    }

    fn icon(pixmaps: Vec<IconPixmap>) -> ToplevelIcon {
        ToplevelIcon { name: None, pixmaps }
    }

    #[test]
    fn best_picks_exact_then_nearest_product_and_breaks_scale_ties() {
        let icons = icon(vec![pixmap(16, 1), pixmap(32, 1), pixmap(16, 2), pixmap(64, 1)]);
        assert_eq!(icons.best(16, 1).map(|pixmap| (pixmap.size, pixmap.scale)), Some((16, 1)));
        assert_eq!(icons.best(24, 1).map(|pixmap| (pixmap.size, pixmap.scale)), Some((32, 1)));
        assert_eq!(icons.best(8, 1).map(|pixmap| (pixmap.size, pixmap.scale)), Some((16, 1)));
        assert_eq!(icons.best(32, 1).map(|pixmap| (pixmap.size, pixmap.scale)), Some((32, 1)));
        assert_eq!(icons.best(16, 2).map(|pixmap| (pixmap.size, pixmap.scale)), Some((16, 2)));

        let tied = icon(vec![pixmap(32, 1), pixmap(16, 2)]);
        assert_eq!(tied.best(20, 2).map(|pixmap| pixmap.scale), Some(2));
        let equally_far = icon(vec![pixmap(48, 1), pixmap(16, 3)]);
        assert_eq!(equally_far.best(24, 2).map(|pixmap| pixmap.scale), Some(1));
        assert!(icon(vec![]).best(16, 1).is_none());
    }
}
