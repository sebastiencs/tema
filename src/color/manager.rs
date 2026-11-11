use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    rc::{Rc, Weak},
};

use super::{
    description::{ImageDescription, ImageDescriptionParams, LinearPart},
    f64_bits,
    intent::RenderIntent,
    luminance::Luminances,
    primaries::Primaries,
    tf::TransferFunction,
    transform::{linear_matrix, Matrix3x4},
    NamedPrimaries,
};

pub struct ColorManager {
    shared: Rc<Shared>,
    srgb_gamma22: Rc<ImageDescription>,
    windows_scrgb: Rc<ImageDescription>,
    windows_bt2100: Rc<ImageDescription>,
}

pub(super) struct Shared {
    linear: RefCell<HashMap<LinearKey, Weak<LinearPart>>>,
    complete: RefCell<HashMap<CompleteKey, Weak<ImageDescription>>>,
    transforms: RefCell<HashMap<TransformKey, Matrix3x4>>,
    next_linear_id: Cell<u64>,
    next_complete_id: Cell<u64>,
    pub(super) dead_linear: Cell<usize>,
    pub(super) dead_complete: Cell<usize>,
}

impl Shared {
    pub(super) fn cached_matrix(
        &self,
        src: &LinearPart,
        dst: &LinearPart,
        intent: RenderIntent,
    ) -> Option<Matrix3x4> {
        let key = TransformKey { src: src.id, dst: dst.id, intent };
        if let Some(matrix) = self.transforms.borrow().get(&key).copied() {
            return Some(matrix);
        }
        let matrix = linear_matrix(src, dst, intent)?;
        self.transforms.borrow_mut().insert(key, matrix);
        Some(matrix)
    }
}

#[derive(Clone, Copy)]
struct LinearKey {
    primaries: Primaries,
    luminances: Luminances,
    target_primaries: Primaries,
    target_min: f64,
    target_max: f64,
    max_cll: Option<f64>,
    max_fall: Option<f64>,
}

impl PartialEq for LinearKey {
    fn eq(&self, other: &Self) -> bool {
        self.primaries == other.primaries
            && self.luminances == other.luminances
            && self.target_primaries == other.target_primaries
            && f64_bits(self.target_min) == f64_bits(other.target_min)
            && f64_bits(self.target_max) == f64_bits(other.target_max)
            && opt_eq(self.max_cll, other.max_cll)
            && opt_eq(self.max_fall, other.max_fall)
    }
}

impl Eq for LinearKey {}

impl Hash for LinearKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.primaries.hash(state);
        self.luminances.hash(state);
        self.target_primaries.hash(state);
        f64_bits(self.target_min).hash(state);
        f64_bits(self.target_max).hash(state);
        hash_opt(self.max_cll, state);
        hash_opt(self.max_fall, state);
    }
}

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
struct CompleteKey {
    linear_id: u64,
    named_primaries: Option<NamedPrimaries>,
    tf: TransferFunction,
}

#[derive(Clone, Copy, Eq, PartialEq, Hash)]
struct TransformKey {
    src: u64,
    dst: u64,
    intent: RenderIntent,
}

impl ColorManager {
    /// sRGB gamma 2.2 for a surface that has no image description.
    ///
    /// `SurfaceRenderState` is built without `State`, so this lives on the
    /// calling thread. Its id is 1, the same as [`Self::srgb_gamma22`], and the
    /// transform short-circuit treats those as one description.
    pub fn untagged_srgb_gamma22() -> Rc<ImageDescription> {
        thread_local! {
            static DESCRIPTION: Rc<ImageDescription> = ColorManager::new().srgb_gamma22();
        }
        DESCRIPTION.with(Rc::clone)
    }

