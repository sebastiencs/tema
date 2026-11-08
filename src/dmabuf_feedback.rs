//! Format table shared by every `zwp_linux_dmabuf_feedback_v1` object.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    rc::Rc,
};

use crate::{
    drm::Drm,
    state::{DrmId, OutputId, State},
};

/// One DRM device's sampling formats, in the order `state.drm` yields devices.
#[derive(Clone, Debug)]
pub struct FeedbackDevice {
    pub drm_id: DrmId,
    pub dev: libc::dev_t,
    pub formats: Vec<(u32, u64)>,
}

/// One device's tranche. `indices` point at [`DmabufFeedbackTable`]'s pair list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceTranche {
    pub drm_id: DrmId,
    pub dev: libc::dev_t,
    pub indices: Vec<u16>,
}

/// Sealed 16-byte format/modifier pairs, plus the tranche for each device.
#[derive(Debug)]
pub struct DmabufFeedbackTable {
    pub id: u64,
    pub memfd: Rc<OwnedFd>,
    pub size: u32,
    /// Index in this vec is the `u16` stored in a tranche.
    pub pairs: Vec<(u32, u64)>,
    /// Devices in the order they were given. The first one is the main device.
    pub devices: Vec<DeviceTranche>,
    /// Opaque primary-plane pairs for outputs that can scan a client buffer out.
    pub scanout: Vec<ScanoutTranche>,
}

/// One output's scanout tranche. `indices` point at the shared pair list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanoutTranche {
    pub output_id: OutputId,
    pub drm_id: DrmId,
    pub dev: libc::dev_t,
    pub indices: Vec<u16>,
}

/// Pairs an output's primary plane can scan out. Only pairs that also appear
/// in a sampling tranche are kept.
#[derive(Clone, Debug)]
pub struct ScanoutFormats {
    pub output_id: OutputId,
    pub drm_id: DrmId,
    pub dev: libc::dev_t,
    pub formats: Vec<(u32, u64)>,
}

