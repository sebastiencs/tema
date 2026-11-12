use std::{fs::OpenOptions, os::fd::OwnedFd, path::PathBuf, rc::Rc};

use drm::buffer::DrmFourcc;

use crate::{
    color::{
        ColorManager, ColorTransform, ImageDescriptionParams, Luminances, NamedPrimaries,
        Primaries, RenderIntent, TransferFunction,
    },
    config::{RendererSelection, RgbaColor, RoundingPower, ShadowPower},
    drm::DrmFd,
    gbm::{GbmDevice, UseFlags},
    region::{DamageRegion, RectI},
    render::{
        create_renderer, BlurVisualStyle, BorderGradient, EffectElement, FrameElement, FramePhase,
        OutputFrameRecipe, RenderFrame, RenderSyncMode, Renderer, RoundedRing, ShadowVisualStyle,
        ShmImage, Texture, TextureElement, TextureMask, TextureSource,
    },
    state::RootSurfaceClip,
    transform::{Mat3, OutputPresentationTransform},
};

struct Pair {
    gl: Box<dyn Renderer>,
    vulkan: Box<dyn Renderer>,
    gbm: GbmDevice,
}

fn open_render_node() -> Option<OwnedFd> {
    let entries = std::fs::read_dir("/dev/dri").ok()?;
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD") || name.starts_with("card"))
        })
        .collect();
    paths.sort_by_key(|path| {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        (usize::from(!name.starts_with("renderD")), name.to_string())
    });
    paths.into_iter().find_map(|path| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .ok()
            .map(OwnedFd::from)
    })
}

fn pair() -> Option<Pair> {
    crate::render::gles::init().ok()?;
    let fd = open_render_node()?;
    let drm = DrmFd::from_owned(fd);
    let gbm = GbmDevice::new(&drm).ok()?;
    let gl = create_renderer(RendererSelection::OpenGl, &gbm).ok()?;
    let vulkan = create_renderer(RendererSelection::Vulkan, &gbm).ok()?;
    Some(Pair { gl, vulkan, gbm })
}

fn solid(color: [u8; 4], width: i32, height: i32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((width * height) as usize * 4);
    for _ in 0..width * height {
        bytes.extend_from_slice(&color);
    }
    bytes
}

fn element(
    texture: Rc<dyn Texture>,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    alpha: u8,
    mask: TextureMask,
) -> FrameElement {
    let rect = RectI::new(x, y, width, height).expect("rect");
    FrameElement::Texture {
        phase: FramePhase::Content,
        element: TextureElement {
            source: TextureSource::Texture,
            texture,
            x,
            y,
            width,
            height,
            tex_transform: Mat3::identity(),
            damage: DamageRegion::from_rect(rect),
            blend: alpha < u8::MAX || !matches!(mask, TextureMask::None),
            alpha,
            alpha_multiplier: u32::MAX,
            color: None,
            alpha_mode: 0,
            yuv: None,
            mask,
        },
    }
}

fn paint(renderer: &mut dyn Renderer, element: FrameElement, width: i32, height: i32) -> Vec<u8> {
    paint_scene(renderer, vec![element], width, height, OutputPresentationTransform::FlipY)
}

fn paint_scene(
    renderer: &mut dyn Renderer,
    elements: Vec<FrameElement>,
    width: i32,
    height: i32,
    presentation: OutputPresentationTransform,
) -> Vec<u8> {
    let recipe = OutputFrameRecipe { elements };
    let damage = DamageRegion::from_rect(RectI::new(0, 0, width, height).expect("damage"));
    let mut target = renderer
        .create_offscreen_target(width, height)
        .expect("offscreen");
    renderer
        .render(
            &mut *target,
            RenderFrame {
                recipe: &recipe,
                damage: &damage,
                preserve_contents: false,
                presentation_transform: presentation,
                sync_mode: RenderSyncMode::BlockingFinishFallback,
                blur_reference: None,
                clear_color: None,
                linear_blend: None,
            },
        )
        .expect("render");
    let stride = width as usize * 4;
    let mut bytes = vec![0_u8; stride * height as usize];
    renderer
        .readback(&*target, RectI::new(0, 0, width, height).expect("read"), stride, &mut bytes)
        .expect("readback");
    bytes
}

fn pixel<'a>(bytes: &'a [u8], width: i32, x: i32, y: i32) -> &'a [u8] {
    let start = ((y * width + x) * 4) as usize;
    &bytes[start..start + 4]
}

fn assert_close(left: &[u8], right: &[u8], tolerance: u8) {
    assert_eq!(left.len(), right.len());
    for (index, (gles, vulkan)) in left.iter().zip(right).enumerate() {
        let delta = gles.abs_diff(*vulkan);
        assert!(
            delta <= tolerance,
            "pixel byte {index}: gles {gles} vulkan {vulkan} differ by {delta}"
        );
    }
}