    pub fn new() -> Self {
        let shared = Rc::new(Shared {
            linear: RefCell::new(HashMap::new()),
            complete: RefCell::new(HashMap::new()),
            transforms: RefCell::new(HashMap::new()),
            next_linear_id: Cell::new(1),
            next_complete_id: Cell::new(1),
            dead_linear: Cell::new(0),
            dead_complete: Cell::new(0),
        });
        let srgb_gamma22 = intern(&shared, ImageDescriptionParams::srgb_gamma22());
        let windows_scrgb = intern(&shared, ImageDescriptionParams::windows_scrgb());
        let windows_bt2100 = intern(&shared, ImageDescriptionParams::windows_bt2100());
        Self { shared, srgb_gamma22, windows_scrgb, windows_bt2100 }
    }

    pub fn srgb_gamma22(&self) -> Rc<ImageDescription> {
        Rc::clone(&self.srgb_gamma22)
    }

    pub fn windows_scrgb(&self) -> Rc<ImageDescription> {
        Rc::clone(&self.windows_scrgb)
    }

    pub fn windows_bt2100(&self) -> Rc<ImageDescription> {
        Rc::clone(&self.windows_bt2100)
    }

    pub fn intern(&self, params: ImageDescriptionParams) -> Rc<ImageDescription> {
        intern(&self.shared, params)
    }

    #[cfg(test)]
    pub(crate) fn complete_cache_len(&self) -> usize {
        self.shared.complete.borrow().len()
    }

    #[cfg(test)]
    pub(crate) fn linear_cache_len(&self) -> usize {
        self.shared.linear.borrow().len()
    }

    #[cfg(test)]
    pub(crate) fn transform_cache_len(&self) -> usize {
        self.shared.transforms.borrow().len()
    }
}

fn intern(shared: &Rc<Shared>, params: ImageDescriptionParams) -> Rc<ImageDescription> {
    let params = canonicalize(params);
    if gc(&shared.linear, &shared.dead_linear) {
        sweep_transforms(shared);
    }
    gc(&shared.complete, &shared.dead_complete);
    let linear = intern_linear(shared, &params);
    intern_complete(shared, &linear, params.tf, params.named_primaries)
}

/// PQ's encoding swing is 10000 cd/m², so the stored maximum is `min + 10000`
/// whatever value the caller passed.
fn canonicalize(mut params: ImageDescriptionParams) -> ImageDescriptionParams {
    if params.tf == TransferFunction::St2084Pq {
        params.luminances.max = params.luminances.min + 10_000.0;
    }
    params
}

fn intern_linear(shared: &Rc<Shared>, params: &ImageDescriptionParams) -> Rc<LinearPart> {
    let key = LinearKey {
        primaries: params.primaries,
        luminances: params.luminances,
        target_primaries: params.target_primaries,
        target_min: params.target_luminances.0,
        target_max: params.target_luminances.1,
        max_cll: params.max_cll,
        max_fall: params.max_fall,
    };
    if let Some(existing) = shared.linear.borrow().get(&key).and_then(Weak::upgrade) {
        return existing;
    }
    if shared.linear.borrow().contains_key(&key) {
        let dead = shared.dead_linear.get();
        shared.dead_linear.set(dead.saturating_sub(1));
    }
    let id = next_id(&shared.next_linear_id);
    let linear = Rc::new(LinearPart {
        id,
        primaries: params.primaries,
        luminances: params.luminances,
        target_primaries: params.target_primaries,
        target_luminances: params.target_luminances,
        max_cll: params.max_cll,
        max_fall: params.max_fall,
        xyz_from_rgb: params.primaries.rgb_to_xyz(),
        rgb_from_xyz: params.primaries.xyz_to_rgb(),
        shared: Rc::clone(shared),
    });
    shared
        .linear
        .borrow_mut()
        .insert(key, Rc::downgrade(&linear));
    linear
}

fn intern_complete(
    shared: &Rc<Shared>,
    linear: &Rc<LinearPart>,
    tf: TransferFunction,
    named_primaries: Option<NamedPrimaries>,
) -> Rc<ImageDescription> {
    let key = CompleteKey { linear_id: linear.id, named_primaries, tf };
    if let Some(existing) = shared.complete.borrow().get(&key).and_then(Weak::upgrade) {
        return existing;
    }
    if shared.complete.borrow().contains_key(&key) {
        let dead = shared.dead_complete.get();
        shared.dead_complete.set(dead.saturating_sub(1));
    }
    let description = Rc::new(ImageDescription {
        id: next_id(&shared.next_complete_id),
        linear: Rc::clone(linear),
        tf,
        named_primaries,
        shared: Rc::clone(shared),
    });
    shared
        .complete
        .borrow_mut()
        .insert(key, Rc::downgrade(&description));
    description
}