#[derive(Debug, thiserror::Error)]
pub enum DmabufFeedbackError {
    #[error("format table has {0} pairs, which does not fit in a u16 index")]
    TooManyPairs(usize),
    #[error("could not create the format table memfd")]
    Memfd(#[source] io::Error),
}

/// `dev_t` clients should allocate on. The render node wins; the primary node
/// is only a fallback when the allocator fd is not a character device.
pub fn render_dev_t(drm: &Drm) -> Option<libc::dev_t> {
    advertised_dev_t(drm.allocator_fd().as_raw_fd(), drm.devnum())
}

/// Devices that can both name a `dev_t` and sample at least one format.
pub fn collect_devices(state: &State) -> Vec<FeedbackDevice> {
    state
        .drm
        .iter()
        .filter_map(|(drm_id, drm)| {
            let dev = render_dev_t(drm)?;
            let mut formats = Vec::new();
            for (format, modifiers) in drm.supported_dmabuf_read_formats() {
                for modifier in modifiers {
                    formats.push((format.drm as u32, modifier.0));
                }
            }
            if formats.is_empty() {
                return None;
            }
            Some(FeedbackDevice { drm_id, dev, formats })
        })
        .collect()
}

/// Build a table, or `Ok(None)` when `previous` already describes `devices`.
///
/// `None` means "unchanged", including an empty device list matched against an
/// empty table. A first empty list still returns a table with no pairs, so a
/// later non-empty list can be told apart from "nothing changed".
pub fn build(
    devices: &[FeedbackDevice],
    scanout: &[ScanoutFormats],
    previous: Option<&DmabufFeedbackTable>,
) -> Result<Option<DmabufFeedbackTable>, DmabufFeedbackError> {
    let (pairs, tranches, scanout) = compile(devices, scanout)?;
    if let Some(previous) = previous {
        if previous.pairs == pairs && previous.devices == tranches && previous.scanout == scanout {
            return Ok(None);
        }
    }
    let bytes = encode_pairs(&pairs);
    let size =
        u32::try_from(bytes.len()).map_err(|_| DmabufFeedbackError::TooManyPairs(pairs.len()))?;
    let memfd = seal_memfd(&bytes).map_err(DmabufFeedbackError::Memfd)?;
    let id = previous.map(|table| table.id.wrapping_add(1)).unwrap_or(1);
    Ok(Some(DmabufFeedbackTable { id, memfd, size, pairs, devices: tranches, scanout }))
}

/// Opaque format/modifier pairs the primary plane shares with this device's sample set.
pub fn collect_scanout(state: &State) -> Vec<ScanoutFormats> {
    let mut offers = Vec::new();
    for (output_id, output) in state.outputs.iter() {
        let Some(drm) = state.drm.try_get(output.backend.drm_id) else {
            continue;
        };
        let Some(dev) = render_dev_t(drm) else {
            continue;
        };
        let formats = drm.opaque_primary_scanout_pairs(output.drm_crtc_id);
        if formats.is_empty() {
            continue;
        }
        offers.push(ScanoutFormats { output_id, drm_id: output.backend.drm_id, dev, formats });
    }
    offers
}

/// Sort each device, drop duplicate pairs inside one tranche, and assign each
/// distinct pair the index of its first appearance.
fn compile(
    devices: &[FeedbackDevice],
    scanout: &[ScanoutFormats],
) -> Result<(Vec<(u32, u64)>, Vec<DeviceTranche>, Vec<ScanoutTranche>), DmabufFeedbackError> {
    let mut pairs = Vec::new();
    let mut index_of: HashMap<(u32, u64), u16> = HashMap::new();
    let mut tranches = Vec::with_capacity(devices.len());
    for device in devices {
        let mut formats = device.formats.clone();
        formats.sort_unstable();
        formats.dedup();
        let mut indices = Vec::with_capacity(formats.len());
        for pair in formats {
            let index = if let Some(index) = index_of.get(&pair).copied() {
                index
            } else {
                let next = pairs.len();
                let index =
                    u16::try_from(next).map_err(|_| DmabufFeedbackError::TooManyPairs(next + 1))?;
                pairs.push(pair);
                index_of.insert(pair, index);
                index
            };
            indices.push(index);
        }
        tranches.push(DeviceTranche { drm_id: device.drm_id, dev: device.dev, indices });
    }
    let mut scanout_tranches = Vec::new();
    for offer in scanout {
        let mut formats = offer.formats.clone();
        formats.sort_unstable();
        formats.dedup();
        let indices = formats
            .iter()
            .filter_map(|pair| index_of.get(pair).copied())
            .collect::<Vec<_>>();
        if indices.is_empty() {
            continue;
        }
        scanout_tranches.push(ScanoutTranche {
            output_id: offer.output_id,
            drm_id: offer.drm_id,
            dev: offer.dev,
            indices,
        });
    }
    Ok((pairs, tranches, scanout_tranches))
}

fn encode_pairs(pairs: &[(u32, u64)]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pairs.len() * 16);
    for &(format, modifier) in pairs {
        bytes.extend_from_slice(&format.to_ne_bytes());
        bytes.extend_from_slice(&0u32.to_ne_bytes());
        bytes.extend_from_slice(&modifier.to_ne_bytes());
    }
    bytes
}

fn advertised_dev_t(fd: libc::c_int, primary: Option<libc::dev_t>) -> Option<libc::dev_t> {
    dev_t_of_fd(fd).or(primary)
}

fn dev_t_of_fd(fd: libc::c_int) -> Option<libc::dev_t> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == -1 {
        return None;
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFCHR {
        return None;
    }
    Some(stat.st_rdev)
}