#[test]
fn vulkan_matches_gles_textured_quads() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan pixel comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;

    let green = [0, 255, 0, 255];
    let bytes = solid(green, 2, 2);
    let gl_texture = pair
        .gl
        .import_shm(ShmImage {
            bytes: &bytes,
            width: 2,
            height: 2,
            stride: 8,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("gles shm");
    let vk_texture = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &bytes,
            width: 2,
            height: 2,
            stride: 8,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("vulkan shm");
    assert_close(
        &paint(&mut *pair.gl, element(gl_texture, 0, 0, 2, 2, u8::MAX, TextureMask::None), 2, 2),
        &paint(
            &mut *pair.vulkan,
            element(vk_texture, 0, 0, 2, 2, u8::MAX, TextureMask::None),
            2,
            2,
        ),
        0,
    );

    let mut rows = solid(green, 2, 1);
    rows.extend(solid([255, 0, 0, 255], 2, 1));
    let gl_rows = pair
        .gl
        .import_shm(ShmImage {
            bytes: &rows,
            width: 2,
            height: 2,
            stride: 8,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("gles rows");
    let vk_rows = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &rows,
            width: 2,
            height: 2,
            stride: 8,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("vulkan rows");
    let gles_rows =
        paint(&mut *pair.gl, element(gl_rows, 0, 0, 2, 2, u8::MAX, TextureMask::None), 2, 2);
    let vulkan_rows =
        paint(&mut *pair.vulkan, element(vk_rows, 0, 0, 2, 2, u8::MAX, TextureMask::None), 2, 2);
    assert_ne!(gles_rows[..4], gles_rows[8..12], "the two rows must differ");
    assert_close(&gles_rows, &vulkan_rows, 0);

    let gl_dot = pair
        .gl
        .import_shm(ShmImage {
            bytes: &green,
            width: 1,
            height: 1,
            stride: 4,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("gles dot");
    let vk_dot = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &green,
            width: 1,
            height: 1,
            stride: 4,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("vulkan dot");
    let gles_dot =
        paint(&mut *pair.gl, element(gl_dot, 0, 0, 1, 1, u8::MAX, TextureMask::None), 2, 2);
    let vulkan_dot =
        paint(&mut *pair.vulkan, element(vk_dot, 0, 0, 1, 1, u8::MAX, TextureMask::None), 2, 2);
    assert_eq!(gles_dot[..4], green);
    assert_eq!(&gles_dot[4..8], &[0, 0, 0, 0]);
    assert_close(&gles_dot, &vulkan_dot, 0);

    let rgba = solid([255, 0, 0, 255], 2, 2);
    let gl_rgba = pair.gl.upload_rgba(&rgba, 2, 2).expect("gles rgba");
    let vk_rgba = pair.vulkan.upload_rgba(&rgba, 2, 2).expect("vulkan rgba");
    let rgba_bytes =
        paint(&mut *pair.gl, element(gl_rgba, 0, 0, 2, 2, u8::MAX, TextureMask::None), 2, 2);
    assert_eq!(&rgba_bytes[..4], &[0, 0, 255, 255]);
    assert_close(
        &rgba_bytes,
        &paint(&mut *pair.vulkan, element(vk_rgba, 0, 0, 2, 2, u8::MAX, TextureMask::None), 2, 2),
        0,
    );

    let gl_alpha = pair
        .gl
        .import_shm(ShmImage {
            bytes: &bytes,
            width: 2,
            height: 2,
            stride: 8,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("gles alpha");
    let vk_alpha = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &bytes,
            width: 2,
            height: 2,
            stride: 8,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("vulkan alpha");
    assert_close(
        &paint(&mut *pair.gl, element(gl_alpha, 0, 0, 2, 2, 128, TextureMask::None), 2, 2),
        &paint(&mut *pair.vulkan, element(vk_alpha, 0, 0, 2, 2, 128, TextureMask::None), 2, 2),
        1,
    );

    let clip =
        RootSurfaceClip::new(RectI::new(0, 0, 4, 4).expect("clip"), 8, RoundingPower::default());
    let mask = TextureMask::RoundedClip { clip };
    let rounded = solid(green, 4, 4);
    let gl_round = pair
        .gl
        .import_shm(ShmImage {
            bytes: &rounded,
            width: 4,
            height: 4,
            stride: 16,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("gles clip");
    let vk_round = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &rounded,
            width: 4,
            height: 4,
            stride: 16,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("vulkan clip");
    let gles_round =
        paint(&mut *pair.gl, element(gl_round.clone(), 0, 0, 4, 4, u8::MAX, mask.clone()), 4, 4);
    let vulkan_round =
        paint(&mut *pair.vulkan, element(vk_round.clone(), 0, 0, 4, 4, u8::MAX, mask), 4, 4);
    assert_close(pixel(&gles_round, 4, 0, 0), &[0, 97, 0, 97], 1);
    assert_close(pixel(&gles_round, 4, 1, 0), &[0, 234, 0, 234], 1);
    assert_eq!(&gles_round[8 * 4 + 8..8 * 4 + 12], &green);
    assert_close(&gles_round, &vulkan_round, 1);

    // Corner-only clip over (2, 2, 4, 4): pixels outside the window box are kept
    // (unlike mode 1), while pixels inside the box near the rounded corner fade
    // with their coverage.
    let corner_clip = RootSurfaceClip::new(
        RectI::new(2, 2, 4, 4).expect("corner clip"),
        8,
        RoundingPower::default(),
    );
    let corner_mask = TextureMask::CornerRoundedClip { clip: corner_clip };
    let gles_corner = paint(
        &mut *pair.gl,
        element(gl_round.clone(), 0, 0, 4, 4, u8::MAX, corner_mask.clone()),
        4,
        4,
    );
    let vulkan_corner =
        paint(&mut *pair.vulkan, element(vk_round.clone(), 0, 0, 4, 4, u8::MAX, corner_mask), 4, 4);
    assert_eq!(&gles_corner[..4], &green);
    assert_close(pixel(&gles_corner, 4, 2, 2), &[0, 97, 0, 97], 1);
    assert_close(&gles_corner, &vulkan_corner, 1);
}

#[test]
fn vulkan_matches_gles_frame_ring_opacity_and_upright_output() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan mask comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;

    let green = [0, 255, 0, 255];
    let red = [0, 0, 255, 255];
    let ring_bytes = solid(green, 8, 8);
    let import = |renderer: &mut dyn Renderer| {
        renderer
            .import_shm(ShmImage {
                bytes: &ring_bytes,
                width: 8,
                height: 8,
                stride: 32,
                fourcc: DrmFourcc::Argb8888,
            })
            .expect("ring texture")
    };
    let gl_ring = import(&mut *pair.gl);
    let vk_ring = import(&mut *pair.vulkan);
    let outer =
        RootSurfaceClip::new(RectI::new(0, 0, 8, 8).expect("outer"), 2, RoundingPower::default());
    let inner =
        RootSurfaceClip::new(RectI::new(2, 2, 4, 4).expect("inner"), 0, RoundingPower::default());
    let mask = TextureMask::FrameRing { ring: RoundedRing { outer, inner } };
    let gles_ring = paint(&mut *pair.gl, element(gl_ring, 0, 0, 8, 8, u8::MAX, mask.clone()), 8, 8);
    let vulkan_ring = paint(&mut *pair.vulkan, element(vk_ring, 0, 0, 8, 8, u8::MAX, mask), 8, 8);
    assert_eq!(pixel(&gles_ring, 8, 1, 4), &green, "the ring band must keep the texture");
    assert_eq!(pixel(&gles_ring, 8, 4, 4), &[0, 0, 0, 0], "the inner rect must be punched out");
    assert_close(&gles_ring, &vulkan_ring, 1);

    let background = solid(red, 2, 2);
    let foreground = solid(green, 2, 2);
    let import_pair = |renderer: &mut dyn Renderer, bytes: &[u8]| {
        renderer
            .import_shm(ShmImage {
                bytes,
                width: 2,
                height: 2,
                stride: 8,
                fourcc: DrmFourcc::Argb8888,
            })
            .expect("layered texture")
    };
    let gl_back = import_pair(&mut *pair.gl, &background);
    let gl_front = import_pair(&mut *pair.gl, &foreground);
    let vk_back = import_pair(&mut *pair.vulkan, &background);
    let vk_front = import_pair(&mut *pair.vulkan, &foreground);
    let layered = |back, front| {
        vec![
            element(back, 0, 0, 2, 2, u8::MAX, TextureMask::None),
            element(front, 0, 0, 2, 2, 128, TextureMask::None),
        ]
    };
    let gles_layers = paint_scene(
        &mut *pair.gl,
        layered(gl_back, gl_front),
        2,
        2,
        OutputPresentationTransform::FlipY,
    );
    let vulkan_layers = paint_scene(
        &mut *pair.vulkan,
        layered(vk_back, vk_front),
        2,
        2,
        OutputPresentationTransform::FlipY,
    );
    let blended = pixel(&gles_layers, 2, 0, 0);
    assert_ne!(blended, &red, "the translucent quad must change the background");
    assert_ne!(blended, &green, "the background must remain visible through the quad");
    assert_close(&gles_layers, &vulkan_layers, 1);

    let mut rows = solid(green, 2, 1);
    rows.extend(solid(red, 2, 1));
    let import_rows = |renderer: &mut dyn Renderer| {
        renderer
            .import_shm(ShmImage {
                bytes: &rows,
                width: 2,
                height: 2,
                stride: 8,
                fourcc: DrmFourcc::Argb8888,
            })
            .expect("row texture")
    };
    let gl_rows = import_rows(&mut *pair.gl);
    let vk_rows = import_rows(&mut *pair.vulkan);
    let gles_flipped = paint(
        &mut *pair.gl,
        element(gl_rows.clone(), 0, 0, 2, 2, u8::MAX, TextureMask::None),
        2,
        2,
    );
    let gl_upright = import_rows(&mut *pair.gl);
    let gles_upright = paint_scene(
        &mut *pair.gl,
        vec![element(gl_upright, 0, 0, 2, 2, u8::MAX, TextureMask::None)],
        2,
        2,
        OutputPresentationTransform::Normal,
    );
    let vk_upright = paint_scene(
        &mut *pair.vulkan,
        vec![element(vk_rows, 0, 0, 2, 2, u8::MAX, TextureMask::None)],
        2,
        2,
        OutputPresentationTransform::Normal,
    );
    assert_ne!(
        pixel(&gles_flipped, 2, 0, 0),
        pixel(&gles_upright, 2, 0, 0),
        "an upright output must not match the flipped row order"
    );
    assert_close(&gles_upright, &vk_upright, 0);
}

#[test]
fn vulkan_draws_a_texture_when_the_frame_also_has_an_effect() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan effect skip: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let green = [0, 255, 0, 255];
    let bytes = solid(green, 2, 2);
    let texture = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &bytes,
            width: 2,
            height: 2,
            stride: 8,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("shm");
    let rect = RectI::new(0, 0, 2, 2).expect("rect");
    let recipe = OutputFrameRecipe {
        elements: vec![
            element(texture, 0, 0, 2, 2, u8::MAX, TextureMask::None),
            FrameElement::Effect {
                phase: FramePhase::OverWindow,
                element: EffectElement::DimOverlay {
                    clip: RootSurfaceClip::new(rect, 0, RoundingPower::default()),
                    alpha: 200,
                },
            },
        ],
    };
    let damage = DamageRegion::from_rect(rect);
    let mut target = pair
        .vulkan
        .create_offscreen_target(2, 2)
        .expect("offscreen");
    pair.vulkan
        .render(
            &mut *target,
            RenderFrame {
                recipe: &recipe,
                damage: &damage,
                preserve_contents: false,
                presentation_transform: OutputPresentationTransform::FlipY,
                sync_mode: RenderSyncMode::BlockingFinishFallback,
                blur_reference: None,
                clear_color: None,
                linear_blend: None,
            },
        )
        .expect("render the texture under the dim overlay");
    let mut read = [0_u8; 16];
    pair.vulkan
        .readback(&*target, rect, 8, &mut read)
        .expect("readback");
    assert_eq!(read[0], 0);
    assert!(read[1] > 0 && read[1] < 250, "dim should darken green, got {}", read[1]);
    assert!(read[3] > 200, "the opaque texture should remain under the overlay");
}

fn paint_elements(
    renderer: &mut dyn Renderer,
    elements: Vec<FrameElement>,
    width: i32,
    height: i32,
) -> Vec<u8> {
    let recipe = OutputFrameRecipe { elements };
    let damage = DamageRegion::from_rect(RectI::new(0, 0, width, height).expect("damage"));
    let mut target = renderer
        .create_offscreen_target(width, height)
        .expect("offscreen");
    renderer
        .render(
            &mut *target,
            RenderFrame {
                recipe: &recipe,
                damage: &damage,
                preserve_contents: false,
                presentation_transform: OutputPresentationTransform::FlipY,
                sync_mode: RenderSyncMode::BlockingFinishFallback,
                blur_reference: None,
                clear_color: None,
                linear_blend: None,
            },
        )
        .expect("render");
    let stride = width as usize * 4;
    let mut bytes = vec![0_u8; stride * height as usize];
    renderer
        .readback(&*target, RectI::new(0, 0, width, height).expect("read"), stride, &mut bytes)
        .expect("readback");
    bytes
}

fn shm(
    renderer: &mut dyn Renderer,
    bytes: &[u8],
    width: i32,
    height: i32,
) -> std::rc::Rc<dyn Texture> {
    renderer
        .import_shm(ShmImage {
            bytes,
            width,
            height,
            stride: width as usize * 4,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("shm")
}

fn hard_ring(outer: RectI, inner: RectI) -> RoundedRing {
    RoundedRing {
        outer: RootSurfaceClip::new(outer, 0, RoundingPower::default()),
        inner: RootSurfaceClip::new(inner, 0, RoundingPower::default()),
    }
}

#[test]
fn vulkan_matches_gles_dim_border_and_shadow() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan effect comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let green = solid([0, 255, 0, 255], 4, 2);
    let gl_green = shm(&mut *pair.gl, &green, 4, 2);
    let vk_green = shm(&mut *pair.vulkan, &green, 4, 2);
    let dim_clip =
        RootSurfaceClip::new(RectI::new(0, 0, 4, 2).expect("dim"), 0, RoundingPower::default());
    let dim = |texture| {
        vec![
            element(texture, 0, 0, 4, 2, u8::MAX, TextureMask::None),
            FrameElement::Effect {
                phase: FramePhase::OverWindow,
                element: EffectElement::DimOverlay { clip: dim_clip, alpha: 128 },
            },
        ]
    };
    let gles_dim = paint_elements(&mut *pair.gl, dim(gl_green), 4, 2);
    assert!(gles_dim[1] < 250);
    assert_close(&gles_dim, &paint_elements(&mut *pair.vulkan, dim(vk_green), 4, 2), 1);

    let bounds = RectI::new(0, 0, 8, 8).expect("border");
    let inner = RectI::new(2, 2, 4, 4).expect("inner");
    let border = FrameElement::Effect {
        phase: FramePhase::OverWindow,
        element: EffectElement::WindowBorder {
            ring: hard_ring(bounds, inner),
            gradient: BorderGradient {
                start_color: RgbaColor::new(255, 255, 255, 255),
                end_color: RgbaColor::new(255, 255, 255, 255),
                angle_degrees: 0,
            },
            alpha: u8::MAX,
        },
    };
    let gles_border = paint_elements(&mut *pair.gl, vec![border.clone()], 8, 8);
    assert_eq!(&gles_border[..4], &[255, 255, 255, 255]);
    assert_eq!(&gles_border[4 * 8 * 4 + 4 * 4..4 * 8 * 4 + 4 * 4 + 4], &[0, 0, 0, 0]);
    assert_close(&gles_border, &paint_elements(&mut *pair.vulkan, vec![border], 8, 8), 1);

    let shadow = FrameElement::Effect {
        phase: FramePhase::UnderWindow,
        element: EffectElement::WindowShadow {
            clip: RootSurfaceClip::new(
                RectI::new(2, 2, 4, 4).expect("shadow"),
                0,
                RoundingPower::default(),
            ),
            style: ShadowVisualStyle {
                radius: 2,
                offset_x: 0,
                offset_y: 0,
                color: RgbaColor::new(0, 0, 0, 255),
                power: ShadowPower::default(),
            },
            alpha: u8::MAX,
        },
    };
    assert_close(
        &paint_elements(&mut *pair.gl, vec![shadow.clone()], 8, 8),
        &paint_elements(&mut *pair.vulkan, vec![shadow], 8, 8),
        2,
    );
}

#[test]
fn rounded_border_fades_its_outer_and_inner_curves() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan rounded border: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let ring = RoundedRing {
        outer: RootSurfaceClip::new(
            RectI::new(0, 0, 8, 8).expect("outer"),
            4,
            RoundingPower::default(),
        ),
        inner: RootSurfaceClip::new(
            RectI::new(2, 2, 4, 4).expect("inner"),
            2,
            RoundingPower::default(),
        ),
    };
    let border = FrameElement::Effect {
        phase: FramePhase::OverWindow,
        element: EffectElement::WindowBorder {
            ring,
            gradient: BorderGradient {
                start_color: RgbaColor::new(255, 255, 255, 255),
                end_color: RgbaColor::new(255, 255, 255, 255),
                angle_degrees: 0,
            },
            alpha: u8::MAX,
        },
    };
    let gles = paint_elements(&mut *pair.gl, vec![border.clone()], 8, 8);
    // Outside the outer curve: d = 0.95.
    assert_eq!(pixel(&gles, 8, 0, 0), &[0, 0, 0, 0]);
    // Outer curve: d = -0.46, coverage 0.96.
    assert_close(pixel(&gles, 8, 1, 1), &[246, 246, 246, 246], 1);
    // Inner curve: the inner coverage is 0.38, so the ring keeps 0.62.
    assert_close(pixel(&gles, 8, 2, 2), &[158, 158, 158, 158], 1);
    // Inside the inner rect.
    assert_eq!(pixel(&gles, 8, 4, 4), &[0, 0, 0, 0]);
    assert_close(&gles, &paint_elements(&mut *pair.vulkan, vec![border], 8, 8), 1);
}

#[test]
fn vulkan_matches_gles_blur_and_keeps_later_textures_on_top() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan blur comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let mut split = Vec::new();
    for y in 0..4 {
        for _x in 0..8 {
            if y < 2 {
                split.extend_from_slice(&[0, 255, 0, 255]);
            } else {
                split.extend_from_slice(&[255, 0, 0, 255]);
            }
        }
    }
    let blue = solid([255, 0, 0, 255], 1, 1);
    let gl_split = shm(&mut *pair.gl, &split, 8, 4);
    let vk_split = shm(&mut *pair.vulkan, &split, 8, 4);
    let gl_blue = shm(&mut *pair.gl, &blue, 1, 1);
    let vk_blue = shm(&mut *pair.vulkan, &blue, 1, 1);
    let scene = |backdrop, mark| {
        vec![
            element(backdrop, 0, 0, 8, 4, u8::MAX, TextureMask::None),
            FrameElement::Effect {
                phase: FramePhase::UnderWindow,
                element: EffectElement::WindowBlurBackdrop {
                    ring: hard_ring(
                        RectI::new(0, 0, 8, 4).expect("outer"),
                        RectI::new(3, 1, 2, 2).expect("inner"),
                    ),
                    style: BlurVisualStyle { radius: 2, passes: 1 },
                    alpha: u8::MAX,
                },
            },
            element(mark, 0, 0, 1, 1, u8::MAX, TextureMask::None),
        ]
    };
    let gles = paint_elements(&mut *pair.gl, scene(gl_split, gl_blue), 8, 4);
    let vulkan = paint_elements(&mut *pair.vulkan, scene(vk_split, vk_blue), 8, 4);
    assert_eq!(&gles[..4], &[255, 0, 0, 255]);
    assert_close(&gles, &vulkan, 3);
}

#[test]
fn vulkan_matches_gles_region_blur() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan region blur comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let width = 8;
    let height = 2;
    let mut bytes = Vec::new();
    for _y in 0..height {
        for x in 0..width {
            let color = if x % 2 == 0 { [255, 0, 0, 255] } else { [0, 0, 255, 255] };
            bytes.extend_from_slice(&color);
        }
    }
    let gl_stripes = shm(&mut *pair.gl, &bytes, width, height);
    let vk_stripes = shm(&mut *pair.vulkan, &bytes, width, height);
    let region = crate::render::BlurRegion {
        bounds: RectI::new(0, 0, 7, height).expect("bounds"),
        rects: Rc::from(vec![
            RectI::new(0, 0, 2, height).expect("left"),
            RectI::new(5, 0, 2, height).expect("right"),
        ]),
    };
    let scene = |backdrop| {
        vec![
            element(backdrop, 0, 0, width, height, u8::MAX, TextureMask::None),
            FrameElement::Effect {
                phase: FramePhase::UnderWindow,
                element: EffectElement::SurfaceBlurBackdrop {
                    region: region.clone(),
                    style: BlurVisualStyle { radius: 2, passes: 1 },
                    alpha: u8::MAX,
                },
            },
        ]
    };
    let gles = paint_elements(&mut *pair.gl, scene(gl_stripes), width, height);
    let vulkan = paint_elements(&mut *pair.vulkan, scene(vk_stripes), width, height);
    assert_close(&gles, &vulkan, 3);
    assert_eq!(pixel(&gles, width, 2, 0), &[255, 0, 0, 255]);
    assert_eq!(pixel(&vulkan, width, 2, 0), &[255, 0, 0, 255]);
    assert_ne!(pixel(&gles, width, 0, 0), &[255, 0, 0, 255]);
}

#[test]
fn vulkan_capture_transparent_matches_gles() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan capture comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let green = solid([0, 255, 0, 255], 2, 2);
    let gl_green = shm(&mut *pair.gl, &green, 2, 2);
    let vk_green = shm(&mut *pair.vulkan, &green, 2, 2);
    let gl_captured = pair
        .gl
        .capture_transparent(
            &[element(gl_green, 0, 0, 2, 2, u8::MAX, TextureMask::None)],
            2,
            2,
            (2, 2),
            OutputPresentationTransform::FlipY,
        )
        .expect("gles capture");
    let vk_captured = pair
        .vulkan
        .capture_transparent(
            &[element(vk_green, 0, 0, 2, 2, u8::MAX, TextureMask::None)],
            2,
            2,
            (2, 2),
            OutputPresentationTransform::FlipY,
        )
        .expect("vulkan capture");
    let gles =
        paint(&mut *pair.gl, element(gl_captured, 0, 0, 2, 2, u8::MAX, TextureMask::None), 2, 2);
    assert_eq!(&gles[..4], &[0, 255, 0, 255]);
    assert_close(
        &gles,
        &paint(
            &mut *pair.vulkan,
            element(vk_captured, 0, 0, 2, 2, u8::MAX, TextureMask::None),
            2,
            2,
        ),
        1,
    );
}

#[test]
fn vulkan_preserves_untouched_scanout_pixels() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan preserve comparison: no matching DRM node");
        return;
    };
    let modifiers = pair
        .vulkan
        .capabilities()
        .render_formats
        .iter()
        .find(|format| format.fourcc == DrmFourcc::Xrgb8888)
        .map(|format| format.modifiers.clone())
        .unwrap_or_default();
    let green = solid([0, 255, 0, 255], 8, 4);
    let texture = shm(&mut *pair.vulkan, &green, 8, 4);
    for index in 0..2 {
        let Some(bo) = modifiers.iter().find_map(|modifier| {
            pair.gbm.try_create_bo_with_modifiers(
                8,
                4,
                DrmFourcc::Xrgb8888,
                UseFlags::RENDERING | UseFlags::SCANOUT,
                &[crate::drm::Modifier(*modifier)],
            )
        }) else {
            eprintln!("skipping vulkan preserve comparison: GBM allocated no scanout buffer");
            return;
        };
        let dmabuf = bo.dmabuf().expect("scanout dma-buf");
        let mut target = pair
            .vulkan
            .import_output_target(dmabuf)
            .expect("import scanout");
        let recipe = OutputFrameRecipe {
            elements: vec![element(texture.clone(), 0, 0, 8, 4, u8::MAX, TextureMask::None)],
        };
        let full = DamageRegion::from_rect(RectI::new(0, 0, 8, 4).expect("full"));
        pair.vulkan
            .render(
                &mut *target,
                RenderFrame {
                    recipe: &recipe,
                    damage: &full,
                    preserve_contents: false,
                    presentation_transform: OutputPresentationTransform::FlipY,
                    sync_mode: RenderSyncMode::BlockingFinishFallback,
                    blur_reference: None,
                    clear_color: None,
                    linear_blend: None,
                },
            )
            .expect("paint the scanout buffer");
        let empty = OutputFrameRecipe { elements: Vec::new() };
        let left = DamageRegion::from_rect(RectI::new(0, 0, 4, 4).expect("left"));
        pair.vulkan
            .render(
                &mut *target,
                RenderFrame {
                    recipe: &empty,
                    damage: &left,
                    preserve_contents: true,
                    presentation_transform: OutputPresentationTransform::FlipY,
                    sync_mode: RenderSyncMode::BlockingFinishFallback,
                    blur_reference: None,
                    clear_color: None,
                    linear_blend: None,
                },
            )
            .expect("redraw the damaged half");
        let mut bytes = [0_u8; 8 * 4 * 4];
        pair.vulkan
            .readback(&*target, RectI::new(0, 0, 8, 4).expect("read"), 32, &mut bytes)
            .expect("read the scanout buffer");
        for y in 0..4 {
            for x in 0..8 {
                let start = y * 32 + x * 4;
                let pixel = &bytes[start..start + 4];
                if x < 4 {
                    assert_eq!(pixel, &[40, 40, 40, 255], "buffer {index} damaged {x},{y}");
                } else {
                    assert_eq!(pixel, &[0, 255, 0, 255], "buffer {index} preserved {x},{y}");
                }
            }
        }
    }
}

#[test]
fn vulkan_output_target_preserves_contents_across_frames() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan output preserve: no matching DRM node");
        return;
    };
    let modifiers = pair
        .vulkan
        .capabilities()
        .render_formats
        .iter()
        .find(|format| format.fourcc == DrmFourcc::Xrgb8888)
        .map(|format| format.modifiers.clone())
        .unwrap_or_default();
    let Some(bo) = modifiers.iter().find_map(|modifier| {
        pair.gbm.try_create_bo_with_modifiers(
            8,
            4,
            DrmFourcc::Xrgb8888,
            UseFlags::RENDERING | UseFlags::SCANOUT,
            &[crate::drm::Modifier(*modifier)],
        )
    }) else {
        eprintln!("skipping vulkan output preserve: GBM allocated no scanout buffer");
        return;
    };
    let dmabuf = bo.dmabuf().expect("scanout dma-buf");
    let mut target = pair
        .vulkan
        .import_output_target(dmabuf)
        .expect("import scanout");
    let empty = OutputFrameRecipe { elements: Vec::new() };
    let full = DamageRegion::from_rect(RectI::new(0, 0, 8, 4).expect("full"));
    pair.vulkan
        .render(
            &mut *target,
            RenderFrame {
                recipe: &empty,
                damage: &full,
                preserve_contents: false,
                presentation_transform: OutputPresentationTransform::FlipY,
                sync_mode: RenderSyncMode::BlockingFinishFallback,
                blur_reference: None,
                clear_color: Some([1.0, 0.0, 0.0, 1.0]),
                linear_blend: None,
            },
        )
        .expect("paint the scanout buffer red");
    let corner = DamageRegion::from_rect(RectI::new(0, 0, 2, 2).expect("corner"));
    pair.vulkan
        .render(
            &mut *target,
            RenderFrame {
                recipe: &empty,
                damage: &corner,
                preserve_contents: true,
                presentation_transform: OutputPresentationTransform::FlipY,
                sync_mode: RenderSyncMode::BlockingFinishFallback,
                blur_reference: None,
                clear_color: Some([0.0, 1.0, 0.0, 1.0]),
                linear_blend: None,
            },
        )
        .expect("paint the damaged corner green");
    let mut bytes = [0_u8; 8 * 4 * 4];
    pair.vulkan
        .readback(&*target, RectI::new(0, 0, 8, 4).expect("read"), 32, &mut bytes)
        .expect("read the scanout buffer");
    for y in 0..4 {
        for x in 0..8 {
            let start = y * 32 + x * 4;
            let pixel = &bytes[start..start + 4];
            // Readback is B8G8R8A8. The clear colors are RGBA.
            if x < 2 && y < 2 {
                assert_eq!(pixel, &[0, 255, 0, 255], "damaged {x},{y}");
            } else {
                assert_eq!(pixel, &[0, 0, 255, 255], "preserved {x},{y}");
            }
        }
    }
}