fn gc<K, V>(map: &RefCell<HashMap<K, Weak<V>>>, dead: &Cell<usize>) -> bool {
    let len = map.borrow().len();
    if len > 16 && dead.get() * 2 > len {
        map.borrow_mut().retain(|_, value| value.strong_count() > 0);
        dead.set(0);
        true
    } else {
        false
    }
}

/// Matrices are not `Weak`. Drop the ones whose linear parts were just collected.
fn sweep_transforms(shared: &Shared) {
    let live = shared
        .linear
        .borrow()
        .values()
        .filter_map(|value| value.upgrade().map(|part| part.id))
        .collect::<HashSet<_>>();
    shared
        .transforms
        .borrow_mut()
        .retain(|key, _| live.contains(&key.src) && live.contains(&key.dst));
}

fn next_id(counter: &Cell<u64>) -> u64 {
    let id = counter.get();
    counter.set(id + 1);
    id
}

fn opt_eq(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => f64_bits(left) == f64_bits(right),
        _ => false,
    }
}

fn hash_opt<H: Hasher>(value: Option<f64>, state: &mut H) {
    match value {
        None => 0u8.hash(state),
        Some(value) => {
            1u8.hash(state);
            f64_bits(value).hash(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use {
        super::ColorManager,
        crate::color::{
            description::ImageDescriptionParams, intent::RenderIntent, luminance::Luminances,
            primaries::Primaries, tf::TransferFunction, transform::ColorTransform, NamedPrimaries,
        },
    };

    #[test]
    fn prebuilt_descriptions_have_the_reference_parameters() {
        let manager = ColorManager::new();
        let srgb = manager.srgb_gamma22();
        assert_eq!(srgb.transfer_function(), TransferFunction::Gamma22);
        assert_eq!(srgb.named_primaries(), Some(NamedPrimaries::Srgb));
        assert_eq!(srgb.primaries(), Primaries::SRGB);
        assert_eq!(srgb.luminances(), Luminances::SRGB);
        assert_eq!(srgb.target_primaries(), Primaries::SRGB);
        assert_ne!(srgb.id(), 0);
        assert_ne!(srgb.linear_id(), 0);

        let scrgb = manager.windows_scrgb();
        assert_eq!(scrgb.transfer_function(), TransferFunction::ExtLinear);
        assert_eq!(scrgb.primaries(), Primaries::SRGB);
        assert_eq!(scrgb.luminances(), Luminances::WINDOWS_SCRGB);
        assert_eq!(scrgb.target_primaries(), Primaries::BT2020);
        assert_eq!(scrgb.target_luminances(), (0.0, 10_000.0));

        let bt2100 = manager.windows_bt2100();
        assert_eq!(bt2100.transfer_function(), TransferFunction::St2084Pq);
        assert_eq!(bt2100.primaries(), Primaries::BT2020);
        assert_eq!(bt2100.luminances(), Luminances::ST2084_PQ);
        assert_eq!(bt2100.target_primaries(), Primaries::BT2020);
    }

    #[test]
    fn equal_params_return_the_same_description_and_id() {
        let manager = ColorManager::new();
        let first = manager.intern(ImageDescriptionParams::srgb_gamma22());
        let second = manager.intern(ImageDescriptionParams::srgb_gamma22());
        assert!(Rc::ptr_eq(&first, &second));
        assert_eq!(first.id(), second.id());
        assert!(Rc::ptr_eq(&first, &manager.srgb_gamma22()));
        assert_ne!(manager.srgb_gamma22().id(), manager.windows_bt2100().id());
        assert_ne!(manager.srgb_gamma22().linear_id(), manager.windows_bt2100().linear_id());
    }

    #[test]
    fn named_and_custom_primaries_share_the_linear_part() {
        let manager = ColorManager::new();
        let mut custom = ImageDescriptionParams::srgb_gamma22();
        custom.named_primaries = None;
        let custom = manager.intern(custom);
        let named = manager.srgb_gamma22();
        assert_eq!(custom.linear_id(), named.linear_id());
        assert_ne!(custom.id(), named.id());
    }

    #[test]
    fn pq_maximum_is_forced_to_min_plus_10000() {
        let manager = ColorManager::new();
        let mut params = ImageDescriptionParams::windows_bt2100();
        params.luminances.min = 0.005;
        params.luminances.max = 400.0;
        let description = manager.intern(params);
        assert!((description.luminances().min - 0.005).abs() < 1e-12);
        assert!((description.luminances().max - 10_000.005).abs() < 1e-9);
    }

    #[test]
    fn dead_entries_are_collected_and_ids_are_not_reused() {
        let manager = ColorManager::new();
        let prebuilt = manager.srgb_gamma22().id();
        let mut held = Vec::new();
        let mut first_params = None;
        let mut first_id = 0;
        for index in 0..20 {
            let mut params = ImageDescriptionParams::srgb_gamma22();
            params.named_primaries = None;
            params.primaries.r.0 = 0.10 + f64::from(index) * 0.01;
            let description = manager.intern(params);
            if index == 0 {
                first_params = Some(params);
                first_id = description.id();
            }
            held.push(description);
        }
        assert_eq!(manager.complete_cache_len(), 23);
        assert_eq!(manager.linear_cache_len(), 23);
        drop(held);
        assert_eq!(manager.complete_cache_len(), 23);
        let recreated = manager.intern(first_params.unwrap());
        assert_eq!(manager.complete_cache_len(), 4);
        assert_eq!(manager.linear_cache_len(), 4);
        assert_ne!(recreated.id(), first_id);
        assert_ne!(recreated.id(), 0);
        let still_there = manager.intern(ImageDescriptionParams::srgb_gamma22());
        assert_eq!(still_there.id(), prebuilt);
        assert!(Rc::ptr_eq(&still_there, &manager.srgb_gamma22()));
    }

    #[test]
    fn one_hundred_thousand_descriptions_stay_bounded() {
        let manager = ColorManager::new();
        let output = manager.srgb_gamma22();
        for index in 0..100_000u32 {
            let mut params = ImageDescriptionParams::srgb_gamma22();
            params.named_primaries = None;
            params.luminances.white = 81.0 + f64::from(index);
            let description = manager.intern(params);
            assert!(
                ColorTransform::between(&description, &output, RenderIntent::Perceptual).is_some()
            );
        }
        assert!(manager.complete_cache_len() <= 32, "{}", manager.complete_cache_len());
        assert!(manager.linear_cache_len() <= 32, "{}", manager.linear_cache_len());
        assert!(manager.transform_cache_len() <= 32, "{}", manager.transform_cache_len());
        let fresh = manager.intern(ImageDescriptionParams::srgb_gamma22());
        assert!(Rc::ptr_eq(&fresh, &output));
    }

    #[test]
    fn matrices_are_cached_per_linear_pair_and_intent() {
        let manager = ColorManager::new();
        let srgb = manager.srgb_gamma22();
        let bt2100 = manager.windows_bt2100();
        let first = ColorTransform::between(&srgb, &bt2100, RenderIntent::Perceptual).unwrap();
        let second = ColorTransform::between(&srgb, &bt2100, RenderIntent::Perceptual).unwrap();
        assert_eq!(first.matrix, second.matrix);
        assert_eq!(manager.transform_cache_len(), 1);
        let _other = ColorTransform::between(&srgb, &bt2100, RenderIntent::Relative).unwrap();
        assert_eq!(manager.transform_cache_len(), 2);
        assert!(ColorTransform::between(&srgb, &srgb, RenderIntent::Perceptual).is_none());
        assert_eq!(manager.transform_cache_len(), 2);
    }
}