fn seal_memfd(bytes: &[u8]) -> io::Result<Rc<OwnedFd>> {
    let raw = unsafe {
        libc::memfd_create(
            c"tema-dmabuf-feedback".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.write_all(bytes)?;
    let fd = OwnedFd::from(file);
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, seals) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(Rc::new(fd))
}

#[cfg(test)]
mod tests {
    use std::{
        fs::File,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
    };

    use {super::*, crate::drm::Modifier};

    fn device(index: usize, dev: libc::dev_t, formats: Vec<(u32, u64)>) -> FeedbackDevice {
        FeedbackDevice { drm_id: DrmId::from(index), dev, formats }
    }

    fn decode(fd: &OwnedFd, size: u32) -> Vec<(u32, u64)> {
        let mut bytes = vec![0u8; size as usize];
        let read =
            unsafe { libc::pread(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len(), 0) };
        assert_eq!(read as usize, bytes.len());
        bytes
            .chunks_exact(16)
            .map(|chunk| {
                let format = u32::from_ne_bytes(chunk[0..4].try_into().unwrap());
                let padding = u32::from_ne_bytes(chunk[4..8].try_into().unwrap());
                assert_eq!(padding, 0);
                let modifier = u64::from_ne_bytes(chunk[8..16].try_into().unwrap());
                (format, modifier)
            })
            .collect()
    }

    #[test]
    fn shared_pairs_are_stored_once_and_order_follows_the_first_device() {
        let first = device(1, 10, vec![(2, 20), (1, 10), (2, 20)]);
        let second = device(2, 11, vec![(1, 11), (2, 20), (1, 10)]);
        let table = build(&[first, second], &[], None).unwrap().unwrap();

        assert_eq!(table.pairs, vec![(1, 10), (2, 20), (1, 11)]);
        assert_eq!(table.devices[0].indices, vec![0, 1]);
        assert_eq!(table.devices[1].indices, vec![0, 2, 1]);
        assert_eq!(table.devices[0].drm_id, DrmId::from(1));
        assert_eq!(table.devices[1].dev, 11);
        assert_eq!(table.size as usize, table.pairs.len() * 16);
        assert_eq!(decode(&table.memfd, table.size), table.pairs);
        let seals = unsafe { libc::fcntl(table.memfd.as_raw_fd(), libc::F_GET_SEALS) };
        assert_eq!(
            seals,
            libc::F_SEAL_SEAL | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_WRITE
        );
    }

    #[test]
    fn pair_order_inside_a_device_does_not_change_the_table() {
        let left = device(1, 10, vec![(2, 1), (1, 1)]);
        let right = device(1, 10, vec![(1, 1), (2, 1)]);
        let first = build(&[left], &[], None).unwrap().unwrap();
        let second = build(&[right], &[], Some(&first)).unwrap();
        assert!(second.is_none());
    }

    #[test]
    fn a_new_dev_t_rebuilds_and_bumps_the_id() {
        let first = build(&[device(1, 10, vec![(1, 1)])], &[], None)
            .unwrap()
            .unwrap();
        assert_eq!(first.id, 1);
        assert!(build(&[device(1, 10, vec![(1, 1)])], &[], Some(&first))
            .unwrap()
            .is_none());
        let rebuilt = build(&[device(1, 11, vec![(1, 1)])], &[], Some(&first))
            .unwrap()
            .unwrap();
        assert_eq!(rebuilt.id, 2);
        assert_eq!(rebuilt.devices[0].dev, 11);
        assert_eq!(rebuilt.pairs, first.pairs);
    }

    #[test]
    fn an_invalid_modifier_is_kept() {
        let invalid = Modifier::INVALID.0;
        let table = build(&[device(1, 10, vec![(1, invalid), (1, 0)])], &[], None)
            .unwrap()
            .unwrap();
        assert_eq!(table.pairs, vec![(1, 0), (1, invalid)]);
    }

    #[test]
    fn the_pair_index_must_fit_in_a_u16() {
        let formats: Vec<(u32, u64)> = (0..65_537).map(|index| (index, 1)).collect();
        let error = build(&[device(1, 10, formats)], &[], None).unwrap_err();
        assert!(matches!(error, DmabufFeedbackError::TooManyPairs(65_537)));
    }

    #[test]
    fn scanout_pairs_reuse_sampling_indices_and_drop_unknown_ones() {
        let sampling = device(1, 10, vec![(1, 10), (2, 20)]);
        let scanout = ScanoutFormats {
            output_id: OutputId::from(4usize),
            drm_id: sampling.drm_id,
            dev: 10,
            formats: vec![(2, 20), (9, 9), (1, 10)],
        };
        let table = build(&[sampling], &[scanout], None).unwrap().unwrap();
        assert_eq!(table.pairs, vec![(1, 10), (2, 20)]);
        assert_eq!(table.scanout[0].indices, vec![0, 1]);
        assert!(build(
            &[device(1, 10, vec![(1, 10), (2, 20)])],
            &[ScanoutFormats {
                output_id: OutputId::from(4usize),
                drm_id: DrmId::from(1usize),
                dev: 10,
                formats: vec![(1, 10), (2, 20)],
            }],
            Some(&table)
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn advertised_dev_t_prefers_a_character_device() {
        let null = File::open("/dev/null").unwrap();
        let rdev = advertised_dev_t(null.as_raw_fd(), Some(9)).unwrap();
        assert_ne!(rdev, 9);

        let raw = unsafe { libc::memfd_create(c"tema-dev-t".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(raw >= 0);
        let memfd = unsafe { OwnedFd::from_raw_fd(raw) };
        assert_eq!(advertised_dev_t(memfd.as_raw_fd(), Some(9)), Some(9));
        assert_eq!(advertised_dev_t(memfd.as_raw_fd(), None), None);
    }
}