#[test]
fn vulkan_client_dmabuf_sees_writes_between_frames() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan client dma-buf: no matching DRM node");
        return;
    };
    let caps = pair.vulkan.capabilities();
    let render = caps
        .render_formats
        .iter()
        .find(|format| format.fourcc == DrmFourcc::Xrgb8888);
    let sample = caps
        .sample_formats
        .iter()
        .find(|format| format.fourcc == DrmFourcc::Xrgb8888);
    let modifiers: Vec<u64> = match (render, sample) {
        (Some(render), Some(sample)) => render
            .modifiers
            .iter()
            .copied()
            .filter(|modifier| sample.modifiers.contains(modifier))
            .collect(),
        _ => Vec::new(),
    };
    let Some(bo) = modifiers.iter().find_map(|modifier| {
        pair.gbm.try_create_bo_with_modifiers(
            8,
            4,
            DrmFourcc::Xrgb8888,
            UseFlags::RENDERING | UseFlags::SCANOUT,
            &[crate::drm::Modifier(*modifier)],
        )
    }) else {
        eprintln!("skipping vulkan client dma-buf: GBM allocated no shared buffer");
        return;
    };
    let Some(dmabuf) = bo.dmabuf() else {
        eprintln!("skipping vulkan client dma-buf: buffer exported no dma-buf");
        return;
    };
    let texture = match pair.vulkan.import_dmabuf(dmabuf) {
        Ok(texture) => texture,
        Err(error) => {
            eprintln!("skipping vulkan client dma-buf: import failed: {error}");
            return;
        }
    };
    let mut source = pair
        .vulkan
        .import_output_target(dmabuf)
        .expect("import the same buffer as a render target");
    clear_output(&mut *pair.vulkan, &mut *source, 8, 4, [1.0, 0.0, 0.0, 1.0]);
    let red = paint(
        &mut *pair.vulkan,
        element(Rc::clone(&texture), 0, 0, 8, 4, u8::MAX, TextureMask::None),
        8,
        4,
    );
    for y in 0..4 {
        for x in 0..8 {
            assert_eq!(pixel(&red, 8, x, y), &[0, 0, 255, 255], "red sample {x},{y}");
        }
    }
    clear_output(&mut *pair.vulkan, &mut *source, 8, 4, [0.0, 1.0, 0.0, 1.0]);
    let green =
        paint(&mut *pair.vulkan, element(texture, 0, 0, 8, 4, u8::MAX, TextureMask::None), 8, 4);
    for y in 0..4 {
        for x in 0..8 {
            assert_eq!(pixel(&green, 8, x, y), &[0, 255, 0, 255], "green sample {x},{y}");
        }
    }
}

fn clear_output(
    renderer: &mut dyn Renderer,
    target: &mut dyn crate::render::RenderTarget,
    width: i32,
    height: i32,
    color: [f32; 4],
) {
    let recipe = OutputFrameRecipe { elements: Vec::new() };
    let damage = DamageRegion::from_rect(RectI::new(0, 0, width, height).expect("full"));
    renderer
        .render(
            target,
            RenderFrame {
                recipe: &recipe,
                damage: &damage,
                preserve_contents: false,
                presentation_transform: OutputPresentationTransform::FlipY,
                sync_mode: RenderSyncMode::BlockingFinishFallback,
                blur_reference: None,
                clear_color: Some(color),
                linear_blend: None,
            },
        )
        .expect("paint the client buffer");
}

fn linear_cursor_bo(gbm: &GbmDevice, size: u32) -> Option<crate::gbm::BufferObject> {
    let attempts = [Some(UseFlags::SCANOUT | UseFlags::RENDERING), Some(UseFlags::empty()), None];
    attempts
        .into_iter()
        .find_map(|flags| gbm.try_create_linear_cursor_bo(size, size, flags))
}

fn render_cursor_image(
    renderer: &mut dyn Renderer,
    target: &mut dyn crate::render::RenderTarget,
    texture: Rc<dyn Texture>,
    image_width: i32,
    image_height: i32,
    target_size: i32,
) {
    let image = RectI::new(0, 0, image_width, image_height).expect("image");
    let bounds = RectI::new(0, 0, target_size, target_size).expect("target");
    let recipe = OutputFrameRecipe {
        elements: vec![FrameElement::Texture {
            phase: FramePhase::Content,
            element: TextureElement {
                source: TextureSource::Texture,
                texture,
                x: 0,
                y: 0,
                width: image_width,
                height: image_height,
                tex_transform: Mat3::identity(),
                damage: DamageRegion::from_rect(image),
                blend: true,
                alpha: u8::MAX,
                alpha_multiplier: u32::MAX,
                color: None,
                alpha_mode: 0,
                yuv: None,
                mask: TextureMask::None,
            },
        }],
    };
    let damage = DamageRegion::from_rect(bounds);
    renderer
        .render(
            target,
            RenderFrame {
                recipe: &recipe,
                damage: &damage,
                preserve_contents: false,
                presentation_transform: OutputPresentationTransform::FlipY,
                sync_mode: RenderSyncMode::BlockingFinishFallback,
                blur_reference: None,
                clear_color: Some([0.0, 0.0, 0.0, 0.0]),
                linear_blend: None,
            },
        )
        .expect("cursor render");
}

fn cursor_pixel(bytes: &[u8], x: i32, y: i32) -> [u8; 4] {
    let start = y as usize * 16 + x as usize * 4;
    bytes[start..start + 4].try_into().expect("pixel")
}

#[test]
fn vulkan_cursor_target_keeps_top_left_and_clears_the_rest() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping Vulkan cursor target: no matching DRM node");
        return;
    };
    let Some(bo) = linear_cursor_bo(&pair.gbm, 4) else {
        eprintln!("skipping Vulkan cursor target: no linear ARGB8888 scanout buffer");
        return;
    };
    let modifier = bo.modifier();
    if modifier != crate::drm::Modifier::LINEAR && modifier != crate::drm::Modifier::INVALID {
        eprintln!("skipping Vulkan cursor target: modifier {modifier:?} is not linear");
        return;
    }
    let Some(dmabuf) = bo.dmabuf() else {
        eprintln!("skipping Vulkan cursor target: buffer exported no dma-buf");
        return;
    };
    let mut target = match pair.vulkan.import_output_target(dmabuf) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("skipping Vulkan cursor target: import failed: {error}");
            return;
        }
    };
    let rgba = [
        255, 0, 0, 255, 0, 255, 0, 255, //
        0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let named = pair
        .vulkan
        .upload_rgba(&rgba, 2, 2)
        .expect("named cursor upload");
    render_cursor_image(&mut *pair.vulkan, &mut *target, named, 2, 2, 4);
    let mut bytes = vec![0u8; 4 * 4 * 4];
    let read = RectI::new(0, 0, 4, 4).unwrap();
    pair.vulkan
        .readback(&*target, read, 16, &mut bytes)
        .expect("cursor readback");
    assert_eq!(cursor_pixel(&bytes, 0, 0), [0, 0, 255, 255], "top-left stays red");
    assert_eq!(cursor_pixel(&bytes, 0, 1), [255, 0, 0, 255], "second row stays blue");
    assert_eq!(cursor_pixel(&bytes, 3, 3), [0, 0, 0, 0], "outside the cursor is transparent");

    let shm = [
        1, 2, 3, 255, 0, 0, 0, 0, //
        0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let imported = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &shm,
            width: 2,
            height: 2,
            stride: 8,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("shm cursor import");
    render_cursor_image(&mut *pair.vulkan, &mut *target, imported, 2, 2, 4);
    pair.vulkan
        .readback(&*target, read, 16, &mut bytes)
        .expect("shm readback");
    assert_eq!(cursor_pixel(&bytes, 0, 0), [1, 2, 3, 255]);
    assert_eq!(cursor_pixel(&bytes, 0, 1), [0, 0, 0, 0]);

    let small = pair
        .vulkan
        .upload_rgba(&[255, 255, 255, 255], 1, 1)
        .expect("small cursor");
    render_cursor_image(&mut *pair.vulkan, &mut *target, small, 1, 1, 4);
    pair.vulkan
        .readback(&*target, read, 16, &mut bytes)
        .expect("small cursor readback");
    assert_eq!(cursor_pixel(&bytes, 0, 0), [255, 255, 255, 255]);
    assert_eq!(cursor_pixel(&bytes, 1, 0), [0, 0, 0, 0], "previous cursor row was cleared");
}

#[test]
fn vulkan_xrgb_shm_stays_opaque_when_blended() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping vulkan xrgb alpha: no matching DRM node");
        return;
    };
    // Little-endian B, G, R, A. Red with a zero alpha byte.
    let xrgb = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &[0, 0, 255, 0],
            width: 1,
            height: 1,
            stride: 4,
            fourcc: DrmFourcc::Xrgb8888,
        })
        .expect("xrgb");
    assert_eq!(
        paint(&mut *pair.vulkan, blended_pixel(xrgb), 1, 1),
        [0, 0, 255, 255],
        "xrgb alpha byte 0 must sample as opaque"
    );

    let argb = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &[0, 0, 255, 0],
            width: 1,
            height: 1,
            stride: 4,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("argb");
    assert_eq!(
        paint(&mut *pair.vulkan, blended_pixel(argb), 1, 1),
        [0, 0, 255, 0],
        "argb keeps the stored alpha"
    );
}

#[test]
fn alpha_multiplier_matches_on_argb_and_xrgb() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping alpha multiplier comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let factor = (0.51 * f64::from(u32::MAX)) as u32;
    let white = [255, 255, 255, 255];
    // Little-endian B, G, R, A. Opaque red, and the same red stored as XRGB.
    let cases = [([0, 0, 255, 255], DrmFourcc::Argb8888), ([0, 0, 255, 0], DrmFourcc::Xrgb8888)];
    for (pixel, fourcc) in cases {
        let gl_back = pair.gl.upload_rgba(&white, 1, 1).expect("gles backdrop");
        let vk_back = pair
            .vulkan
            .upload_rgba(&white, 1, 1)
            .expect("vulkan backdrop");
        let gl_tex = pair
            .gl
            .import_shm(ShmImage { bytes: &pixel, width: 1, height: 1, stride: 4, fourcc })
            .expect("gles pixel");
        let vk_tex = pair
            .vulkan
            .import_shm(ShmImage { bytes: &pixel, width: 1, height: 1, stride: 4, fourcc })
            .expect("vulkan pixel");
        let gles = paint_scene(
            &mut *pair.gl,
            vec![
                element(gl_back, 0, 0, 1, 1, u8::MAX, TextureMask::None),
                multiplied(gl_tex, factor),
            ],
            1,
            1,
            OutputPresentationTransform::FlipY,
        );
        let vulkan = paint_scene(
            &mut *pair.vulkan,
            vec![
                element(vk_back, 0, 0, 1, 1, u8::MAX, TextureMask::None),
                multiplied(vk_tex, factor),
            ],
            1,
            1,
            OutputPresentationTransform::FlipY,
        );
        assert_close(&gles, &vulkan, 1);
        // Premultiplied red over white stays red and lets the white through in the other channels.
        assert!(gles[2] > 250, "blended red stays bright, got {gles:?}");
        assert!(gles[0] > 80, "white shows through the multiplier, got {gles:?}");
    }
}

#[test]
fn ten_bit_gradient_matches_on_gles_and_vulkan() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping 10-bit gradient comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let width = 4;
    let samples = [(0, 1023, 512), (341, 682, 512), (682, 341, 512), (1023, 0, 512)];
    let cases = [
        (DrmFourcc::Xrgb2101010, true, 0u32),
        (DrmFourcc::Argb2101010, true, 0),
        (DrmFourcc::Xbgr2101010, false, 0),
        (DrmFourcc::Abgr2101010, false, 3),
    ];
    for (fourcc, argb_order, alpha) in cases {
        let mut bytes = Vec::with_capacity(width * 4);
        for (red, green, blue) in samples {
            bytes.extend(pack_2101010(red, green, blue, alpha, argb_order));
        }
        let gl_tex = pair
            .gl
            .import_shm(ShmImage {
                bytes: &bytes,
                width: width as i32,
                height: 1,
                stride: width * 4,
                fourcc,
            })
            .expect("gles 10-bit");
        let vk_tex = pair
            .vulkan
            .import_shm(ShmImage {
                bytes: &bytes,
                width: width as i32,
                height: 1,
                stride: width * 4,
                fourcc,
            })
            .expect("vulkan 10-bit");
        let gles = paint(
            &mut *pair.gl,
            element(gl_tex, 0, 0, width as i32, 1, u8::MAX, TextureMask::None),
            width as i32,
            1,
        );
        let vulkan = paint(
            &mut *pair.vulkan,
            element(vk_tex, 0, 0, width as i32, 1, u8::MAX, TextureMask::None),
            width as i32,
            1,
        );
        assert_close(&gles, &vulkan, 1);
        let first = pixel(&gles, width as i32, 0, 0);
        let last = pixel(&gles, width as i32, width as i32 - 1, 0);
        assert!(first[2] <= 1, "{fourcc:?} left edge stays dark red, got {first:?}");
        assert!(last[2] >= 254, "{fourcc:?} right edge stays bright red, got {last:?}");
        assert!(
            first[0] > 100 && first[0] < 160,
            "{fourcc:?} blue channel stays mid, got {first:?}"
        );
        assert!(last[1] <= 1, "{fourcc:?} right edge has no green, got {last:?}");
        let expect_alpha = if alpha == 0 && !fourcc_has_alpha(fourcc) {
            255
        } else if alpha == 0 {
            0
        } else {
            255
        };
        assert_eq!(first[3], expect_alpha, "{fourcc:?} alpha, got {first:?}");
    }
}

fn fourcc_has_alpha(fourcc: DrmFourcc) -> bool {
    matches!(fourcc, DrmFourcc::Argb2101010 | DrmFourcc::Abgr2101010)
}

/// Little-endian `2:10:10:10`. `argb_order` stores blue in the low 10 bits.
fn pack_2101010(red: u32, green: u32, blue: u32, alpha: u32, argb_order: bool) -> [u8; 4] {
    let (low, high) = if argb_order { (blue, red) } else { (red, blue) };
    let word =
        (low & 0x3ff) | ((green & 0x3ff) << 10) | ((high & 0x3ff) << 20) | ((alpha & 0x3) << 30);
    word.to_le_bytes()
}

#[test]
fn fp16_windows_scrgb_matches_the_cpu_transform() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping fp16 scrgb comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let manager = ColorManager::new();
    let transform = ColorTransform::between(
        &manager.windows_scrgb(),
        &manager.srgb_gamma22(),
        RenderIntent::Perceptual,
    )
    .expect("scrgb to srgb");
    assert!(transform.tonemap.is_some(), "scRGB's 10000 nit target tone maps onto SDR");
    let clipped = ColorTransform::between(
        &manager.windows_scrgb(),
        &manager.srgb_gamma22(),
        RenderIntent::RelativeBpc,
    )
    .expect("scrgb clip");
    let signal_one = expected_linear(&transform, [1.0, 1.0, 1.0]);
    let signal_two = expected_linear(&transform, [2.0, 2.0, 2.0]);
    let reference_white = 203.0_f64 / 80.0;
    let white = expected_linear(&transform, [reference_white, reference_white, reference_white]);
    let clipped_white =
        expected_linear(&clipped, [reference_white, reference_white, reference_white]);
    assert!(signal_one[0] < 200, "scRGB 1.0 stays below sRGB white, got {signal_one:?}");
    assert!(
        signal_two[0] > signal_one[0] + 10,
        "scRGB 2.0 stays above 1.0 after the shoulder, got {signal_two:?} vs {signal_one:?}"
    );
    assert!(
        clipped_white[..3].iter().all(|channel| *channel >= 254),
        "relative intent still encodes scRGB 203 nits as sRGB white, got {clipped_white:?}"
    );
    assert!(
        white[0] + 2 < clipped_white[0],
        "perceptual reference white sits below the clip, got {white:?} vs {clipped_white:?}"
    );

    for fourcc in [DrmFourcc::Abgr16161616f, DrmFourcc::Xbgr16161616f] {
        let alpha = if fourcc == DrmFourcc::Abgr16161616f { 1.0 } else { 0.0 };
        let (gles, vulkan) = paint_fp16(&mut pair, fourcc, 1.0, alpha, transform);
        assert_pixel_close("gles scrgb 1.0", &gles, &signal_one);
        assert_pixel_close("vulkan scrgb 1.0", &vulkan, &signal_one);
        assert_eq!(gles[3], 255, "{fourcc:?} alpha, got {gles:?}");
        assert_eq!(vulkan[3], 255, "{fourcc:?} alpha, got {vulkan:?}");
    }
    let (gles, vulkan) = paint_fp16(&mut pair, DrmFourcc::Abgr16161616f, 2.0, 1.0, transform);
    assert_pixel_close("gles scrgb 2.0", &gles, &signal_two);
    assert_pixel_close("vulkan scrgb 2.0", &vulkan, &signal_two);
    let (gles, vulkan) =
        paint_fp16(&mut pair, DrmFourcc::Abgr16161616f, reference_white as f32, 1.0, transform);
    assert_pixel_close("gles scrgb reference white", &gles, &white);
    assert_pixel_close("vulkan scrgb reference white", &vulkan, &white);
}

fn paint_fp16(
    pair: &mut Pair,
    fourcc: DrmFourcc,
    value: f32,
    alpha: f32,
    color: ColorTransform,
) -> ([u8; 4], [u8; 4]) {
    let bytes = fp16_pixel(value, value, value, alpha);
    let image = ShmImage { bytes: &bytes, width: 1, height: 1, stride: 8, fourcc };
    let gl_tex = pair.gl.import_shm(image).expect("gles fp16");
    let vk_tex = pair
        .vulkan
        .import_shm(ShmImage { bytes: &bytes, width: 1, height: 1, stride: 8, fourcc })
        .expect("vulkan fp16");
    (paint_colored(&mut *pair.gl, gl_tex, color), paint_colored(&mut *pair.vulkan, vk_tex, color))
}

fn paint_colored(
    renderer: &mut dyn Renderer,
    texture: Rc<dyn Texture>,
    color: ColorTransform,
) -> [u8; 4] {
    let FrameElement::Texture { phase, mut element } =
        element(texture, 0, 0, 1, 1, u8::MAX, TextureMask::None)
    else {
        unreachable!("texture element");
    };
    element.color = Some(Rc::new(color));
    let bytes = paint(renderer, FrameElement::Texture { phase, element }, 1, 1);
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

fn expected_linear(transform: &ColorTransform, rgb: [f64; 3]) -> [u8; 4] {
    let mapped = transform.map_rgb(rgb);
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [channel(mapped[2]), channel(mapped[1]), channel(mapped[0]), 255]
}

fn fp16_pixel(red: f32, green: f32, blue: f32, alpha: f32) -> [u8; 8] {
    let mut bytes = [0; 8];
    for (index, value) in [red, green, blue, alpha].into_iter().enumerate() {
        bytes[index * 2..index * 2 + 2].copy_from_slice(&f16_bits(value).to_le_bytes());
    }
    bytes
}

/// Round a positive finite value to binary16.
fn f16_bits(value: f32) -> u16 {
    if value == 0.0 {
        return 0;
    }
    assert!(value.is_finite() && value > 0.0, "fp16 test value {value}");
    let bits = value.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = bits & 0x7fffff;
    assert!((1..31).contains(&exponent), "fp16 exponent for {value}");
    let mut result = ((exponent as u16) << 10) | ((mantissa >> 13) as u16);
    let remainder = mantissa & 0x1fff;
    if remainder > 0x1000 || (remainder == 0x1000 && (mantissa & 0x2000) != 0) {
        result += 1;
    }
    result
}

#[test]
fn half_float_one_and_two_use_the_binary16_encodings() {
    assert_eq!(f16_bits(1.0), 0x3c00);
    assert_eq!(f16_bits(2.0), 0x4000);
}

fn multiplied(texture: Rc<dyn Texture>, alpha_multiplier: u32) -> FrameElement {
    let FrameElement::Texture { mut element, .. } =
        element(texture, 0, 0, 1, 1, u8::MAX, TextureMask::None)
    else {
        unreachable!("texture element");
    };
    element.blend = false;
    element.alpha_multiplier = alpha_multiplier;
    FrameElement::Texture { phase: FramePhase::Content, element }
}

fn blended_pixel(texture: Rc<dyn Texture>) -> FrameElement {
    let rect = RectI::new(0, 0, 1, 1).expect("rect");
    FrameElement::Texture {
        phase: FramePhase::Content,
        element: TextureElement {
            source: TextureSource::Texture,
            texture,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            tex_transform: Mat3::identity(),
            damage: DamageRegion::from_rect(rect),
            blend: true,
            alpha: u8::MAX,
            alpha_multiplier: u32::MAX,
            color: None,
            alpha_mode: 0,
            yuv: None,
            mask: TextureMask::None,
        },
    }
}

#[test]
fn color_transforms_match_on_gles_and_vulkan() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping color transform comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let manager = ColorManager::new();
    let srgb = manager.srgb_gamma22();
    let display_p3 = manager.intern(ImageDescriptionParams {
        tf: TransferFunction::Gamma22,
        primaries: Primaries::DISPLAY_P3,
        named_primaries: Some(NamedPrimaries::DisplayP3),
        luminances: Luminances::SRGB,
        target_primaries: Primaries::DISPLAY_P3,
        target_luminances: (Luminances::SRGB.min, Luminances::SRGB.max),
        max_cll: None,
        max_fall: None,
    });
    let to_p3 =
        ColorTransform::between(&srgb, &display_p3, RenderIntent::Perceptual).expect("srgb to p3");
    let p3_pixel = [220, 40, 90, 255];
    assert_ne!(
        expected_bgra(&to_p3, p3_pixel)[0..3],
        [p3_pixel[2], p3_pixel[1], p3_pixel[0]],
        "sRGB red-heavy pixel must move in Display P3"
    );
    assert_renderers_match(&mut pair, p3_pixel, to_p3);

    let mut pq_params = ImageDescriptionParams::srgb_gamma22();
    pq_params.tf = TransferFunction::St2084Pq;
    pq_params.luminances = Luminances::ST2084_PQ;
    let pq = manager.intern(pq_params);
    let to_srgb =
        ColorTransform::between(&pq, &srgb, RenderIntent::Perceptual).expect("pq to srgb");
    let white = TransferFunction::St2084Pq.encode(203.0 / 10_000.0);
    let white_byte = (white.clamp(0.0, 1.0) * 255.0).round() as u8;
    let pq_white = [white_byte, white_byte, white_byte, 255];
    let expected_white = expected_bgra(&to_srgb, pq_white);
    assert!(
        expected_white[..3].iter().all(|channel| *channel >= 254),
        "PQ 203 nits must stay white, got {expected_white:?}"
    );
    assert_renderers_match(&mut pair, pq_white, to_srgb);

    let to_srgb_from_linear =
        ColorTransform::between(&manager.windows_scrgb(), &srgb, RenderIntent::Perceptual)
            .expect("scrgb to srgb");
    assert_renderers_match(&mut pair, [255, 255, 255, 255], to_srgb_from_linear);
}

fn assert_renderers_match(pair: &mut Pair, rgba: [u8; 4], color: ColorTransform) {
    let gles = painted_color(&mut *pair.gl, rgba, color);
    let vulkan = painted_color(&mut *pair.vulkan, rgba, color);
    let expected = expected_bgra(&color, rgba);
    assert_pixel_close("gles", &gles, &expected);
    assert_pixel_close("vulkan", &vulkan, &expected);
    assert_pixel_close("gles/vulkan", &gles, &vulkan);
}

fn painted_color(renderer: &mut dyn Renderer, rgba: [u8; 4], color: ColorTransform) -> [u8; 4] {
    let texture = renderer.upload_rgba(&rgba, 1, 1).expect("rgba");
    let FrameElement::Texture { phase, mut element } =
        element(texture, 0, 0, 1, 1, u8::MAX, TextureMask::None)
    else {
        unreachable!("texture element");
    };
    element.color = Some(Rc::new(color));
    let bytes = paint(renderer, FrameElement::Texture { phase, element }, 1, 1);
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

#[test]
fn nv12_and_p010_match_the_bt709_limited_matrix() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping yuv comparison: no matching DRM node");
        return;
    };
    paint_flat_yuv(&mut pair, DrmFourcc::Nv12, false, 126, 64, 192, None);
    paint_flat_yuv(&mut pair, DrmFourcc::P010, true, 470, 256, 768, None);
    let bt601 = crate::color::ColorRepresentation {
        coefficients: Some(crate::color::Coefficients {
            kind: crate::color::yuv::CoefficientKind::Bt601,
            range: crate::color::yuv::VideoRange::Limited,
        }),
        ..crate::color::ColorRepresentation::default()
    }
    .yuv_conversion(false);
    paint_flat_yuv(&mut pair, DrmFourcc::Nv12, false, 126, 64, 192, Some(bt601));
    paint_nv12_checker(&mut pair);
}

fn import_yuv_textures(
    pair: &mut Pair,
    mut dmabuf: crate::dmabuf::DmaBuf,
    fourcc: DrmFourcc,
) -> Option<(Option<Rc<dyn Texture>>, Option<Rc<dyn Texture>>)> {
    dmabuf.modifier = crate::drm::Modifier::LINEAR;
    let gl_texture = match pair.gl.import_dmabuf(&dmabuf) {
        Ok(texture) => Some(texture),
        Err(error) => {
            yuv_note(&format!("{fourcc:?} gles: {error}"));
            None
        }
    };
    let vk_texture = match pair.vulkan.import_dmabuf(&dmabuf) {
        Ok(texture) => Some(texture),
        Err(error) => {
            yuv_note(&format!("{fourcc:?} vulkan: {error}"));
            None
        }
    };
    if gl_texture.is_none() && vk_texture.is_none() {
        None
    } else {
        Some((gl_texture, vk_texture))
    }
}

fn yuv_note(message: &str) {
    eprintln!("{message}");
}

#[test]
fn straight_and_optical_alpha_match_the_cpu() {
    let Some(mut pair) = pair() else {
        eprintln!("skipping alpha-mode comparison: no matching DRM node");
        return;
    };
    let _ = pair.gbm;
    let premul = [0, 0, 128, 128];
    let straight = [0, 0, 255, 128];
    let premul_out = paint_bgra(&mut pair, premul, 0);
    let straight_out = paint_bgra(&mut pair, straight, 2);
    for (name, bytes) in [("premul", &premul_out), ("straight", &straight_out)] {
        for (renderer, pixel) in [("gles", &bytes.0), ("vulkan", &bytes.1)] {
            assert_bgra_close(&format!("{name} {renderer}"), pixel, &premul);
        }
    }

    let optical = [0, 0, 200, 128];
    let encoded = 200.0_f32 / 255.0;
    let alpha = 128.0_f32 / 255.0;
    let linear = encoded.powf(2.2) / alpha;
    let red = (linear.powf(1.0 / 2.2) * alpha * 255.0).round() as u8;
    assert!(
        (red as i32 - 200).abs() > 2,
        "optical premultiplication must leave gamma 2.2, got {red}"
    );
    let (gles, vulkan) = paint_bgra(&mut pair, optical, 1);
    let expected = [0, 0, red, 128];
    assert_bgra_close("optical gles", &gles, &expected);
    assert_bgra_close("optical vulkan", &vulkan, &expected);
}

#[test]
fn hdr_translucency_blends_in_linear_then_encodes_pq() {
    let pq = TransferFunction::St2084Pq;
    let alpha = 128.0 / 255.0;
    let quant = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let sample = |byte: u8| f64::from(byte) / 255.0;
    let bottom = quant(pq.encode(0.25));
    let top_red = quant(pq.encode(0.9) * alpha);
    let top_green = quant(pq.encode(0.05) * alpha);
    let decode_premul = |byte: u8| pq.decode(sample(byte) / alpha);
    let blend_linear = |top: u8| {
        quant(pq.encode(decode_premul(top) * alpha + pq.decode(sample(bottom)) * (1.0 - alpha)))
    };
    let blend_encoded = |top: u8| quant(sample(top) + sample(bottom) * (1.0 - alpha));
    let linear = [blend_linear(top_green), blend_linear(top_green), blend_linear(top_red), 255];
    let encoded = [blend_encoded(top_green), blend_encoded(top_green), blend_encoded(top_red), 255];
    assert!(
        linear[..3].iter().zip(encoded[..3].iter()).any(|(left, right)| left.abs_diff(*right) > 1),
        "PQ blending in linear must leave the encoded-space blend, linear {linear:?} encoded {encoded:?}"
    );

    let Some(mut pair) = pair() else {
        eprintln!("skipping linear blend comparison: no matching DRM node");
        return;
    };
    let bottom_px = [bottom, bottom, bottom, 255];
    let top_px = [top_green, top_green, top_red, 128];
    let paint = |renderer: &mut dyn Renderer, linear_blend| {
        paint_layers(renderer, &[(bottom_px, false), (top_px, true)], linear_blend)
    };
    let gles = paint(&mut *pair.gl, Some(pq));
    let vulkan = paint(&mut *pair.vulkan, Some(pq));
    assert_pixel_close("linear blend gles", &gles, &linear);
    assert_pixel_close("linear blend vulkan", &vulkan, &linear);

    let gles_encoded = paint(&mut *pair.gl, None);
    let vulkan_encoded = paint(&mut *pair.vulkan, None);
    assert_pixel_close("encoded blend gles", &gles_encoded, &encoded);
    assert_pixel_close("encoded blend vulkan", &vulkan_encoded, &encoded);

    let opaque = |renderer: &mut dyn Renderer, linear_blend| {
        paint_layers(renderer, &[(bottom_px, false)], linear_blend)
    };
    let gles_direct = opaque(&mut *pair.gl, None);
    let gles_elided = opaque(&mut *pair.gl, Some(pq));
    let vulkan_direct = opaque(&mut *pair.vulkan, None);
    let vulkan_elided = opaque(&mut *pair.vulkan, Some(pq));
    assert_pixel_close("opaque elision gles", &gles_elided, &gles_direct);
    assert_pixel_close("opaque elision vulkan", &vulkan_elided, &vulkan_direct);
    assert_eq!(gles_direct, bottom_px, "an opaque texel is copied");
}

fn paint_layers(
    renderer: &mut dyn Renderer,
    layers: &[([u8; 4], bool)],
    linear_blend: Option<TransferFunction>,
) -> [u8; 4] {
    let mut elements = Vec::with_capacity(layers.len());
    for (bytes, translucent) in layers {
        let texture = renderer
            .import_shm(ShmImage {
                bytes,
                width: 1,
                height: 1,
                stride: 4,
                fourcc: DrmFourcc::Argb8888,
            })
            .expect("shm");
        let mut painted = element(texture, 0, 0, 1, 1, u8::MAX, TextureMask::None);
        if let FrameElement::Texture { element, .. } = &mut painted {
            element.blend = *translucent;
        }
        elements.push(painted);
    }
    let recipe = OutputFrameRecipe { elements };
    let damage = DamageRegion::from_rect(RectI::new(0, 0, 1, 1).expect("damage"));
    let mut target = renderer.create_offscreen_target(1, 1).expect("offscreen");
    renderer
        .render(
            &mut *target,
            RenderFrame {
                recipe: &recipe,
                damage: &damage,
                preserve_contents: false,
                presentation_transform: OutputPresentationTransform::FlipY,
                sync_mode: RenderSyncMode::BlockingFinishFallback,
                blur_reference: None,
                clear_color: None,
                linear_blend,
            },
        )
        .expect("render");
    let mut bytes = [0_u8; 4];
    renderer
        .readback(&*target, RectI::new(0, 0, 1, 1).expect("read"), 4, &mut bytes)
        .expect("readback");
    bytes
}

fn paint_bgra(pair: &mut Pair, bgra: [u8; 4], alpha_mode: u32) -> ([u8; 4], [u8; 4]) {
    let with_mode = |texture| {
        let mut painted = element(texture, 0, 0, 1, 1, u8::MAX, TextureMask::None);
        if let FrameElement::Texture { element, .. } = &mut painted {
            element.alpha_mode = alpha_mode;
        }
        painted
    };
    let gl_texture = pair
        .gl
        .import_shm(ShmImage {
            bytes: &bgra,
            width: 1,
            height: 1,
            stride: 4,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("gles shm");
    let vk_texture = pair
        .vulkan
        .import_shm(ShmImage {
            bytes: &bgra,
            width: 1,
            height: 1,
            stride: 4,
            fourcc: DrmFourcc::Argb8888,
        })
        .expect("vulkan shm");
    let gles = paint(&mut *pair.gl, with_mode(gl_texture), 1, 1);
    let vulkan = paint(&mut *pair.vulkan, with_mode(vk_texture), 1, 1);
    ([gles[0], gles[1], gles[2], gles[3]], [vulkan[0], vulkan[1], vulkan[2], vulkan[3]])
}

fn paint_flat_yuv(
    pair: &mut Pair,
    fourcc: DrmFourcc,
    ten_bit: bool,
    y: u16,
    cb: u16,
    cr: u16,
    conversion: Option<crate::color::YuvConversion>,
) {
    let Some(dmabuf) = dumb_yuv(&pair.gbm, fourcc, 4, 4, ten_bit, |plane, x, _row| {
        if plane == 0 {
            y
        } else if x % 2 == 0 {
            cb
        } else {
            cr
        }
    }) else {
        yuv_note(&format!("skip {fourcc:?}: no dumb buffer"));
        return;
    };
    let Some((gl_texture, vk_texture)) = import_yuv_textures(pair, dmabuf, fourcc) else {
        yuv_note(&format!("skip {fourcc:?}: neither renderer imported it"));
        return;
    };
    let paint_one = |texture| {
        let mut painted = element(texture, 0, 0, 4, 4, u8::MAX, TextureMask::None);
        if let FrameElement::Texture { element, .. } = &mut painted {
            element.yuv = conversion;
        }
        painted
    };
    let gles = gl_texture.map(|texture| paint(&mut *pair.gl, paint_one(texture), 4, 4));
    let vulkan = vk_texture.map(|texture| paint(&mut *pair.vulkan, paint_one(texture), 4, 4));
    let rows = conversion
        .unwrap_or_else(|| crate::color::ColorRepresentation::default().yuv_conversion(ten_bit))
        .rows;
    let sample = if ten_bit {
        let norm = |code: u16| ((u32::from(code) << 6) as f32) / 65535.0;
        crate::color::yuv::apply_rows(rows, norm(y), norm(cb), norm(cr))
    } else {
        crate::color::yuv::apply_rows(rows, y as f32 / 255.0, cb as f32 / 255.0, cr as f32 / 255.0)
    };
    let expected = bgra_of(sample);
    for y_px in 0..4 {
        for x in 0..4 {
            if let Some(gles) = &gles {
                assert_bgra_close(
                    &format!("{fourcc:?} gles {x},{y_px}"),
                    pixel(gles, 4, x, y_px),
                    &expected,
                );
            }
            if let Some(vulkan) = &vulkan {
                assert_bgra_close(
                    &format!("{fourcc:?} vulkan {x},{y_px}"),
                    pixel(vulkan, 4, x, y_px),
                    &expected,
                );
            }
        }
    }
}

fn paint_nv12_checker(pair: &mut Pair) {
    let chroma = |x: usize, chroma_row: usize| match (x / 2, chroma_row) {
        (0, 0) => (64u16, 128u16),
        (1, 0) => (192, 128),
        (0, 1) => (128, 64),
        _ => (128, 192),
    };
    let Some(dmabuf) = dumb_yuv(&pair.gbm, DrmFourcc::Nv12, 4, 4, false, |plane, x, row| {
        if plane == 0 {
            126
        } else {
            let (cb, cr) = chroma(x, row);
            if x % 2 == 0 {
                cb
            } else {
                cr
            }
        }
    }) else {
        yuv_note("skip checker: no dumb buffer");
        return;
    };
    let Some((gl_texture, vk_texture)) = import_yuv_textures(pair, dmabuf, DrmFourcc::Nv12) else {
        yuv_note("skip checker: neither renderer imported it");
        return;
    };
    let gles = gl_texture.map(|texture| {
        paint(&mut *pair.gl, element(texture, 0, 0, 4, 4, u8::MAX, TextureMask::None), 4, 4)
    });
    let vulkan = vk_texture.map(|texture| {
        paint(&mut *pair.vulkan, element(texture, 0, 0, 4, 4, u8::MAX, TextureMask::None), 4, 4)
    });
    let expect = |x: i32, y: i32| {
        let (cb, cr) = chroma(x as usize, (y / 2) as usize);
        bgra_of(crate::color::yuv::bt709_limited_rgb(
            126.0 / 255.0,
            cb as f32 / 255.0,
            cr as f32 / 255.0,
            false,
        ))
    };
    for (x, y) in [(0, 0), (2, 0), (0, 3), (2, 3)] {
        if let Some(gles) = &gles {
            assert_bgra_close(
                &format!("nv12 checker gles {x},{y}"),
                pixel(gles, 4, x, y),
                &expect(x, y),
            );
        }
        if let Some(vulkan) = &vulkan {
            assert_bgra_close(
                &format!("nv12 checker vulkan {x},{y}"),
                pixel(vulkan, 4, x, y),
                &expect(x, y),
            );
        }
    }
}

fn dumb_create(
    gbm: &GbmDevice,
    width: u32,
    height: u32,
    bpp: u32,
) -> Option<(std::os::fd::OwnedFd, drm_ffi::drm_sys::drm_mode_create_dumb)> {
    use std::os::fd::{AsFd, OwnedFd};
    let mut devices = Vec::new();
    if let Ok(fd) = gbm.as_fd().try_clone_to_owned() {
        devices.push(fd);
    }
    if let Ok(entries) = std::fs::read_dir("/dev/dri") {
        let mut paths: Vec<_> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("card"))
            })
            .collect();
        paths.sort();
        for path in paths {
            if let Ok(file) = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
            {
                devices.push(OwnedFd::from(file));
            }
        }
    }
    for fd in devices {
        match drm_ffi::mode::dumbbuffer::create(fd.as_fd(), width, height, bpp, 0) {
            Ok(created) => return Some((fd, created)),
            Err(_) => {}
        }
    }
    None
}

/// GBM on this machine will not allocate NV12. A linear dumb buffer is one
/// dma-buf with the chroma plane placed after the luma rows.
fn dumb_yuv(
    gbm: &GbmDevice,
    fourcc: DrmFourcc,
    width: u32,
    height: u32,
    ten_bit: bool,
    sample: impl Fn(usize, usize, usize) -> u16,
) -> Option<crate::dmabuf::DmaBuf> {
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
    let bpp = if ten_bit { 16 } else { 8 };
    let (device, created) = match dumb_create(gbm, width, height + height / 2, bpp) {
        Some(created) => created,
        None => {
            yuv_note(&format!("dumb create {fourcc:?}: no device accepted it"));
            return None;
        }
    };
    let pitch = created.pitch as usize;
    let sample_bpp = bpp as usize / 8;
    let luma_bytes = pitch * height as usize;
    let total = pitch * (height as usize + height as usize / 2);
    if created.size < total as u64 || pitch < width as usize * sample_bpp {
        let _ = drm_ffi::mode::dumbbuffer::destroy(device.as_fd(), created.handle);
        yuv_note(&format!("dumb {fourcc:?} pitch {pitch} size {} is short", created.size));
        return None;
    }
    let mut bytes = vec![0u8; total];
    for row in 0..height as usize {
        for col in 0..width as usize {
            store(&mut bytes, row * pitch + col * sample_bpp, sample(0, col, row), ten_bit);
        }
    }
    for row in 0..height as usize / 2 {
        for col in 0..width as usize {
            store(
                &mut bytes,
                luma_bytes + row * pitch + col * sample_bpp,
                sample(1, col, row),
                ten_bit,
            );
        }
    }
    let mapped = match drm_ffi::mode::dumbbuffer::map(device.as_fd(), created.handle, 0, 0) {
        Ok(mapped) => mapped,
        Err(error) => {
            let _ = drm_ffi::mode::dumbbuffer::destroy(device.as_fd(), created.handle);
            yuv_note(&format!("dumb map {fourcc:?}: {error}"));
            return None;
        }
    };
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            created.size as libc::size_t,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            device.as_raw_fd(),
            mapped.offset as libc::off_t,
        )
    };
    if ptr == libc::MAP_FAILED {
        let _ = drm_ffi::mode::dumbbuffer::destroy(device.as_fd(), created.handle);
        yuv_note(&format!("dumb mmap {fourcc:?} failed"));
        return None;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast(), bytes.len());
        libc::munmap(ptr, created.size as libc::size_t);
    }
    let prime =
        match drm_ffi::gem::handle_to_fd(device.as_fd(), created.handle, libc::O_CLOEXEC as u32) {
            Ok(prime) => prime,
            Err(error) => {
                let _ = drm_ffi::mode::dumbbuffer::destroy(device.as_fd(), created.handle);
                yuv_note(&format!("dumb prime {fourcc:?}: {error}"));
                return None;
            }
        };
    let _ = drm_ffi::mode::dumbbuffer::destroy(device.as_fd(), created.handle);
    let fd = unsafe { OwnedFd::from_raw_fd(prime.fd) };
    let Ok(dup) = fd.try_clone() else {
        return None;
    };
    let format = crate::drm::client_format(fourcc)?;
    Some(crate::dmabuf::DmaBuf {
        id: crate::dmabuf::DmaBufId::next(),
        width: width as i32,
        height: height as i32,
        format,
        modifier: crate::drm::Modifier::LINEAR,
        planes: vec![
            crate::dmabuf::DmaBufPlane { offset: 0, stride: created.pitch, fd },
            crate::dmabuf::DmaBufPlane {
                offset: luma_bytes as u32,
                stride: created.pitch,
                fd: dup,
            },
        ]
        .into_boxed_slice(),
        sampling_device: None,
    })
}

fn store(bytes: &mut [u8], at: usize, code: u16, ten_bit: bool) {
    if ten_bit {
        bytes[at..at + 2].copy_from_slice(&(code << 6).to_le_bytes());
    } else {
        bytes[at] = code as u8;
    }
}

fn bgra_of(rgb: [f32; 3]) -> [u8; 4] {
    [
        crate::color::yuv::encode_byte(rgb[2]),
        crate::color::yuv::encode_byte(rgb[1]),
        crate::color::yuv::encode_byte(rgb[0]),
        255,
    ]
}

fn assert_bgra_close(label: &str, got: &[u8], want: &[u8; 4]) {
    for (index, (got, want)) in got.iter().zip(want).enumerate() {
        let delta = got.abs_diff(*want);
        assert!(delta <= 2, "{label} channel {index}: {got} vs {want}");
    }
}

fn assert_pixel_close(label: &str, got: &[u8], want: &[u8]) {
    for (index, (got, want)) in got.iter().zip(want).enumerate() {
        let delta = got.abs_diff(*want);
        assert!(delta <= 1, "{label} channel {index}: {got} vs {want}");
    }
}

fn expected_bgra(transform: &ColorTransform, rgba: [u8; 4]) -> [u8; 4] {
    let sample = |byte: u8| f64::from(byte) / 255.0;
    let mapped = transform.map_rgb([sample(rgba[0]), sample(rgba[1]), sample(rgba[2])]);
    let channel = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    [channel(mapped[2]), channel(mapped[1]), channel(mapped[0]), rgba[3]]
}
