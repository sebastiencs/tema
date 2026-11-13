# Tema architecture notes

This document is a practical map of the repository and of the Linux graphics stack it sits on top of.

It is written for "future me after a long break", not for a graphics specialist. The goal is to make it easy to answer:

- What kind of project is this?
- Which layers exist between a Wayland client and the monitor?
- What does each acronym do?
- How do the modules in this repository map to those layers?

This is not a full graphics textbook. It focuses on the pieces that matter for understanding this codebase.

## 1. What This Repository Is

This repository is a small Wayland compositor built fairly close to the metal.

At a high level, it does four things:

1. Accepts Wayland clients and speaks the Wayland protocol.
2. Tracks compositor state such as objects, globals, devices, and buffers.
3. Talks to Linux graphics and input subsystems.
4. Sets up display output through DRM/KMS and GBM, then draws with OpenGL ES or Vulkan.

Another useful way to say it:

- The top half is "Wayland server and compositor logic".
- The bottom half is "Linux graphics stack and hardware-facing pieces".

## 2. The Big Layer Cake

From top to bottom, the stack looks like this:

```text
Applications
  GTK / Qt / SDL / custom Wayland clients

Wayland protocol
  wl_display, wl_registry, wl_surface, xdg_surface, wl_shm, etc.

This compositor
  Clients, Wayland objects, windows, surface state, roles, buffer handling

Rendering layer
  OpenGL ES or Vulkan

Buffer allocation / sharing layer
  GBM + dma-buf

Kernel graphics / display control
  DRM / KMS + GEM

Hardware
  GPU, display engine, connectors, monitor
```

That same stack can also be read as a pipeline:

1. A client creates a buffer.
2. The compositor receives that buffer through Wayland.
3. The compositor imports or uploads the buffer into GPU-usable form.
4. The compositor renders the final scene into an output buffer.
5. DRM/KMS tells the display engine to scan out that buffer to a connector.
6. The monitor shows the image.

## 3. How This Repository Maps to the Layers

These files are the most important anchors:

- `src/main.rs`: startup and wiring.
- `src/process/`: reaper, forker, and the `ProcessManager` model. See section 16.
- `src/runner.rs`: the event/action engine.
- `src/state.rs`: shared compositor state.
- `src/clients.rs`: Wayland server socket, client connections, protocol dispatch.
- `src/wayland_objects/*`: concrete Wayland object implementations.
- `src/input.rs`: libinput integration.
- `src/udev.rs`: device discovery and hotplug.
- `src/dbus.rs`: session and device access through logind.
- `src/drm.rs`: display resources, modesetting, planes, framebuffers.
- `src/gbm/*`: GBM buffer allocation.
- `src/render/`: renderer selection and the shared frame recipe.
- `src/render/gles/`: OpenGL ES backend, including its EGL display.
- `src/render/vulkan/`: Vulkan backend.
- `src/dmabuf.rs`: shared dma-buf description.

### Repository mental model

The center of the architecture is:

- `State`: the world.
- `Runner`: the scheduler.
- `Model`s: active subsystems.
- `Object`s: Wayland protocol objects.

The code is structured as a small action-driven runtime:

- `io_uring` produces completions.
- completions are turned into actions.
- actions are dispatched to models.
- models mutate `State` and schedule more work.

So this is not just "a compositor that happens to use async I/O". It is closer to "a compositor runtime whose subsystems communicate through typed actions".

## 4. The Main Runtime Flow

At startup:

1. The original process becomes the reaper and forks the compositor. The compositor then starts the forker. Both forks happen before EGL, while the process is still one thread. Section 16 describes the tree.
2. `main.rs` initializes EGL, XKB, globals, and shared `State`.
3. It creates models for:
   - `IoUringState`
   - `Dbus`
   - `Udev`
   - `Libinput`
   - `Clients`
   - `ProcessManager`
4. `ClientManagerAction::Start` starts the Wayland socket accept loop and also starts D-Bus and udev. `ProcessManager` starts next and sends the default environment to the forker.

At runtime:

- `Runner` continuously drains queued actions.
- When there are no queued actions, it waits for `io_uring` completions.
- Those completions become new actions and wake the appropriate model.

That means the Wayland server, input, hotplug, D-Bus, and DRM event reading all flow through the same core dispatch mechanism.

## 5. Wayland Layer: What Happens Above Graphics

The Wayland side of the repository is the protocol-facing half.

Important concepts:

- A Wayland client connects over a Unix socket.
- It sends protocol messages addressed to object IDs.
- The compositor looks up the object and calls its handler.
- Objects represent protocol instances like `wl_display`, `wl_registry`, `wl_surface`, `wl_buffer`, and `xdg_surface`.

In this repository:

- `src/clients.rs` manages client sockets, reads messages, parses them, and dispatches requests.
- `src/wayland_objects/*` implements the behavior of each protocol object.

Examples:

- `wl_display.get_registry` creates a registry object and advertises globals.
- `wl_compositor.create_surface` creates a `wl_surface`.
- `xdg_wm_base.get_xdg_surface` gives a surface an xdg role.
- `wl_shm.create_pool` and `wl_shm_pool.create_buffer` create CPU-shared pixel buffers.
- `wl_surface.attach` + `commit` moves a pending buffer onto the visible surface state.

The Wayland layer does not directly put pixels on the monitor. It defines:

- who owns which surface
- which buffer is attached
- which role a surface has
- when a state change becomes visible

A managed toplevel is also a window. Layout, workspaces, grabs, snapshots, keybindings, and capture use that `WindowId`. Section 18 describes the split between windows and surfaces.

The lower graphics layers take over after that.

## 5.1 Input methods

Text entry is three protocols, all advertised to every client:

- `zwp_text_input_v3` is the application's side. Tema advertises it at version 1. A client enables it on the focused surface and sends the surrounding text, the content type, and the cursor rectangle. `done.serial` counts that object's commits.
- `zwp_input_method_v2` is the input method's side. One live input method per seat. A second `get_input_method` is created inert and sent `unavailable` immediately.
- `zwp_virtual_keyboard_v1` injects keys. `wtype`, fcitx5, and on-screen keyboards use it for keysyms the hardware keymap does not have. Those keys do not enter the hardware key path, so they cannot loop back into the input-method grab.

The relay lives in `src/input/ime.rs`. An enable on the focused surface sends `activate`, the current state, and `done` to the input method. Later commits send only the fields that changed. The input method's `commit` must echo the last `done` serial; a mismatch drops the pending strings. A match is forwarded to the active text input as delete, preedit, commit string, then `done`. A focus change clears the preedit, deactivates the input method, leaves the old client's text inputs, and enters every text input of the new client. Requests the client sends after `leave` are still applied, so its following `disable` is a real edge.

Each client has its own keymap. The hardware keymap is id 0. A virtual keyboard compiles the keymap string it sends and switches the focused client to that blob: if keys are held, the client gets `leave`, the new keymap, `enter` with those keys, then modifiers. The next hardware key or focus change switches back to the hardware keymap. Replacing or destroying a virtual keyboard releases the keys it still has down.

VT switching (section 5.2) runs first. `zwp_input_method_keyboard_grab_v2` is consulted after compositor key bindings and before the focused client. A press goes to the grab and is recorded. Its release stays with the grab, and is swallowed if the grab has already gone. Presses that happened before the grab, or while the session is locked, still reach the client. The lock screen bypasses the grab, and input-method popups are not drawn on it.

`zwp_input_popup_surface_v2` is the candidate window. It is placed below the cursor rectangle on the output that contains the caret, flipped above when it would leave that output, then clamped inside it. `text_input_rectangle` tells the input method where the caret sits in popup-local coordinates. The popup is not a pointer target and never takes keyboard focus.

An on-screen keyboard is a layer-shell surface, usually with `keyboard_interactivity = none`, plus a virtual keyboard. A click on that layer leaves keyboard focus where it is, so the tap does not deactivate the input method. `on_demand` and `exclusive` layers still take focus. Other focus requests, including an explicit `set_keyboard_focus` of a `none` layer, still go through the normal focus normalization.

## 5.2 VT switching

Ctrl+Alt+F1 through Ctrl+Alt+F12 are built in. The `pc` keymap includes `srvr_ctrl`, so that chord produces `XF86Switch_VT_1` through `XF86Switch_VT_12`. A press of one of those keysyms calls logind `Seat.SwitchTo(n)` on the seat object path. The matching release is swallowed, so the client never sees that F-key. Ctrl and Alt still reach the client.

The check runs before compositor bindings, the input-method grab, and the lock gate. A locked session still switches and comes back locked. A binding on that chord never runs. While the session is paused, the key stays on the normal path and `SwitchTo` is not sent.

`SwitchTo` is not sent when tema is nested under X11, because session control was not taken, or after session control has been released on shutdown. An error reply logs `could not switch to VT n: …` and leaves the session running. Pause, quiesce, and resume are the existing session pause path: `PauseDevice`, then `Active=false`, then the resume signals on the way back.

## 6. Linux Graphics Stack: The Lower Half

This is the part that usually feels like a wall of acronyms:

- dma-buf
- GBM
- EGL
- OpenGL ES / GL
- DRM / KMS
- GEM
- connector / CRTC / plane

The easiest way to understand them is to group them by job.

### 6.1 Memory sharing: dma-buf

`dma-buf` is the standard Linux mechanism for sharing image buffers across subsystems and processes without copying the pixels.

It is not a renderer and not a display API.

It is basically:

- a file descriptor
- plus metadata
- describing an image stored in memory

That metadata includes:

- width
- height
- pixel format
- per-plane stride
- per-plane offset
- modifier

In this repository, that description lives in `src/dmabuf.rs`.

Clients learn which formats this compositor can sample through `zwp_linux_dmabuf_v1`, advertised at version 6. Binding version 4 or newer does not receive the old flat format list. The client creates a `zwp_linux_dmabuf_feedback_v1` instead. That object sends a sealed table of format and modifier pairs (`src/dmabuf_feedback.rs`), then one tranche per GPU. Version 6 marks each tranche for sampling and does not send a main device. Feedback for a surface lists that surface's GPU first. A client that still binds version 3 receives the first GPU's formats on the global, as before.

Practical meaning:

- a client, decoder, compositor, or GPU component can all refer to the same underlying image
- if they all understand the format and modifier

### 6.2 Buffer allocation: GBM

`GBM` is the Generic Buffer Manager.

Its job is to allocate buffer objects that are suitable for graphics use, especially:

- GPU rendering
- display scanout
- exporting as dma-buf

Think of GBM as: "please give me a buffer the driver and hardware will actually like".

In this repository:

- `src/gbm/gbm.rs` creates a GBM device from the DRM device
- GBM allocates buffer objects using usage flags and modifiers
- those GBM buffer objects are exported as `DmaBuf`

GBM sits between high-level rendering code and low-level driver memory management.

### 6.3 Rendering API glue: EGL

`EGL` is the bridge between native platform graphics objects and rendering APIs like OpenGL ES.

It does several jobs:

- create a rendering display from GBM
- choose a config
- create a GL context
- make that context current
- import dma-bufs as `EGLImage`

EGL is not the display engine and not the renderer itself.

It is the integration layer that makes "GL on Linux native buffers" work.

In this repository, `src/render/gles/` is where:

- the EGL display is created from GBM
- supported dma-buf formats/modifiers are queried
- dma-bufs are imported into EGL images
- those images are turned into renderbuffers, framebuffers, or textures

Vulkan does not use EGL. Its device, images, and sync files live in `src/render/vulkan/`.

### 6.4 Rendering API: OpenGL ES / GL

`OpenGL ES` is one of the two rendering APIs. Vulkan is the other, and section 6.5 is how a device chooses.

For this repository, GL is mostly about:

- taking imported surface data
- treating it as textures or image-backed render targets
- drawing into output buffers

Useful terms here:

- render: generate pixels into a destination
- texture: image resource sampled by shaders
- blend: combine source pixels with destination pixels
- viewport: map normalized output coordinates into a target rectangle
- scissor: clip writes to a rectangular area

OpenGL ES operates above buffer allocation and below compositor scene logic.

### 6.5 Choosing the renderer

Each DRM device builds one renderer at startup from `general.renderer`.

- An omitted key, or `opengl`, uses OpenGL ES.
- `auto` tries OpenGL ES on that device, then Vulkan.
- `vulkan` uses Vulkan. If that device cannot match the DRM node or lacks a required extension, startup fails for that device. The compositor does not switch it to OpenGL.

A running compositor keeps its renderer. Changing the setting needs a restart.

When the renderer can export a sync file and the primary plane has `IN_FENCE_FD`, KMS waits on that fence. Otherwise the renderer waits for the GPU to finish before `render` returns. The DRM log for that second path is `Falling back to blocking GL sync`.

### 6.6 Kernel display control: DRM/KMS

`DRM` is the Linux kernel graphics subsystem.

`KMS` means Kernel Mode Setting, the display-control side of DRM.

This is the part that answers:

- which monitor is connected?
- which mode is active?
- which image is being shown?
- which plane/CRTC/connector relationships are active?

In this repository, `src/drm.rs`:

- enables atomic modesetting
- enumerates connectors, CRTCs, encoders, and planes
- chooses a usable connector and primary plane
- creates DRM framebuffers from dma-bufs
- commits display state through atomic KMS

This is the layer closest to "actually putting pixels on screen".

### 6.7 Kernel graphics memory objects: GEM

`GEM` stands for Graphics Execution Manager.

In practice, a GEM object is a kernel/driver-managed graphics memory object.

A helpful distinction:

- dma-buf fd: shareable cross-subsystem handle
- GEM handle/object: DRM-driver-side graphics memory object
- framebuffer: KMS object describing how to interpret memory for scanout

Your GBM buffers are backed by driver-managed memory. When KMS needs to scan them out, that memory is represented in DRM terms through GEM-backed handles.

## 7. Display Hardware Objects: Connector, CRTC, Plane

These are the most important KMS concepts.

### Connector

A connector is an output endpoint, such as:

- HDMI-A-1
- DP-1
- eDP-1

It represents the place where a monitor is attached.

The connector knows things like:

- connected or disconnected state
- supported modes
- EDID-derived monitor information

### CRTC

A CRTC is the scanout and timing engine.

Historically the name came from old display hardware, but in modern Linux graphics it roughly means:

- the hardware pipeline that reads image data
- applies display timing
- feeds the output path

You can think of the CRTC as the part that is actively driving the frame to the display.

### Plane

A plane is a hardware image layer.

Typical kinds are:

- primary plane
- cursor plane
- overlay plane

The primary plane usually carries the main desktop image.
Cursor and overlay planes can be used for hardware composition.

### Relationship

One useful simplified model is:

- connector: where the image goes
- CRTC: the engine that drives the image
- plane: which image layer is scanned

And underneath the plane sits a framebuffer object pointing at image memory.

## 8. Scanout, Page Flip, and Direct Scanout

### Scanout

To scan out a buffer means:

- the display engine reads pixels from memory
- and streams them to the monitor

So rendering writes pixels into memory, while scanout reads pixels from memory for display.

### Page flip

A page flip is when the display system switches from one framebuffer to another for scanout.

Typical double-buffering idea:

- one buffer is currently visible
- another buffer is rendered offscreen
- then the display switches to the new buffer at a safe point

That switch is the page flip.

In modern KMS this often appears as part of an atomic commit rather than an older standalone flip API, but the concept is the same.

### Direct scanout

Normally the compositor imports client buffers and composites them into a new output buffer.

Direct scanout is the optimization where the compositor skips that extra composition step and lets the display hardware scan out a client buffer directly.

This is only possible when many constraints are satisfied, for example:

- format is supported
- modifier is supported
- geometry is suitable
- no overlapping composition is needed

### Tearing

A vsync page flip switches framebuffers at the vertical blank. An async page flip switches as soon as the kernel accepts the commit, so the monitor can show part of the old frame and part of the new one. That is tearing. A client asks for it with `wp_tearing_control_v1` by setting the presentation hint to async. Mesa does this for Vulkan immediate presentation.

An output tears only when every one of these holds:

- `general.allow_tearing` is true, or `[output.tearing]` sets this connector to true. The key is the connector name, the same string as `wl_output.name`, for example `"DP-1"`. A missing entry uses the global switch. The global default is true. The name is case-sensitive.
- The active workspace shows one fullscreen window, and that window's committed hint is async. Popups that belong to that window still count. Another window, a layer-shell surface, an effect, a lock surface, a closing snapshot, a pending capture, or protected content does not.
- No session lock is gating input, and no workspace transition is running on the output.
- The driver reports `DRM_CAP_ATOMIC_ASYNC_PAGE_FLIP`. There is no legacy page-flip path.
- This frame keeps the mode, the primary-plane geometry, gamma, the color transform, and the variable-refresh enable bit as they were on the previous successful commit.
- The hardware cursor keeps the same slot, position, and visibility, and this frame does not upload a new cursor image. A software cursor drawn into the scene is allowed.

The async atomic commit programs the primary plane's framebuffer, plus the in-fence and damage clips when that plane accepts them. Connector, CRTC, cursor, color, variable-refresh, and geometry properties stay out of the request. amdgpu rejects an async commit that changes them. Any error is logged the first time and then every 32 rejections, and the same frame is submitted again as a normal vsync flip. A direct-scanout attempt that fails this way retries direct scanout synchronously. The next frame tries async again.

`wp_presentation` drops the vsync flag only for the commit that landed as async. Hardware completion stays. Zero-copy stays on a direct scanout.

While tearing, the frame clock presents as soon as the previous flip finishes. It skips the vblank deadline and the variable-refresh minimum interval. Variable refresh itself stays as the output already had it, so the async commit does not toggle `VRR_ENABLED`. One flip stays in flight. A commit that arrives during that flip is rendered when the flip completes. Frame callbacks are still sent when the frame is submitted.

The log line `output DP-1: tearing on` or `output DP-1: tearing off (cursor)` is printed only when that state changes. The reason in parentheses is one of: config, session lock, workspace transition, hint, composited content, cursor, geometry, variable refresh, color, modeset, capability, kernel rejection.

### Checking tearing on amdgpu

1. `drm_info | grep -i async` shows `ATOMIC_ASYNC_PAGE_FLIP = 1`.
2. `wayland-info` lists `wp_tearing_control_manager_v1`.
3. Fullscreen `vkcube --present_mode 0` tears and runs above the refresh rate.
4. Moving the mouse drops single frames to vsync. The log says `tearing off (cursor)` once, then `tearing on` once the cursor settles. It does not print a line per frame.
5. A windowed `vkcube` never tears. The log stays on `tearing off (hint)`.
6. Set `allow_tearing = false` and save the config. Tearing stops without a restart. `"DP-1" = false` under `[output.tearing]` does the same for that connector.
7. A layer-shell notification over the fullscreen window stops tearing while it is visible. The log says `tearing off (composited content)`, then `tearing on` when the notification goes away.

### Logical layout and the physical frame

Layout, input, and protocol positions are logical pixels. `State::output_bounds` is that layout rect. `Output::scale` is a v120 fraction, snapped so the mode divides evenly on both axes. The Xwayland wire scale is a separate integer; section 25 is that path.

`collect_output_render_plan` (`src/drm/render_planner.rs`) is the boundary:

1. Collect items in global logical pixels.
2. Translate by the output origin. The scene is now output-local and still logical.
3. `scale_scene_items` and `scale_recorded_frame_elements` (`src/output_scale.rs`) turn it into physical framebuffer pixels. Geometry rounds each edge half toward +∞. Damage rounds outward.
4. `build_frame_plan`, damage, direct scanout, and both renderers stay in those physical pixels. The projection maps the render buffer, which is the mode size. Shaders stay item-local.

`DrmOutput::output_bounds` is that physical buffer, at the origin. It is not a layout rect. A missing layout rect uses the output's logical size, including when the output is disabled.

A closing snapshot and a toplevel capture are window-local physical pixels at the output's scale. Live blur stays logical until the output plan scales it. Output capture and any debug view of the CRTC use this same plan. There is no separate overlay renderer. Output rotation is not applied. Scale happens first so a later transform can sit on the physical rect.

### Client blur

A client asks for blur with `ext_background_effect_v1`. The committed region is a `BlurRegion`: the bounds plus the rects that show the blur, capped at 256. `State::root_surface_blur_region` returns it for a layer surface, an xdg toplevel, or an xdg popup, and only while `effects.blur.enabled` is set. Any other role may still create the object. The compositor accepts it and draws nothing.

The capability bit is `Blur` exactly when that flag is on. A config reload resends `capabilities` to every bound manager. Turning the flag off stops the effect; the query returns nothing, so the next frame does not draw it.

The effect is recorded as `SurfaceBlurBackdrop` immediately before that surface's texture. The radius and the pass count come from `effects.blur`. The client only supplies the region. A window that also has the config-driven decoration blur gets both: the ring behind the frame, and this region on the surface. Fractional scale goes through `scale_effect_element`. A region change, a move, or a blur config change damages the region bounds padded by `radius * passes * 4`. A frame that contains the effect is not a direct-scanout candidate.

## 9. Format and Modifier

These two are easy to confuse.

### Format

The format tells you what the pixels mean.

Examples:

- `XRGB8888`
- `ARGB8888`

It answers:

- how many channels exist?
- in what order?
- how many bits per channel?
- is there alpha?

### Modifier

The modifier tells you how those pixels are laid out in memory.

It may describe:

- linear layout
- tiled layout
- compressed tiled layout
- vendor-specific swizzles or compression

So:

- format = what the pixels are
- modifier = how the pixels are physically stored

This matters because two components can only share a buffer safely if they agree on both.

That is why the repository intersects modifier support between the rendering side and the KMS plane side.

## 10. Linear, Tiled, and Compressed Tiled Memory

### Linear memory

Pixels are stored row by row in straightforward order.

This is the easiest layout to understand, but not always the fastest for hardware.

### Tiled GPU memory

Pixels are stored in hardware-friendly tiles rather than simple rows.

This usually improves:

- cache locality
- memory bandwidth usage
- rendering efficiency

### Compressed tiled memory

The image is tiled and also stored in a hardware-understood compressed form.

This can save bandwidth and power, but it requires that all users of the image understand that layout.

This is one major reason modifiers are so important.

## 11. Texture, EGLImage, and Framebuffer

These concepts sit in the rendering half.

### Texture

A texture is a GL image resource that shaders can sample from.

For a compositor, a client surface often becomes a texture so it can be drawn onto the final output.

### EGLImage

An `EGLImage` is an EGL/native image object used for interop.

It is often created from an external resource such as a dma-buf and then attached to GL objects.

The shortest distinction is:

- texture: GL-side sampled image object
- EGLImage: native/interoperable image bridge object

### Framebuffer

A framebuffer is the rendering destination.

It is the thing GL renders into.

Separately, on the DRM/KMS side, a framebuffer object is also the KMS description of how image memory should be interpreted for scanout.

The word "framebuffer" is overloaded, so context matters:

- GL framebuffer: rendering target
- DRM framebuffer: scanout description for KMS

## 12. A Concrete End-to-End Story

This is the shortest realistic "one frame" narrative for this repository:

1. A Wayland client creates a `wl_surface`.
2. The client creates a buffer, for example through `wl_shm`.
3. The client attaches that buffer and commits the surface.
4. The compositor records that the surface now has a new front buffer.
5. The buffer data is imported or uploaded into a GL-usable form.
6. The compositor scales the logical scene to the output buffer's pixel size, then renders it into a buffer allocated through GBM.
7. That output buffer is represented as a dma-buf with a format and modifier.
8. DRM/KMS creates a framebuffer object for that memory.
9. An atomic commit binds that framebuffer to a plane on a CRTC attached to a connector.
10. The display engine scans out the pixels to the monitor.

That is the entire stack in motion.

## 13. Practical Reading Order for This Repository

If coming back after a long time, this order should restore the mental model quickly:

1. `src/main.rs` and section 16
   Understand startup, including the reaper and the forker, and which subsystems exist.
2. `src/runner.rs`
   Understand the action-driven runtime.
3. `src/state.rs`
   See what global state the compositor owns. Windows, the layout, and the `Toplevel` trait are section 18 and `src/window.rs`.
4. `src/clients.rs`
   Rebuild the Wayland server/client mental model.
5. `src/wayland_objects/wl_display.rs`
   Remember how the protocol bootstrap starts.
6. `src/wayland_objects/wl_registry.rs`
   Remember how globals are advertised.
7. `src/wayland_objects/wl_surface.rs`
   Rebuild the attach/commit model. The X role and its serial are section 17.
8. `src/wayland_objects/wl_buffer.rs` and `src/wayland_objects/wl_shm*.rs`
   See where client image data enters the compositor.
9. `src/render/`
   See how a DRM device chooses OpenGL ES or Vulkan, then how that backend imports a buffer.
10. `src/drm.rs` and `src/output_scale.rs`
   Rebuild KMS resources, framebuffers, and display output. The frame plan is logical until `scale_scene_items`.
11. `src/gbm/gbm.rs`
   Understand output buffer allocation.
12. `src/input.rs`, `src/udev.rs`, `src/dbus.rs`
   Rebuild device discovery and session/device access.
13. `src/x11/` and sections 19 and 20
   The X11 codec, the sans-IO connection, startup, and the io_uring driver. Section 20 is the XWM that owns them.
14. `src/xwayland/xwm/` and section 21
    When a normal X window becomes a tema window, how the layout drives `ConfigureWindow`, and how an override-redirect window stays above that content.
15. `src/xwayland/xwm/focus.rs` and section 22
    How keyboard focus becomes X input focus: the input model, clicks, map and close, `FocusIn`, and a client's `_NET_ACTIVE_WINDOW`.
16. `src/capture.rs` and section 23
    How a managed X window is announced to taskbars and copied by toplevel image capture.
17. `src/xwayland/xwm/selection.rs`, `src/xwayland/xwm/transfer.rs`, and section 24
    How `CLIPBOARD` and `PRIMARY` move between X clients and Wayland clients.
18. `src/wire_scale.rs` and section 25
    How downscaled Xwayland gets one integer scale, and where that scale is applied.
19. `src/ipc/` and section 26
    The IPC socket, the snapshot diff, and `temactl`.
20. `src/wayland_objects/xdg_system_bell_v1.rs`, `xdg_toplevel_tag_manager_v1.rs`, `xdg_toplevel_icon_v1.rs`, `xdg_session_manager_v1.rs`, `xdg_toplevel_drag_v1.rs`, and section 27
    The five staging protocols: bell, tag, icon, session restore, and toplevel drag.

## 14. Vocabulary

### Wayland

- compositor: the server that clients talk to and that decides what appears on screen
- client: an application using the Wayland protocol
- global: an advertised protocol capability such as `wl_compositor` or `wl_shm`
- surface: a client-visible drawable object
- window: one managed toplevel, identified by a `WindowId` for as long as its role object lives
- role: the semantic meaning of a surface, such as xdg toplevel
- buffer: image data attached to a surface
- attach/commit: Wayland's "pending state then apply atomically" model

### Rendering

- render: generate pixels into a destination
- texture: image resource sampled during rendering
- shader: program run on the GPU during drawing
- blend: combine source pixels with destination pixels
- blend state: the rules controlling how that combination happens
- viewport: map normalized output coordinates into a target rectangle
- scissor: rectangular clip on writes
- GL context: the current OpenGL ES stateful execution environment
- rendering context: general term for that stateful rendering environment
- logical pixel: layout, input, and protocol unit. `State::output_bounds` is in these
- physical pixel: one framebuffer pixel. The frame plan is physical after `scale_scene_items`
- output scale: the v120 fraction on an output. A logical length times that scale is the physical length when the scale divides the mode

### EGL / GL

- OpenGL ES: one rendering API used to draw images
- Vulkan: the other rendering API. It imports the same dma-bufs and does not use EGL
- GL: shorthand for the OpenGL API family or its function set
- EGL: bridge between native graphics resources and GL
- EGLImage: EGL-side interoperable image object, often imported from dma-buf

### Buffer / memory

- dma-buf: Linux shared-buffer mechanism based on file descriptors plus image metadata
- GBM: Generic Buffer Manager, used to allocate graphics/display-friendly buffers
- GEM object: kernel/driver graphics memory object
- format: what the pixels mean
- modifier: how the pixels are laid out in memory
- linear: simple row-by-row memory layout
- tiled: hardware-optimized non-linear memory layout
- compressed tiled: tiled layout with hardware-understood compression

### Display / KMS

- DRM: Linux kernel graphics subsystem
- KMS: kernel mode setting, the display-control side of DRM
- framebuffer: image memory description used for rendering or scanout, depending on context
- scanout: display engine reading pixels from memory to drive the monitor
- page flip: switching scanout from one framebuffer to another
- tearing: an async page flip that can change the framebuffer during scanout, so one refresh shows two frames
- direct scanout: scanning out a client buffer directly without an extra composition step
- connector: output endpoint such as HDMI or DP
- CRTC: display timing and scanout engine
- plane: hardware image layer attached to a CRTC

### X11

- X connection: `XConnection`, the sans-IO core. The owner moves the bytes
- sequence: the 64-bit count of requests on one connection. The wire carries the low 16 bits
- pending queue: sent requests, in order, waiting for a later sequence to retire them
- sync point: the GetInputFocus that `take_outgoing` appends after void requests, so those voids can retire
- atom: a numeric id for a string. `NAMES` are interned at startup. `PREDEFINED` are fixed values from the generated constants
- XWM: the window manager in section 20. It owns the X connection for one Xwayland run and claims `WM_S0`
- check window: the 10×10 window created when the XWM claims its role. It owns `WM_S0` and `_NET_WM_CM_S0`. Its id is `_NET_SUPPORTING_WM_CHECK` on the root and on itself, and its `_NET_WM_NAME` is `tema`
- `WM_STATE`: the ICCCM property, type `WM_STATE`, format 32, two CARD32s `[state, icon_window]`. Normal is `[1, 0]`. Iconic is `[3, 0]`. Withdrawn is `[0, 0]`
- managed X window: a normal X window that section 21 has turned into a `WindowId`. An override-redirect window is not one
- `xwayland_surface_v1`: the role object that links one `wl_surface` to an X window. The X role remains after the object is destroyed
- serial: the non-zero `u64` from `set_serial`, low half in `serial_lo` and high half in `serial_hi`. It is pending until `wl_surface.commit`, then stored on the current Xwayland run

## 15. Final Mental Shortcut

When this repository feels too abstract, reduce it to this:

- Wayland decides which client buffer should be visible.
- The renderer, OpenGL ES or Vulkan, draws those buffers into an output image.
- GBM allocates the buffers the GPU and display hardware want.
- dma-buf lets those buffers be shared without copying.
- DRM/KMS tells the display hardware what to show.
- connector, CRTC, and plane are the hardware display graph.

That is the whole system.

## 16. Processes

The compositor does not fork. The process that was started from the terminal becomes a reaper, and a helper process, the forker, creates every child. Keybinding commands and Xwayland use this path.

```text
tema reaper          original pid, child subreaper, terminal process group
├── tema             compositor, death signal SIGKILL
└── tema forker      double-forked, reparented to the reaper, own session,
                     death signal SIGKILL
    └── sh -lc …     own process group, then the requested program
```

`ps -o pid,ppid,pgid,sid,comm` shows those names. `comm` is at most 15 characters. The compositor keeps the binary name `tema`. The reaper and the compositor stay in the terminal's process group. The forker calls `setsid`, so its session id is its own pid. Each spawned child calls `setpgid(0, 0)` before `exec`, so its process group is its own pid.

Startup order in `main`, before the logger and before EGL:

1. `become_reaped` resets signals, marks this process a child subreaper, and forks. The subreaper flag is not inherited, so only the reaper has it.
2. The child sets `PDEATHSIG` to `SIGKILL` and continues only if its parent is still the reaper.
3. The parent names itself `tema reaper`, forwards `SIGINT`, `SIGTERM`, and `SIGHUP` to the compositor, and `wait`s. The handler only calls `kill`. Forwarding `SIGINT` matters: the default action would kill the reaper, and the compositor's death signal would then `SIGKILL` it before DRM and logind cleanup.
4. The compositor checks that it still has one thread, then starts the forker. A second thread aborts. The forker is double-forked so the reaper, not the compositor, is its parent.
5. If the forker cannot be created, that error is logged after the logger is installed and startup continues.

The reaper and the compositor keep the terminal's standard streams. The forker puts `/dev/null` on descriptors 0, 1, and 2, moves its socket to descriptor 3 with close-on-exec, and closes everything from 4 up. Children inherit `/dev/null` unless a spawn passes its own descriptors for those slots. A keybinding spawn passes none.

The reaper exits when the compositor exits, with the same code, or `128 + signal`. A status that does not fit in a byte becomes 1. It does not wait for other children. Programs that are still running are reparented and keep running. The forker dies with the reaper because of its death signal. While the reaper is alive it reaps every child, including a daemon whose parent already exited.

On shutdown the runner stops Xwayland, then `ClientManager`, then `ProcessManager`. The process manager sends the activation unset below, then drops the socket. The forker sees the close and exits 0.

### File descriptors

| Owner | What it holds |
| --- | --- |
| Reaper | The terminal's standard streams. No extra descriptors. |
| Compositor | The terminal's standard streams, one end of the forker socket, and a pidfd for the forker. A `Spawned` reply also carries a pidfd for that child. A keybinding drops that pidfd after logging. The forker keeps its own copy and is the one that reports the exit. |
| Forker | Socket on descriptor 3, close-on-exec. `/dev/null` on 0–2, inherited across `exec`. One pidfd per live child. The exec-error pipe exists only until `exec`. |
| Child | Descriptors named by the spawn's slot list, or `/dev/null` on 0–2. |

One seqpacket is one message. Descriptors travel beside the bytes with `SCM_RIGHTS`, at most 32 of them. The bytes are bincode, little-endian, fixed-width integers, at most 32 KiB, with no trailing bytes. Variant order is the numeric tag, so the variants stay in the order below.

Sends are one at a time. An `io_uring` completion can arrive out of submission order, and a `SetEnv` has to reach the forker before a later `Spawn`.

### Messages

Compositor to forker:

- `SetEnv { key, value }`. `value` absent removes `key` from the forker. Later spawns inherit the result.
- `Spawn { token, program, args, env, slots }`. `args` does not include `argv[0]`. `slots[i]` is the child descriptor number of the i-th attached descriptor. `env` overrides the forker's environment for this child only.

Forker to compositor:

- `Log { level, message }`. `level` is `log::Level` as a byte (`Error` is 1). Logged as `forker: …`.
- `Spawned { token, pid }`, plus the child's pidfd.
- `SpawnFailed { token, error }`. The child never reached `exec`.
- `Exited { token, exit }`. `exit` is `Code(n)` or `Signal(n)`.

The forker removes `DISPLAY` and `WAYLAND_DISPLAY` from itself and sets `XDG_SESSION_TYPE=wayland`. After `ProcessManager` starts, the compositor sends:

- `WAYLAND_DISPLAY=tema-0`
- `XDG_CURRENT_DESKTOP=tema`
- `TEMA_SOCK` set to the IPC socket path from section 26
- `_JAVA_AWT_WM_NONREPARENTING=1`

`DISPLAY` is not sent again, so a compositor nested under X does not pass that outer display to new programs.

A keybinding runs `sh -lc <command>` with no extra environment and no extra descriptors. Failure is a warning: `failed to spawn`, the command in backticks, then the error. A non-zero exit, including death by signal, is a debug line. A zero exit is quiet.

### Degraded mode

The compositor keeps running when the forker is missing.

- `spawn_forker` fails at startup: one error in the log. `ProcessManager` has no socket.
- The forker's pidfd poll fires, a receive fails, or a read returns no bytes: an error in the log, the poll is removed, and the socket is dropped. A spawn that has not reached `Spawned` fails with `forker died`. A child that was already running is marked lost. Its exit will not arrive, and a keybinding does not log that again.
- Any later spawn fails immediately with the warning `forker is unavailable`.

`Stop` uses the same notification with `forker stopped`. That is a normal shutdown, not a death.

### Activation environment

D-Bus-activated programs and systemd user units do not inherit the forker. tema publishes a separate copy, one helper at a time, through the forker. `[general] import_environment` controls that export. It defaults to `true`. A nested session never exports or unsets, whatever the config says.

At startup, after the Wayland socket exists and after the forker `SetEnv`s, tema applies the config value and records the session variables. While the switch is on, one job exports:

- `WAYLAND_DISPLAY=tema-0`
- `XDG_CURRENT_DESKTOP=tema`
- `TEMA_SOCK` set to the IPC socket path from section 26
- `XDG_SESSION_TYPE=wayland`

`_JAVA_AWT_WM_NONREPARENTING=1` stays on the forker. Section 17 adds `DISPLAY=:N` when a display is allocated, and removes it on give-up or disable. A restart on the same display exports nothing new. `Xwayland`'s `Stop` leaves `DISPLAY` in place. The shutdown unset below removes it with the rest, when it was exported.

Every export and unexport is recorded as what tema wants published. A job is queued only while the session is allowed to export and the switch is on. The exported set is what a helper was asked to set.

A reload that changes the switch logs `general.import_environment changed to true` or `false` and applies it. Turning it on publishes the wanted set in one job. Turning it off unsets the exported set in one value-matched job. A set that is still waiting stays ahead of that unset. The action is queued before `xwayland.enabled` from the same reload, so the session variables go out before `DISPLAY`.

A set is `sh -c` with this script, `$0` set to `tema-activation`, and one `K=V` argument per variable:

```text
if command -v dbus-update-activation-environment >/dev/null 2>&1; then
    exec dbus-update-activation-environment --systemd "$@"
else
    exec systemctl --user set-environment "$@"
fi
```

An unset is `systemctl --user unset-environment K=V…`, using the value tema exported. systemd removes the variable when it still has that value, so another tema that exported a different value keeps its own.

Exporting a value that is already recorded queues nothing. The next job starts when the previous helper has exited, failed, or been lost. A missing forker fails the job through the usual spawn path, and the queue continues with the next one.

On shutdown, `ProcessManager::stop` sends `SIGKILL` to a helper that is still running, so an export cannot land after the unset. It then writes one unset spawn, listing every variable still exported, straight onto the forker socket, and then drops the socket. Only what was exported is unset, so a session with the switch off sends nothing. The forker runs a spawn it has already read before it treats the close as exit. The helper finishes after tema is gone. `EAGAIN` or any other send error logs `could not unset the activation environment: …` and shutdown continues.

When `DISPLAY` is already set in tema's own environment, tema is nested under X. The process manager is created with export not allowed. The config switch cannot override it.

D-Bus `UpdateActivationEnvironment` stores an empty value as `K=`. Removing a name is the systemd unset. With dbus-broker, and with dbus-daemon in systemd mode, activation goes through systemd, so that unset is what later services see. dbus-daemon without systemd keeps the old activation value.

### Manual check

Run a normal session from a terminal, with the example bindings (`Super+Return` spawns `alacritty`).

1. `ps -o pid,ppid,pgid,sid,comm` shows `tema reaper` as the parent of both `tema` and `tema forker`. The forker's session id is its own pid.
2. `Super+Return`. The new process is in its own process group, and `ls -l /proc/<pid>/fd/0 /proc/<pid>/fd/1 /proc/<pid>/fd/2` points all three at `/dev/null`.
3. Ctrl+C in the terminal. The session exits 130. The forker is gone. `ps` shows no leftover `tema` zombie.
4. Start again and `kill -9` the compositor pid. The reaper exits 137 and the forker dies.
5. Start again and `kill -9` the forker pid. The compositor logs `forker died` and stays up. Another `Super+Return` warns `forker is unavailable`.
6. Spawn `sleep 300 &` from a keybinding (`sh -lc` is added by the compositor, so the command itself is `sleep 300 &`). `ps -o pid,ppid,comm` shows that `sleep` is a child of `tema reaper`.

## 17. Xwayland

Xwayland is enabled by default. At startup the compositor claims one X display and publishes it to the forker as `DISPLAY=:N`, and exports that same value through section 16. No Xwayland process exists yet. The first connection to the listening socket is what spawns it. The compositor's own environment stays as it was, including when tema is nested under an outer X server. Programs the forker starts receive `:N`. Section 16 publishes it while `[general] import_environment` is on and the session is not nested.

```toml
[xwayland]
enabled = true
```

`enabled = false` at startup logs `Xwayland disabled` and skips the display. A reload that changes `enabled` logs `xwayland.enabled changed to true` or `false` and applies it. `SetEnabled` is queued before a `scaling_mode` change from the same reload. `scaling_mode` is `default` or `downscaled`. A reload applies it live. Section 25 is that mode.

Turning `enabled` on from off, or from give-up, resets the early-failure count, allocates a display, sets and exports `DISPLAY=:N`, and polls. Turning it off while only listening removes the poll, both files, and `DISPLAY`, and logs `Xwayland disabled`. Turning it off while Xwayland is launching or running unlinks the socket, keeps the lock, clears and unexports `DISPLAY`, and logs `Xwayland disabled; waiting for X clients to exit`. That server keeps running, including its window manager, until its clients leave. A spawn that arrives after the disable is adopted and left running. When that server exits, the lock is dropped and the log says `Xwayland exited after being disabled`. Enabling again during that wait leaves `DISPLAY` unset until the exit, then allocates a fresh display, usually the same number, and exports it. After `Stop`, a later enable does nothing. A lost forker still gives up. A second unexport of a name that is already gone is ignored.

```text
Start
  disabled, or no display available → stay down, no DISPLAY
  otherwise                         → Waiting
                                      /tmp/.X11-unix/XN and /tmp/.XN-lock
                                      forker DISPLAY=:N, and export DISPLAY=:N
                                      poll the socket, no Xwayland process

connection
  → Launching
      Spawned → Running
                read displayfd and stderr
                adopt the Wayland socket as the Xwayland client
      Failed  → count an early failure
  displayfd text → ready, log "Xwayland ready on :N"
  Wayland client drops → SIGKILL that run; the exit follows

exit of a good run → Waiting again, same :N, same files, no new export
early failure      → Waiting again, until the third in a row
third early failure, a listen error, or a lost forker
                   → GivenUp
                     unlink both files, clear forker DISPLAY, unexport DISPLAY

Waiting → Disabled
  disable: remove the poll, unlink both files, clear and unexport DISPLAY
  log "Xwayland disabled"

Running or Launching → Draining
  disable: unlink the socket, keep the lock, clear and unexport DISPLAY
  log "Xwayland disabled; waiting for X clients to exit"
  the server and its window manager keep running

Draining → Disabled
  the server exits: drop the lock
  log "Xwayland exited after being disabled"

Draining → Waiting
  enable again, then the server exits: allocate a display, export DISPLAY, poll

Disabled or GivenUp → Waiting
  enable: reset the early-failure count, allocate, export DISPLAY, poll

Stop → unlink both files, SIGKILL a running server, leave DISPLAY set
       section 16 unsets it with the rest of the activation environment
```

A completion tagged with an older launch is ignored. A pid that arrives after `Stop` is killed and not adopted.

### File descriptors

Xwayland is started as `Xwayland -terminate 10 -rootless -verbose 10 -displayfd 3 -listenfd 4 -wm 5`. Descriptors 0 and 1 stay `/dev/null`. The spawn also sets `WAYLAND_SOCKET=6` and removes `LIBEI_SOCKET`.

| Slot | In Xwayland | Kept by the compositor |
| --- | --- | --- |
| 2 | stderr, write end | read end. Whole lines are logged as `Xwayland: …` |
| 3 | `-displayfd`, write end | read end. The first bytes mark the server ready |
| 4 | `-listenfd` | the same listening socket, duplicated |
| 5 | `-wm` | the other end of the socketpair, held open |
| 6 | `WAYLAND_SOCKET` | the other end, adopted as a Wayland client |

The adopted client is logged as `new client_id=… command=… xwayland=true`. Its pid is Xwayland's. A socketpair's peer credentials would otherwise name the compositor.

### Lock file and socket

Displays `500` through `1499` are tried in order. Display `N` is the filesystem socket `/tmp/.X11-unix/XN` plus the lock `/tmp/.XN-lock`. There is no abstract-namespace socket. `/tmp/.X11-unix` has to already exist, be a directory, and be writable.

The lock is created exclusively, mode `0444`. Its text is the compositor pid in ten columns and a newline, because the compositor owns the display across Xwayland restarts. A lock whose pid is dead is removed and the number is tried again. A live pid, or a file that is not a pid, is left in place and the search moves on. A number whose socket cannot be bound is skipped the same way. The listen backlog is 4096.

Dropping the display unlinks both files. Restart keeps them. Give-up and `Stop` drop them. Disabling while a server is still running unlinks the socket and keeps the lock until that server exits.

### Restart

A run fails early when it never becomes ready, or when it became ready and then exits with a non-zero code or a signal in under ten seconds. A ready exit with code 0 is a clean run at any age. So is a ready run that lasted at least ten seconds, whatever the status. Three early failures in a row give up. Any other run clears the count.

A failed `exec` counts, because the server never became ready. The listening socket still has the unaccepted connection, so the new poll wakes at once and the next attempt follows immediately. A listen error, and a lost forker, give up on the spot. The lost-forker path kills the running server first.

Give-up logs `Xwayland gave up: …`, unlinks both files, removes `DISPLAY` from the forker, and unexports it. `Stop` leaves `DISPLAY` set on the forker. Section 16 clears the activation environment when the process manager stops.

### `WM_S0`

`-wm 5` is always passed, and our end of that socket stays open. Displayfd readiness starts the window manager in section 20 on that socket. Before that window manager owns the `WM_S0` selection, it sends `XFixesSetClientDisconnectMode` with the terminate flag. Xwayland accepts clients on the listen socket once that selection is owned. The flag tells the server this connection may stay open while the server exits, so `-terminate 10` waits ten seconds after the last application disconnects and then exits.

### Xwayland-only globals

A global may answer `xwayland_only`. The default is no, so the other globals stay visible to every client. An Xwayland-only global is offered only to the Xwayland client, both in the first registry dump and when the global appears later. A normal client that binds the name is disconnected with `invalid global name`, the same response as an unknown name. Removal is sent only to registries that were offered the name.

### `xwayland_shell_v1`

`xwayland_shell_v1` version 1 is registered from `Globals::initialize`. It is the `xwayland_only` global. A normal client's `wl_registry` omits it, and so does `wayland-info` run as that client. The bind is logged at debug: `Xwayland client ClientId(…) bound xwayland_shell_v1`.

The objects live in `src/wayland_objects/xwayland_shell_v1.rs` and `src/wayland_objects/xwayland_surface_v1.rs`. `get_xwayland_surface` gives the `wl_surface` the X role and links the new `xwayland_surface_v1`. It is `xwayland_shell_v1.role`, and the client is disconnected, when the surface already has another role or already has an `xwayland_surface_v1`. Destroying the shell leaves the child objects in place. There is no `WL_SURFACE_ID` path. Xwayland 24.1 speaks this shell.

### The X surface role

The role is `Role::XwaylandSurface`. `get_xwayland_surface` assigns it, and it stays after `xwayland_surface_v1.destroy`. That destroy clears the link, so a later `get_xwayland_surface` is accepted. A serial already committed stays committed.

`set_serial` builds `serial_lo | (serial_hi << 32)`. Each client remembers the highest serial it has accepted. The memory starts at 0, so 0 and any serial that is not strictly higher are `invalid_serial` and the client is disconnected. The memory moves when `set_serial` is accepted, and the value is stored as pending on the surface. A later `set_serial` before the commit replaces that pending value.

`wl_surface.commit` applies the pending serial after the mapping and sync checks. The committed serial is set, the pending value is cleared, and one `XwaylandAction::Surface` with `SerialAssigned` is queued. A commit with nothing pending queues nothing, and a second commit does not send the serial again. A commit that fails those checks drops the pending serial and leaves the committed serial empty. `set_serial` after a committed serial is `already_associated`.

`wl_surface.destroy` while the `xwayland_surface_v1` still exists is `wl_surface.defunct_role_object`. The client is disconnected, and both objects stay. After the role object is gone, destroying the surface succeeds. A committed serial queues `Destroyed` with that serial and the surface. With no committed serial, that destroy queues nothing.

A hidden X-role root stays out of the scene, pointer picking, and rendering. `register_root_surface` still records it. Section 20 shows it, as an unmanaged root at its X position, once the window is paired and mapped. Keyboard focus restore still skips every X-role surface.

### Surface events

`XwaylandAction::Surface` carries `SerialAssigned` or `Destroyed`, and the client id. The types live in `src/xwayland/surfaces.rs`. The current run applies the event when that client is the adopted Xwayland client. Another client, or an event while Xwayland is not running, is logged at debug and dropped. `Destroyed` removes the serial when the stored surface is the one in the event. `ClientClosed` clears the map. `surface_for_serial` reads it. A successful assign is stored there and is not logged on its own.

The window manager in section 20 pairs the X client message `WL_SURFACE_SERIAL` with that map. The message serial is `data[0] | (data[1] << 32)`, the same packing as `set_serial`. The lookup is `surface_for_serial`.

### Manual check

Run a normal session from a terminal. Note the `:N` in `Allocated display :N`.

1. `ls /tmp/.X11-unix/XN /tmp/.XN-lock` shows both files. `pgrep -x Xwayland` prints nothing.
2. Spawn a terminal with the usual keybinding. `echo $DISPLAY` prints `:N`.
3. `Xwayland -version` reports 24.1 or newer. Run `xterm` with `RUST_LOG=debug` on the compositor. The log shows Xwayland starting, `new client_id=… xwayland=true`, one or more `Xwayland:` lines, `Xwayland ready on :N`, `bound xwayland_shell_v1`, and `XWM ready on :N`. The window checks are in sections 20 and 21.
4. `kill -9 $(pgrep -x Xwayland)`. The log shows `Xwayland exited with signal 9`. A new `xterm` starts Xwayland again on the same `:N`, and both files are still there. A signal within ten seconds of becoming ready counts as an early failure, so one kill relaunches, and a third early failure gives up.
5. Start a session whose `PATH` can see `sh` and the keybinding's terminal, and cannot see `Xwayland`. `mkdir -p /tmp/no-xwayland`, symlink those two programs into it, and run tema with `PATH=/tmp/no-xwayland`. From a normal shell, run `xterm` with `DISPLAY=:N`. The connection stays queued, so each failed launch tries again at once. The third logs `Xwayland gave up: repeated early failures`. Both files are gone. A terminal spawned after that has an empty `DISPLAY`.
6. Activation environment, the live `import_environment` switch, and the live `enabled` switch. Items 1–7 and 9–11 are a session that is not nested under X. Item 8 is a nested one.
   1. Start tema. `systemctl --user show-environment | grep -E 'DISPLAY|WAYLAND_DISPLAY|XDG_CURRENT_DESKTOP|XDG_SESSION_TYPE|TEMA_SOCK'` shows `WAYLAND_DISPLAY=tema-0`, `DISPLAY=:N`, `XDG_CURRENT_DESKTOP=tema`, `XDG_SESSION_TYPE=wayland`, and `TEMA_SOCK` set to the socket from section 26. Restart a D-Bus-activated service with `systemctl --user restart xdg-desktop-portal` and read `tr '\0' '\n' < /proc/$(pgrep -xf '.*xdg-desktop-portal')/environ`.
   2. Open `xterm`. Set `enabled = false` and save. The log shows `waiting for X clients to exit`. `ls /tmp/.X11-unix/XN` is gone and `/tmp/.XN-lock` remains. A new terminal has no `DISPLAY`. `systemctl --user show-environment` has no `DISPLAY`. The open `xterm` still works. `xterm -display :N` fails.
   3. Close `xterm`. Xwayland exits, the lock file is gone, and nothing relaunches.
   4. Set `enabled = true`. `DISPLAY=:N` is back in a new terminal and in systemd. `xterm` starts Xwayland on the first connection.
   5. With `xterm` open, set `enabled` false and then true before it exits. After closing `xterm`, a fresh display is allocated and `DISPLAY` is exported again.
   6. Start with `enabled = false`, then set it true and save. This matches item 4. The startup log says `Xwayland disabled`, and neither file exists until the reload.
   7. Quit tema. `systemctl --user show-environment` no longer has `DISPLAY`, `WAYLAND_DISPLAY`, `XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, or `TEMA_SOCK`, unless another instance exported a different value.
   8. Run tema nested under X11. `systemctl --user show-environment` is unchanged. `import_environment = true` leaves it unchanged too.
   9. Start with `import_environment = false`. `systemctl --user show-environment` has none of `DISPLAY`, `WAYLAND_DISPLAY`, `XDG_CURRENT_DESKTOP`, `XDG_SESSION_TYPE`, or `TEMA_SOCK`. `xterm` still works. Quitting changes nothing.
   10. Set `import_environment = true` and save. The five variables appear, and `DISPLAY` is the current `:N`.
   11. Set it back to `false` and save. Those variables are removed. A second tema instance's different values stay.
7. Quit tema during a normal session. No `Xwayland` process remains, and both files are gone.
8. `wayland-info` from a normal client does not list `xwayland_shell_v1`.

## 18. Windows

A managed toplevel is a window. The layout, workspaces, grabs, closing snapshots, keybindings, and capture talk about a `WindowId` and reach shell behavior through the `Toplevel` trait. `XdgToplevel` and `XToplevel` implement it. `XToplevel` is section 21. The types live in `src/window.rs`. The layout tree lives in `src/tiling.rs`.

### Lifetime

`xdg_surface.get_toplevel` allocates the `WindowId` and calls `insert_window`. The first id is 1. Each value is used once.

`WindowEntry` stores the root `wl_surface` and the `ObjectId` of the role object. `windows` is keyed by `WindowId`. `window_by_root` finds the window that currently owns a root. A root hosts at most one window.

`insert_window` also creates the `ManagedWindowState`: tiled, waiting for its first layout, on the workspace the root already belongs to. `destroy_window` drops that state and takes the window out of the workspace layout.

The id is released when that `xdg_toplevel` is destroyed, when its client disconnects, or when the root is unregistered. Unregistering a root is what a deleted `wl_surface` does.

### A new toplevel on the same surface

Destroying the `xdg_toplevel` drops the window and frees the root. The next `get_toplevel` on that same `wl_surface` receives a new `WindowId` and a fresh `ManagedWindowState`. It is tiled and placed like a window that just appeared. Capture does the same for foreign-toplevel handles: `by_window` maps the `WindowId` to a `ToplevelKey`, and a later map receives a new key.

### The trait and the grab rule

`State::toplevel` and `toplevel_mut` read `window_role_object`, then downcast through `Object::as_toplevel`. They panic when that role object is already grabbed. A request handler that holds the role uses `self`. `window_role_object` only reads the map, so it stays safe during the grab. Capture updates that only touch the capture registry are safe from inside the handler. `map_window` calls `state.toplevel`, so it runs from the `wl_surface` commit, where the toplevel is free.

The trait covers the title, the app id, the parent window, size limits, the maximize and fullscreen requests, the decoration mode, close, interactive resize, a fullscreen request made by the compositor, and `position_changed`. The xdg protocol actions that still name an `xdg_toplevel` are close, interactive resize configure, and the move and resize requests. `position_changed` is a no-op for xdg.

### What stays on the surface

Keyboard focus is a `SurfaceFocus` holding the root surface. `focused_window` reads that surface and looks up the window.

These maps stay keyed by the root surface: the geometry cache, output membership, presentation, process info, `Workspace.root_surfaces`, and `root_surface_workspaces`. Popups, layer surfaces, cursors, and role-less roots stay surfaces. They have no `WindowId`.

`WorkspaceLayout` is keyed by `WindowId`: tiled leaves, the floating stack, and fullscreen. The scene still lists root surfaces. It calls `window_for_root` when it needs the window's layout or animation state.

### X windows

A managed X window uses this same `WindowId`, `ManagedWindowState`, and `Toplevel` path. The role object is an `XToplevel` created with `insert_object`. It is not in any client's id map. `insert_window` runs when the window becomes managed. `destroy_window` runs on unmap, on `DestroyNotify`, and when the WM stops. The next map allocates a new `WindowId`. Section 21 is that path.

### Interactive resize

Press and hover share one decision. The scene pick in `src/input/pick.rs` returns a `CompositorZone`. `window_interaction_intent_at` and `decoration_hover_cursor_icon_names` both read that zone. Inside a zone the pick's `surface_focus` is empty, so the client gets a pointer leave and the compositor draws the resize cursor.

A floating window produces `WindowResize`. The grab is the frame plus `decorations.resize_grab_extent` logical pixels outside it. The default is 10, read from `state.config` when the zone is built. A corner spans `rounding + border + 10`, and no more than half the content width or height. A transparent rounded corner inside the content is a corner. That geometry lives in `src/input/resize_zone.rs`. A maximized or fullscreen window has no zone. With `border.size = 0` the outer ring and the curved corners remain.

A tiled window produces `SplitResize`. `src/tiling.rs` rebuilds the split handles at the end of each layout. A handle covers the two borders and the gap between siblings, and is at least `max(resize_grab_extent, 4)` pixels across. An edge is a handle when some ancestor has a neighbour on that side. A corner where a horizontal handle meets a vertical one resizes both. The cursor uses the xdg resize-edge bits.

The pick walks the output from the top. An `Overlay` or `Top` layer wins. A floating window is next: its extent wins over whatever is below it, and a popup of that window keeps pointer focus and clears the zone. Split handles come after that, above the tiled windows and above `Bottom` and `Background` layers. A point in the gap has a zone and no window. Hover and press still start the resize. Mod+drag and `xdg_toplevel.resize` still name a window. They use `resize_edges_for_pointer` on the binding rect, then `begin_window_edge_drags`, and become the same split drags.

The pointer grab is `ResizeSplit` in `src/input/types.rs`. It stores the workspace, the drags, the cursor edges, and an optional origin window. It is not tied to one client. While the grab is held, `SNAP_TILED_RESIZE` makes the tiles jump to the new rectangles. Keyboard resize stays on `State::resize_tiled_window` and keeps the window animation. The buffer that fills a jumped or animating tile is the subject of the next section.

### Tiled content during resize

A tiled window stores the content rect from the last time its buffer had that size. The field is `tiled_content_basis` on `ManagedWindowState`. `State::refresh_tiled_content_basis` updates it on commit and when a relayout moves the frame, and only while the surface is mapped, tiled, and not fullscreen. `anchored_content_origin` in `src/root_geometry.rs` places the current buffer against that rect. On each axis, a buffer that still has the old size stays on the edge that did not move: dragging the top edge keeps the bottom still, and the gap is on the edge that moved. A tile that only translates, or a buffer that already matches the tile, follows the tile. Floating windows keep their frame where it was placed (see Floating position). Fullscreen skips the anchor. Sway's `view_center_and_clip_surface` pins every tiled surface to the content top-left. Tema pins the edge that stayed.

`State::window_render_mapping` is the one logical-to-visual pair for a window. `render_composed_entry` in `src/drm.rs` reads it, and so do `logical_pointer_for_surface` and `visual_pointer_for_surface`. The frame pair moves the border, the shadow, and the rounded clip from the tile frame onto `geometry_animation.visual_rect()`, through `place_visual_frame`. The content pair moves the root's surfaces, including subsurfaces and popups, and maps pointer coordinates back into the buffer. `None` means the two rectangles are the same. A direct floating move reads the frame pair through `window_render_mapping_with_logical_fallback`: the toplevel is grabbed, `State::toplevel` would panic, and an empty geometry cache falls back to the move intent's target rect. `geometry_sample_influence` still places a caller-supplied sample for damage bounds. That sample is the union used by scheduling, and it is separate from the window pair.

While `tiled_size_answer_pending` is true, the content pair stretches the current buffer onto the visual content box, which is the visual frame inset by the border on each edge. The border thickness stays in pixels. The predicate stays true from the size configure until the commit that follows the newest one, including the ack-to-commit gap that an ordinary layout configure does not cover with `resize_throttle`. It is false for a floating window, a fullscreen window, an unmanaged root, and an X window. An X window is placed with the anchor and stays unscaled, because it has no xdg size configure to answer. After the client commits, the stretch ends. A buffer that still differs from the tile is drawn unscaled at the anchor, and the leftover area is empty.

The stretch is what makes `SNAP_TILED_RESIZE` look full. The tile jumps, and the old buffer fills the new content box until the client commits. A keyboard resize keeps the window animation, and the same stretch runs for as long as the size request and the animation overlap. Direct scanout waits until both pairs are `None`. Hyprland draws into its animated `m_realPosition` / `m_realSize` box. Sway does not stretch a tiled buffer.

### Floating position

A floating xdg window stores its frame origin in `floating_frame_origin` on `ManagedWindowState`. The origin is the global top-left of the outer frame. The frame size is the committed window geometry plus the decoration insets. X11 windows never set the field. They keep their root surface origin as their position, because an X window has no window-geometry offset that could move its frame.

`State::sync_floating_root_position` adopts the origin. It runs in the `wl_surface` commit tail and in the relayout pass. The first call for a mapped, floating, non-fullscreen xdg window without an origin stores the frame of the geometry built from the current root, and moves nothing. Later calls copy the derived root onto the surface when it differs.

`set_window_floating(false)`, `float_window_for_drag`, `retile_window_after_drag`, and `set_window_fullscreen(true)` clear the origin. Restoring a remembered floating state, through `restore_root_surface_floating_state`, sets it from `RememberedFloatingState::layout_rect`.

`build_committed_root_geometry` in `src/root_geometry.rs` derives the root when the window is floating and not fullscreen, has an origin, and has an effective window geometry `wg`. The insets are zero when the frame is not reserved, and otherwise the decoration insets. The content rect is the origin plus the left and top insets, sized `wg`. The frame rect is the origin with `wg` plus the insets. The root position is the content origin minus the `wg` offset. A `root_pos` override is ignored in this branch. A window-geometry offset change therefore moves the root and leaves the frame in place.

Every floating move goes through two `State` methods. `set_root_surface_position` takes a root position and shifts the origin by the difference from the current derived root. `translate_root_surface` adds a delta to the origin. Both fall back to moving the root when the window has no origin. `place_grabbed_root` and the tiled-drag pickup call the first method, and `anchored_grab_target` calls the second.

A left or top interactive resize stores the frame's right and bottom edges in `ResizeAnchor`. `State::apply_resize_anchor` runs in the commit tail. It reads the fresh `layout_rect_global` and moves the frame so those edges stay put. The anchor is in frame space, so the edges stay fixed when the client changes its window-geometry offset during the resize.

This follows jay's `update_surface_position`, which places the surface at the desired extents minus the effective geometry position on each geometry change. Hyprland's renderer draws the surface at the window position minus `geometry.box.pos()`.

## 19. The X11 connection

`src/x11/` is the library the window manager in section 20 owns. Section 17 launches Xwayland and holds the `-wm` socket open. This library is what speaks on it.

The owner keeps an `XConnection` and an `XIo` beside the model that drives them. `send` takes a `Redispatcher`, and the reply comes back as an action.

### Codegen

`src/x11/generated.rs` is checked in. The generator lives in `src/x11/gen/` and is compiled only for tests. It reads `/usr/share/xcb/xproto.xml`, `composite.xml`, `xfixes.xml`, `res.xml`, and `render.xml`. `quick-xml` is a dev-dependency. A normal build uses the checked-in file. The test `generated_source_matches_installed_xcb` compares a fresh generation with that file, and returns immediately when `xproto.xml` is absent.

Rewrite the checked-in file with:

```text
cargo test --offline --bin tema regenerate_x11_generated -- --ignored
```

`src/x11/wire.rs` writes the machine's native byte order. The setup request announces that order as `l` or `B`, protocol 11.0, with empty authorization. Every generated struct, reply, and event can both serialize and parse, so a test can play the server. The requests and events that are emitted are the const list in `src/x11/gen/emit.rs`: the core window, property, atom, and selection requests this compositor will use, plus Composite, XFIXES, RENDER, and X-Resource, and the types those items reference.

### The connection

`XConnection::new` returns the core and the setup bytes. The owner writes those bytes. `on_read` accumulates and parses the handshake: 8 bytes, plus the `u16` at bytes 6..8 times 4. Success keeps `roots[0]`, its root window and root visual, `maximum_request_length`, and the resource-id base and mask. The states are Handshake, Ready, and Dead.

Resource ids are `base | (counter & mask)`, stepping by `1 << mask.trailing_zeros()`. The id with every mask bit set is handed out. The next call fails with `IdExhausted`.

`send` and `send_void` append to an outgoing buffer and return the sequence. The first request is sequence 1. A request longer than `maximum_request_length * 4` bytes is `RequestTooLong` and consumes no sequence. An extension request needs `set_extension` first, or `send` returns `ExtensionMissing`.

### Sequences and the pending queue

The wire sequence is 16 bits. The core widens it against the last sequence that carried one: `serial = (last & !0xffff) | wire`, and if that result is below `last`, it adds `0x10000`.

KeymapNotify, code 11, carries no sequence. It leaves `last` where it is and retires nothing.

Every sent request sits in a queue in send order. A message with sequence N retires every entry older than N. A void request retires as success. A request that expected a reply fails with `MissingReply`. The entry whose sequence is N is then taken.

Response type 0 is an error and type 1 is a reply. A reply is parsed and handed to the `Redispatcher` from `send`. Bytes left over after a parsed reply kill the connection, and that waiter receives `Dead`. An error for a waiting request is `Protocol`. An error for a void request, for the sync reply, or for a sequence nobody is waiting on is logged as `X request {name} failed with {label}`.

An event keeps the send-event flag, bit 7 of the first byte, and the parser sees the code with that bit cleared. Code 35 is a Generic Event. Its extra length is the `u32` at bytes 4..8, and it arrives as `Event::Unknown`. Ordinary frames are 32 bytes. Replies and Generic Events add `length * 4` bytes after that.

A read error, end of file, malformed bytes, or a write error marks the connection Dead. Every pending reply then fails with `Dead`, and `Read.dead` carries the reason once. Later reads leave `dead` empty. `send` and `generate_id` return the stored failure.

### Sync points

`take_outgoing` is the flush. When the last committed request was void, it appends a GetInputFocus and records that request as a sync. The reply is discarded. Its sequence retires the void requests in front of it, and the widened sequence keeps moving.

### The io driver

`XIo` holds the `-wm` descriptor. The caller closes it. The owner passes two factories, and each factory builds the `Redispatcher` for one completion, tagged with the generation and, for a read, the `RingId`.

`start` arms one `ReadSome` of 64 KiB. A second `start` while that read is armed replaces the factories and leaves the single read in place. `queue_bytes` takes the setup bytes from `XConnection::new`, which sit outside the outgoing buffer. `flush` calls `take_outgoing` and queues one `Write`. One write is in flight. The ring retries a short write, so a successful completion means the whole buffer was written. Further bytes wait until `on_written`.

A completion whose generation or read id differs from the armed one is dropped. An empty read or a read error logs, disconnects the core once, and stops I/O. When `on_read` has already marked the core dead, `XIo` keeps that first reason. A write error disconnects and cancels the armed read. `stop` cancels the armed read, drops bytes that have not been submitted, bumps the generation, and leaves the core connected. The factories stay, so a later `start` can arm again.

### Startup and atoms

`Startup` is a state machine the owner drives. `start` sends `QueryExtension` for Composite, XFIXES, RENDER, and X-Resource. `QueryVersion` follows for each one that is present: Composite 0.4, XFIXES 6.0, RENDER 0.11, and X-Resource 1.2. A server older than the requested version fails. A newer server version is kept. Composite, XFIXES, and RENDER are required. X-Resource may be absent, and a present copy that is too old still fails. Each present extension is registered on the connection before its version request, so later events and errors use those bases.

The intern batch follows the version replies. Each name in `NAMES` goes out as an `InternAtom` with `only_if_exists` 0, back to back. Replies are stored by the index they were tagged with. One error fails the batch. `PREDEFINED` is copied from the generated `Atom` constants, among them `WM_NAME`, `WM_CLASS`, `STRING`, `CARDINAL`, `ATOM`, and `WINDOW`. Ready means both steps succeeded: `Status::Ready(Bringup { extensions, atoms })`. A startup failure leaves the X connection up. `AtomCache` looks a name up in either direction, and `intern_more` sends names the cache has yet to see.

### Properties

`get_property` sends one `GetProperty`. `long_offset` is 0, `delete` is 0, and `long_length` is `max_bytes / 4`. The reply becomes a `Property` with its type, format, bytes, and `truncated` set when `bytes_after` is non-zero. `Atom::Any` keeps the type the server returns. A requested type that differs, on a property that exists, is a type error. Format 0 is an absent property. `as_u32s`, `as_atoms`, and `as_windows` read native-endian format 32 words. `as_utf8` replaces invalid sequences. `as_latin1` maps each byte to a character. The change helpers build a `Replace` `ChangeProperty`. `delete_property` builds `DeleteProperty`.

### Selections

`ConvertSelection`, deleting `GetProperty`, `ChangeProperty`, `SetSelectionOwner`, `SendEvent`, `SelectionNotify`, `SelectionRequest`, `SelectionClear`, and `XFixesSelectSelectionInput` are in use. The clipboard and primary bridge that drives them is section 24.

### Who owns it

The XWM in section 20 owns the `XConnection` and the `XIo` for the current run. Displayfd readiness starts it. It writes the setup, runs `Startup`, and passes a `Redispatcher` that turns each reply into `XwaylandAction::Wm`. The `WM_S0` claim runs on this connection once startup is ready.

### Trying it against Xwayland

The live test is ignored. Under a Wayland compositor:

```text
WAYLAND_DISPLAY=<socket> cargo test --offline --bin tema x11::live:: -- --ignored --test-threads=1
```

It spawns `/usr/bin/Xwayland :N -rootless -wm <fd> -displayfd <fd>`, drives the connection with `poll`, `read`, and `write`, runs startup, and reads `WM_NAME` on the root. The spawn runs when `WAYLAND_DISPLAY` is set.

## 20. The X window manager

`src/xwayland/xwm/` is the window manager for one Xwayland run. The current `Run` holds it as `wm`. Displayfd readiness, the same moment section 17 logs `Xwayland ready on :N`, calls `Wm::start` with the `-wm` descriptor, the run counter, and the display number. `XwaylandAction::Wm` carries the run. An action from an older run is dropped. The code lives next to the connection in section 19 and uses it. It does not speak Wayland itself. Surface pairing arrives as `XwaylandAction::Surface`.

### Startup

`Wm::start` queues the setup bytes from `XConnection::new` and arms one 64 KiB read. The handshake reaching Ready starts `Startup`. Ready claims the role, logs `XWM ready on :N`, and then uploads the root cursor. A failed handshake, a failed startup, or a send error during the claim logs `XWM failed: …`, cancels the X read, drops `Run.wm`, and SIGKILLs that Xwayland. Section 17's restart policy then applies. A protocol error on one of the claim's void requests is logged with the request name and the connection stays up.

`take_child`, `Stop`, and a lost forker stop the WM before they cancel reads or kill the process. The Wayland client closing does the same through the process exit.

### The role claim

The claim is ten void requests, in this order:

1. `ChangeWindowAttributes` on the root. The event mask is `SubstructureNotify`, `SubstructureRedirect`, and `PropertyChange`.
2. `CompositeRedirectSubwindows` with `Redirect::Manual`.
3. `CreateWindow` for the check window: 10×10, `InputOutput`, depth 0, visual 0, parent the root, placed at (0, 0). The id is the first one `generate_id` returns.
4. `_NET_WM_NAME` `tema`, type `UTF8_STRING`, on the check window.
5. `_NET_SUPPORTING_WM_CHECK`, type `WINDOW`, on the root. The value is the check window id.
6. `_NET_SUPPORTING_WM_CHECK`, type `WINDOW`, on the check window. The value is the check window id.
7. `SetSelectionOwner` of `WM_S0`, owner the check window, time 0.
8. `SetSelectionOwner` of `_NET_WM_CM_S0`, the same owner and time.
9. `_NET_SUPPORTED`, type `ATOM`, on the root. The atoms are `_NET_WM_STATE`, `_NET_ACTIVE_WINDOW`, `_NET_WM_MOVERESIZE`, `_NET_WM_STATE_FOCUSED`, `_NET_WM_STATE_MODAL`, `_NET_WM_STATE_FULLSCREEN`, `_NET_WM_STATE_MAXIMIZED_VERT`, `_NET_WM_STATE_MAXIMIZED_HORZ`, `_NET_WM_STATE_HIDDEN`, `_NET_CLIENT_LIST`, and `_NET_CLIENT_LIST_STACKING`.
10. `_NET_ACTIVE_WINDOW`, type `WINDOW`, value `[0]`, on the root.

### The root cursor

After the claim, the seat's default cursor image is the first frame of `default`, or of `left_ptr` when `default` has none. With an image, the WM sends `RenderQueryPictFormats` and keeps the image until the reply. The format is the direct pictformat of depth 32 whose channels are alpha at 24, red at 16, green at 8, and blue at 0, each with mask `0xff`. The upload is `CreatePixmap`, `CreateGc`, `PutImage` of the premultiplied pixels as native-endian ARGB, `RenderCreatePicture`, `RenderCreateCursor` at the image hotspot, and `ChangeWindowAttributes` of the root cursor. The picture, gc, and pixmap are then freed. The previous root cursor is freed after `ChangeWindowAttributes` replaces it. Section 25 is the pixel size of that image.

A missing image, an image with no pixels, a missing format, a request that does not fit, or a reply error is logged at debug and skipped. The WM stays up.

### Window lifetime

`CreateNotify` records a window other than the check window and other than an id already recorded. A duplicate does not reset the record. The event rectangle is both `geometry` and `pending`. The window then gets `PropertyChange` and `FocusChange`. `override_redirect` is stored from the event.

`MapRequest` on a known window writes `WM_STATE` Normal, which is `[1, 0]`. When `pending` has a positive width and height, it configures the window to 1×1 at (0, 0) and then to `pending`. It then writes `_NET_WM_STATE` from the stored flags and sends `MapWindow`. An unknown window sends nothing. A normal window is also recorded in the client lists, as section 21 describes.

`MapNotify` sets `mapped` and refreshes `override_redirect`. `UnmapNotify` clears `mapped` and writes `WM_STATE` Withdrawn, which is `[0, 0]`.

`ConfigureRequest` on a window that is not shown merges the `X`, `Y`, `Width`, and `Height` bits into `pending`. A shown window, and an unknown window, leave `pending` as it was. `ConfigureNotify` updates `override_redirect` and `geometry`. While the window is not shown, `pending` becomes that geometry. While it is shown, the root moves.

`DestroyNotify` drops the window. When `by_serial` points at it, that entry goes too. A shown surface is hidden.

### Serial pairing

`WL_SURFACE_SERIAL` is a `ClientMessage` of format 32 whose type is that atom, on a window already tracked. The serial is `data32[0] | (data32[1] << 32)`. `last_serial` starts at 0, so serial 0 is ignored. A serial at or below `last_serial` is logged at debug and ignored. A higher serial becomes `last_serial`, the previous `by_serial` entry is removed, the old surface is unpaired, and the new serial is stored. When the run's surface map already holds that serial, the window is paired. `SerialAssigned` pairs in the other order. Pairing requires the X role and a committed serial equal to the message. The log is `X window … paired with …`.

`Destroyed` clears the surface when it is the one stored on that serial's window. The `by_serial` entry stays until `DestroyNotify`.

### Showing

A window is shown when it has a paired surface and `mapped` is set. An override-redirect window is placed at its X geometry and stays an unmanaged root. A normal window waits for the property batch in section 21 and then becomes a managed window. The layout owns that root's position. `xwayland_shown` stays set for both, so the scene, picking, and output assignment see them. Hiding clears the flag, with `try_grab` because the surface may already be gone, and damages the old rectangle. A hidden X root is skipped. The committed buffer supplies the surface size.

`ConfigureNotify` on an unmanaged window updates that position. `ConfigureRequest` is ignored once the window is shown or managed, so a client cannot drag a window that is already on screen. A click focuses the root when section 22 allows it. Mapping a managed window takes keyboard focus under the same policy as an xdg toplevel, and that focus is sent to X. Unlock restores a managed X window. An override-redirect root stays unrestored.

### Properties

Pairing sends `GetProperty` for `WM_CLASS`, `WM_NAME`, `_NET_WM_NAME`, `WM_TRANSIENT_FOR` (`WINDOW`), `WM_PROTOCOLS` (`ATOM`), `WM_HINTS`, `WM_NORMAL_HINTS` (`WM_SIZE_HINTS`), `_MOTIF_WM_HINTS`, `_NET_STARTUP_ID`, `_NET_WM_STATE`, `_NET_WM_WINDOW_TYPE` (`ATOM`), and `WM_WINDOW_ROLE`. The type is `Atom::Any` unless a type is named above. `max_bytes` is 4096. The reply is `WmEvent::Property`. A window that is already gone drops it. `truncated` keeps the bytes that arrived and logs them at debug.

`PropertyNotify` on a tracked window reloads that one atom. An atom outside the list, or a window that is not tracked, sends nothing.

A UTF-8 title, from `WM_NAME` or `_NET_WM_NAME`, replaces `title` and sets `utf8_title`. A later `STRING` `WM_NAME` leaves that title in place. `STRING` before any UTF-8 title is Latin-1. `COMPOUND_TEXT` leaves the title unchanged. A missing `WM_CLASS` clears instance and class. A present value splits on NUL into instance and class. A missing `WM_WINDOW_ROLE` clears the role. `WM_TRANSIENT_FOR` keeps the first window, and a missing or failed read clears it. `WM_PROTOCOLS` and `_NET_WM_WINDOW_TYPE` replace their atom lists when the read succeeds. A missing `WM_HINTS`, a failed read, or a value whose flags omit `InputHint`, sets `input_hint`. `WM_NORMAL_HINTS` keeps at most 18 words. `_MOTIF_WM_HINTS` stores the decorations word, index 2, as 0 when the value is shorter. `_NET_WM_STATE` clears `fullscreen` and then sets each of modal, fullscreen, maximized vertically, maximized horizontally, and hidden whose atom is present. Flags absent from the new value stay as they were, other than fullscreen.

`MapRequest` writes those five flags back as `_NET_WM_STATE`, type `ATOM`, in that order, immediately before `MapWindow`.

### `WM_STATE`

`WM_STATE` is type `WM_STATE`, format 32, and two CARD32s, `[state, icon_window]`. `MapRequest` writes Normal, `[1, 0]`. `UnmapNotify` writes Withdrawn, `[0, 0]`. The icon window is 0.

### Manual check

Run a normal session from a terminal, with `RUST_LOG=debug`. Note `:N` in `Allocated display :N`.

1. `xprop -root _NET_SUPPORTING_WM_CHECK` prints a window id. `xprop -id <that id> _NET_WM_NAME` prints `tema`. `xprop -root _NET_SUPPORTED` lists the eleven atoms from the claim, in that order. `xprop WM_STATE` on an xterm prints `window state: Normal` and icon window `0x0`.
2. `kill -9 $(pgrep -x Xwayland)`. A new `xterm` starts Xwayland again on the same `:N`, logs `XWM ready on :N`, and shows a window. A signal within ten seconds of becoming ready still counts as an early failure, as section 17 describes.

The managed-window checks are in section 21.

## 21. Managed X windows

A normal X window becomes a tema window. An override-redirect window does not. The code is `src/xwayland/xwm/`, on top of the lifetime in section 20 and the `WindowId` in section 18.

### The three states

`sync_presence` chooses one state. Not paired, or not mapped, stays hidden. Override-redirect, once paired and mapped, is unmanaged: the root is placed at the X geometry and `xwayland_shown` is set. A normal window stays hidden until its first property batch has arrived, and then becomes managed.

Managed means a `WindowId`, an `XToplevel`, and `xwayland_shown`. The layout owns the root position. Unmap runs `sync_presence`, which leaves managed and destroys the window. `DestroyNotify` and stopping the WM release the window directly. The next map allocates a new `WindowId`. A re-pair keeps `props_loaded`, so the window does not wait for another batch.

### `XToplevel`

`enter_managed` builds an `XToplevel` and inserts it with `insert_object`. It is not in any client's id map. `wl_surface` is a poor host: a commit grabs it, and `State::toplevel` panics when the role object is grabbed. `xwayland_surface_v1` can be destroyed while the X window lives. The WM reads the object with `try_get_mut` and `downcast_mut`. It never grabs it.

The title is `props.title`, or `""`. The app id is `props.class`, or `""`. Maximized means both maximized flags. Fullscreen requested is `props.fullscreen`. Minimum and maximum sizes come from `WM_NORMAL_HINTS`: flags at index 0, minimum at 5 and 6, maximum at 7 and 8. `P_MIN_SIZE` is bit 4 and `P_MAX_SIZE` is bit 5. A pair is used only when its flag is set, the vector is long enough, and both values are positive. Decorations are always `ServerSide`. Motif hints are stored and ignored. `set_requested_fullscreen` does nothing, so a compositor fullscreen toggle is never written back to X. `id` returns `WaylandId` 0. `on` accepts every request.

`request_close` queues `WmEvent::Close`. `interactive_resize` queues `WmEvent::Resize` and ignores the resizing flag. `position_changed` queues `XwaylandAction::Reconfigure`.

### The property gate

Pairing fetches the properties in section 20. Each send increments `props_outstanding`. Each reply decrements it, including an error. A reply for a destroyed window is dropped and does not decrement. A reply that arrives when the count is already 0 does not decrement. The first time the count reaches 0, `props_loaded` becomes true and stays true, and `sync_presence` runs. A later `PropertyNotify` refreshes the `XToplevel`. A new minimum size on a tiled or fullscreen window marks the workspace dirty and dispatches `WorkspaceStateChanged`.

### The layout gate

`is_managed_root_surface_candidate_common` accepts an X role as well as an xdg surface. The window is live once that root has a committed buffer and a toplevel, the same check as xdg.

On a `wl_surface` commit, a mapped change of an X root that already has a window confirms the map, relayouts inside `with_ungrabbed`, and kicks the animation pump when that opens surfaces. Map and unmap both mark the workspace dirty and dispatch `WorkspaceStateChanged`. Map also dispatches `FocusNewToplevel`. Section 23 is the taskbar announcement on that commit. `enter_managed` focuses the root when it is already mapped and the same policy allows it. One map takes one of those two paths. Entering or leaving managed does the same dirty, relayout, pump, and dispatch when the window has a workspace. Section 22 is the focus that follows.

### `ConfigureWindow`

`reconfigure_mapped_toplevels` ends by dispatching `XwaylandAction::Reconfigure`. For each managed window whose root has a committed buffer and protocol visibility:

- Tiled or fullscreen uses `content_rect_global`. Width and height are at least 1.
- Floating uses the root position. The size is the floating restore's configure size, then `floating_size`, then the X geometry. Width and height are at least 1.

A hidden workspace has no protocol visibility, so nothing is sent. When the target differs from the stored `configured` rect, one `ConfigureWindow` carries x, y, width, height, and `border_width` 0. Those values are X pixels. Section 25 is the conversion from the logical layout. There is no sibling, no stack mode, and no synthetic `ConfigureNotify`. `ConfigureRequest` is ignored once the window is shown or managed.

### Placement

The decision is made once, in `enter_managed`. The window floats when it is modal, its type contains dialog, utility, toolbar, or splash, its minimum and maximum sizes are set and equal, or `WM_TRANSIENT_FOR` is set. Otherwise it tiles. A later property change does not move it between tiled and floating.

A floating window is centered on its `WM_TRANSIENT_FOR` parent when that parent is managed, mapped, and on the same workspace. Otherwise it is centered on the output's usable bounds, or the output bounds when those are missing. The frame is the X content size plus the decoration insets. A side larger than the anchor shrinks to that side, and the origin stays at the anchor's origin. The result is clamped into the bounds. The root position is the frame origin plus the left and top insets. When the placed content size differs from the X geometry, that size is stored as `floating_size`.

`_NET_WM_STATE_FULLSCREEN` at map makes a tiled window fullscreen. It does not make a floating window fullscreen.

### Close

`WmEvent::Close` on an unknown window sends nothing. When `WM_PROTOCOLS` contains `WM_DELETE_WINDOW`, the WM sends `SendEvent` with propagate 0, destination the X window, and event mask 0. The event is a `ClientMessage` with the send-event bit set, format 32, type `WM_PROTOCOLS`, and data `[WM_DELETE_WINDOW, 0, 0, 0, 0]`. The second word is `CurrentTime`. Otherwise it sends `KillClient` with the X window id. The Close keybinding reaches this through `request_window_close` once the root has keyboard focus.

### `_NET_WM_STATE` and `WM_CHANGE_STATE`

A `ClientMessage` whose format is not 32, or whose window is unknown, is ignored. The handled types are `WL_SURFACE_SERIAL`, `_NET_WM_STATE`, `WM_CHANGE_STATE`, and `_NET_ACTIVE_WINDOW`. `_NET_WM_MOVERESIZE` is advertised and ignored. Only a compositor grab moves or resizes an X window. Section 22 handles `_NET_ACTIVE_WINDOW`.

`_NET_WM_STATE` data `[0]` is 0 remove, 1 add, or 2 toggle. Any other action is ignored. Data `[1]` and `[2]` are atoms. Only modal, fullscreen, maximized vertically, maximized horizontally, and hidden are honored. When nothing changes, the WM stops. A fullscreen change on a managed window calls `set_window_fullscreen` and dispatches `WorkspaceStateChanged`. Hidden going from false to true goes through `request_iconify`. The `XToplevel` is then refreshed and `_NET_WM_STATE` is written from the flags. Modal and maximize are stored only.

`WM_CHANGE_STATE` data `[0]` of 3 is Iconic and calls `request_iconify`. Data `[0]` of 1 is Normal: it clears hidden, writes `WM_STATE` Normal, and rewrites `_NET_WM_STATE`. Any other value is ignored.

`request_iconify` refuses when the seat's keyboard focus is that window's root. It writes `WM_STATE` Normal and leaves hidden false. Otherwise it writes `WM_STATE` Iconic, `[3, 0]`, and sets hidden. Either way it rewrites `_NET_WM_STATE`. The window stays mapped and in the layout.

Every `_NET_WM_STATE` write lists the client flags. When `Wm.focus` is that window, `_NET_WM_STATE_FOCUSED` is appended.

### Stacking and the client lists

`stack` is bottom to top. `map_order` is map order. Both hold normal windows only. `MapRequest` appends a missing window to `map_order`, moves it to the top of `stack`, and writes `_NET_CLIENT_LIST`. `MapNotify` of a window in `stack` restacks it and writes `_NET_CLIENT_LIST_STACKING`. A window with one below it is `ConfigureWindow` above that sibling. With none below and one above, it is below that sibling. Alone, the stack mode is Above. `UnmapNotify` removes the window from `map_order` and writes `_NET_CLIENT_LIST`. `DestroyNotify` removes it from both lists and writes each list that changed. Both properties are on the root, type `WINDOW`, format 32, mode Replace.

`move_to_top` moves a normal window to the top of `stack` and sends nothing by itself. Section 22 calls it when a managed window is activated, and the following restack sends the `ConfigureWindow`.

### Override-redirect order

Unmanaged roots are drawn in the content tier after workspace roots, so they sit above managed content and below the layer-shell top and overlay layers. Each time a window enters unmanaged, the WM stamps `xwayland_shown_order` with the next counter value. `collect_unmanaged_scene_entries` keeps non-X roots in their current order, then appends the X roots sorted by that stamp. A later window is higher. Pick walks the scene from the top, so the later menu is hit first.

### Grabs

`WmEvent::Resize` stores `floating_size`, each side at least 1, and reconfigures that window from its current root position. A left or top edge resize has already moved the root, so one `ConfigureWindow` carries the new origin and the new size.

`position_changed` runs after `translate_root_surface` in `anchored_grab_target`, and after `set_root_surface_position` in the tiled-drag pickup and in `place_grabbed_root`. For an X window that dispatches `Reconfigure`, which sends the new x and y. A tiled resize dispatches `WorkspaceStateChanged`. The following `Reconfigure` configures both neighbors.

### Manual check

Run a normal session from a terminal, with `RUST_LOG=debug`. Note `:N` in `Allocated display :N`. The claim and the restart check are in section 20.

1. `xterm` tiles next to a Wayland window.
2. A second `xterm` splits the workspace.
3. `zenity --info` with `GDK_BACKEND=x11` floats, centered over its parent or on the output.
4. The Close keybinding closes `xterm` with `WM_DELETE_WINDOW`. `xeyes` falls back to `KillClient`.
5. `wmctrl -r :ACTIVE: -b toggle,fullscreen`, or F11 in a GTK app, makes the window fullscreen.
6. `xprop -root _NET_CLIENT_LIST _NET_CLIENT_LIST_STACKING` lists the windows.
7. An `xterm` Ctrl+click menu shows above the window, at the pointer.
8. After a floating move and resize, a menu opens at the new position.

## 22. X focus

Keyboard focus on a managed X window is X input focus. The code is `src/xwayland/xwm/focus.rs`. One `Wm.focus` follows the seat. Timestamps are `CurrentTime`, which is 0.

### The trigger

`set_keyboard_focus_with_snapshot` stores the new focus and then dispatches `XwaylandAction::KeyboardFocus`. An unchanged focus dispatches nothing. The WM reads the seat when it handles the event, so an action that has been overtaken describes the focus that exists now.

### The target

No keyboard focus clears X focus. A root that has no X role clears it too. Clearing writes `_NET_ACTIVE_WINDOW` as `[0]`, type `WINDOW`, and sends `SetInputFocus` to `Window(0)` with revert `PointerRoot`. An X root that is unmanaged, including an override-redirect window, leaves `Wm.focus` where it is. A managed X root that is not override-redirect is activated.

### The input model

`input_hint` is the ICCCM input flag. `WM_TAKE_FOCUS` in `WM_PROTOCOLS` is the take-focus protocol.

- Neither is set: the WM sends no focus request.
- Input, and no take-focus: `SetInputFocus`.
- Both: `SetInputFocus`, then `WM_TAKE_FOCUS`.
- Take-focus only: `WM_TAKE_FOCUS`.

`SetInputFocus` uses revert `PointerRoot`, the window, and time 0. Its sequence is stored as `last_input_sequence`. `WM_TAKE_FOCUS` is the same `SendEvent` shape as `WM_DELETE_WINDOW` in section 21, with `WM_TAKE_FOCUS` in the first data word.

### Properties and the raise

Activation writes `_NET_ACTIVE_WINDOW` on the root, moves `Wm.focus`, rewrites `_NET_WM_STATE` on the previous window, sends the input-model requests, rewrites `_NET_WM_STATE` on the new window, calls `move_to_top`, and restacks. `_NET_WM_STATE_FOCUSED` is present on the window `Wm.focus` names. An iconic window returns to `WM_STATE` Normal, `[1, 0]`, before the active-window write. The same target as the current `Wm.focus` sends nothing.

`DestroyNotify` of the focused window clears `Wm.focus`, writes each client list that changed, then writes `_NET_ACTIVE_WINDOW` as `[0]` and sends `SetInputFocus` to `None`. Unmap leaves `Wm.focus` until the seat focus moves.

### Map and the successor

A newly mapped managed X window takes keyboard focus when `should_focus_new_toplevel_on_map` allows it, the same policy as an xdg toplevel. The commit path dispatches `FocusNewToplevel` when the buffer maps a window that already exists. `enter_managed` calls `set_keyboard_focus` when `window_is_mapped` is already true. One map uses one of those paths.

`leave_managed`, `DestroyNotify`, and stopping the WM take `focused_window_close` before `destroy_window`, then call `refocus_after_focused_window_unmap`. Hiding an override-redirect root that holds keyboard focus clears it and calls `focus_after_client_disconnect`. Unlock restores a managed X window. An override-redirect root is left unrestored.

### Clicks

`Toplevel::accepts_keyboard_focus` is true for an xdg window. `XToplevel` stores false when neither `input_hint` nor `WM_TAKE_FOCUS` is set, or when `window_types` contains combo, dnd, dropdown menu, menu, notification, popup menu, splash, tooltip, or utility. A press on that window keeps the current keyboard focus. A press on an unmanaged X root keeps the current focus when that focus root already has the X role, and focuses the menu when the current focus is elsewhere. The button event still goes to the hit surface. A decoration grab still focuses its window. Map-time focus still applies to a window that refuses a click.

### The client PID

Pairing sends `ResQueryClientIds` when startup found the X-Resource extension. The spec is `{ client: window id, mask: LocalClientPID }`. The first value with that mask and a non-empty `value` becomes `pid`. An error, or a reply with no such value, stores `None`. The query does not delay management. A missing PID refuses every `FocusIn` handoff and every client activation.

### `FocusIn`

`FocusOut` is ignored. `FocusIn` with mode `Grab` or `Ungrab`, or detail `Pointer`, is ignored. Any other `FocusIn` is accepted when all of these hold:

- The window is known and paired.
- `Wm.focus` is a known window.
- Both PIDs are `Some` and equal.
- `widen_event_sequence` of the event is at least `last_input_sequence`.
- The window is visible. A managed window's root has protocol visibility. An unmanaged window is `shown`.

Accepting sets `Wm.focus` and rewrites `_NET_WM_STATE` on the previous window when it is managed, and on the new window when it is managed. It does not write `_NET_ACTIVE_WINDOW`. A managed window is raised with `raise_root_surface` and given keyboard focus. The `KeyboardFocus` that follows finds the same target and sends nothing. An override-redirect accept records `Wm.focus` and leaves the seat focus in place. A rejected event calls `send_focus` for the current `Wm.focus`. That resend is the focus requests alone. With `Wm.focus` empty, it is `SetInputFocus` to `None`.

### Client activation

A `ClientMessage` of type `_NET_ACTIVE_WINDOW` ignores its five data words. It acts when the target is managed, `Wm.focus` has a known PID, the target PID equals it, and the target root has protocol visibility. It raises that root and calls `set_keyboard_focus`. The following `KeyboardFocus` sends the activation and the restack. Any other request changes nothing. There is no workspace switch and no attention state. An override-redirect target is one of the requests that change nothing.

### Manual check

Run a normal session from a terminal, with `RUST_LOG=debug`. Note `:N` in `Allocated display :N`. The claim is in section 20.

1. With two `xterm`s, typing goes to the focused one. `FocusNext` moves typing. `xprop -root _NET_ACTIVE_WINDOW` names the focused one.
2. Clicking a Wayland window sets `_NET_ACTIVE_WINDOW` to `0x0`, and typing goes there.
3. A new `xterm` takes focus when it maps.
4. A GTK menu (`GDK_BACKEND=x11`) keeps the app frame active and works from the keyboard.
5. A GTK dialog takes focus. Closing it returns focus to the parent.
6. `xprop _NET_WM_STATE` on the focused window lists `_NET_WM_STATE_FOCUSED`.
7. In a GTK app with two windows, `wmctrl -i -a <other id>` focuses and raises it. From an `xterm`, `wmctrl -a` on another app does nothing.
8. Lock and unlock the session: focus returns to the X window.

## 23. X windows in taskbars and capture

A managed X window is listed like an xdg toplevel. An override-redirect window has no `WindowId`, so menus and tooltips stay off both foreign-toplevel protocols. The calls live in `src/xwayland/xwm/window.rs` and the X-role commit in `src/wayland_objects/wl_surface.rs`. The records live in `src/capture.rs`.

### Announced and closed

`enter_managed` calls `capture::map_window` when the root already has a committed buffer. The X-role commit calls it when the window is already mapped and the buffer arrives later. `map_window` is idempotent. Title and app id come from the `Toplevel` trait. The next map after a close allocates a new `WindowId` and a new toplevel key.

`leave_managed` calls `capture::unmap_window` before `destroy_window`. `DestroyNotify` and stopping the WM do the same, through `forget` and `release_managed`. A commit that clears the buffer calls it too. `unmap_window` closes the record, drops pending updates for that key, fails queued capture jobs, stops open image-copy sessions, and fails open frames.

### Title, class, and parent

`refresh_managed` copies the title, the app id, and the parent out of the `XToplevel`, then calls `retitle_window`, `reapp_window`, and `note_window_parent`. The title is the UTF-8 name from section 20. The app id is the `WM_CLASS` class, the second NUL-separated field, on both ext-foreign-toplevel-list and zwlr-foreign-toplevel-management. An unchanged value queues nothing. A later `PropertyNotify` resends the new value.

The parent is the managed window named by `WM_TRANSIENT_FOR`. zwlr version 3 sends it. `enter_managed` and `leave_managed` finish by calling `refresh_transient_children`, so a child gains that parent when the named window becomes managed and loses it when that window leaves. `forget` and `release_managed` leave children for their own next refresh.

### The zwlr state

`sync_layout_shell_state` stores maximized clear and fullscreen from `window_is_fullscreen`, which reads the layout. `enter_managed` and the X-role commit call it after `map_window`. `Wm::reconfigure` calls it again for every managed window, so a fullscreen the layout applies during that pass reaches the record. The `_NET_WM_STATE` maximized flags stay on the `XToplevel`. The handle's maximized bit stays clear.

`set_keyboard_focus` calls `sync_activated_from_keyboard_focus` on both the unchanged-focus path and the change path. The activated record is the managed window whose root holds keyboard focus. Focus on a Wayland surface, or on an override-redirect menu, leaves no X record activated.

Output membership follows the root. `refresh_surface_outputs` calls `sync_toplevel_outputs` for the window of that root.

### Taskbar requests

The zwlr handle in `src/wayland_objects/zwlr_foreign_toplevel_handle_v1.rs` is shell-neutral. Activate shows the window's workspace, raises the root, and focuses it. The following `KeyboardFocus` is section 22, so an iconic window returns to `WM_STATE` Normal. Close calls `request_window_close`, which is section 21. `set_fullscreen` and `unset_fullscreen` change the layout on the current workspace. `XToplevel::set_requested_fullscreen` does nothing, so the WM writes no `_NET_WM_STATE` for that request. The next `reconfigure` copies the layout bit onto the handle. `set_maximized`, `set_minimized`, and `set_rectangle` are checked and discarded. A request on a closed handle does nothing.

### Image capture

`toplevel_rendered_bounds` unions the root's scene bounds, its popup tree, and the server-side decoration influence. `submit_toplevel_capture` queues a job of that size. An override-redirect menu is its own root, so the copy leaves it out. Closing the toplevel fails a queued job, stops an open session, and fails an open frame. A new image-capture source on the closed handle disconnects the client.

### Manual check

Run a normal session from a terminal. The display number is in section 20.

1. `waybar` with `wlr/taskbar`, or `lswt`, lists an `xterm` with its title and class `XTerm`.
2. `printf '\033]2;renamed\007'` in that `xterm` updates the title.
3. Clicking the taskbar entry focuses the `xterm`, and typing reaches it.
4. Closing from the taskbar closes it.
5. A GTK menu (`GDK_BACKEND=x11`) adds no entry.
6. `grim` through a toplevel picker, or an xdg-desktop-portal window share, captures the `xterm` with its decorations.

## 24. Clipboard and primary selection

`CLIPBOARD` and `PRIMARY` move both ways between X clients and Wayland clients. The clipboard is `wl_data_device`. The primary selection is `zwp_primary_selection_v1`. Data-control devices receive the same offers. The windows and the owner tracking live in `src/xwayland/xwm/selection.rs`. The bytes live in `src/xwayland/xwm/transfer.rs`. MIME names live in `src/xwayland/xwm/mime.rs`.

### The two windows

The claim in section 20 creates one `InputOnly` 10×10 window for `CLIPBOARD` and one for `PRIMARY`. Each has `PropertyChange`. `XFixesSelectSelectionInput` on each window selects `SetSelectionOwner`, `SelectionWindowDestroy`, and `SelectionClientClose`. `CreateNotify` for either window is ignored.

### The two echo breaks

An `XFixesSelectionNotify` whose owner is that selection's own window stores `own_timestamp` from `selection_timestamp`. A foreign owner, including owner `None`, drops the Wayland source this bridge had published for that selection. A real foreign window then receives `ConvertSelection` of `TARGETS`. The property is `_WL_SELECTION` on that selection's window, and the time is the notify's `timestamp`.

A Wayland clipboard or primary change whose seat slot already holds the source this bridge published leaves the X owner in place. `become_ready` runs both changes after the claim, so a selection that is already on the seat is owned on X for the new run.

### Focus and ownership

Every Wayland clipboard or primary change takes the matching X selection with `SetSelectionOwner`, owner the selection window and time 0, including while a Wayland window is focused. An empty Wayland selection, while this WM owns that X selection, sends `SetSelectionOwner` with owner `None`. `SelectionClear` for our window clears `owned` and the outgoing offer.

An X owner is published once its `TARGETS` reply has become a non-empty MIME list, the keyboard-focus root has the Xwayland role, and the run's Xwayland client has connected. The root of the focused surface is what is checked, so an override-redirect menu counts. `install_x_selection` offers the result to the focused data device and to data-control devices. The source is an `XSelectionSourceObject` kept outside any client map.

A `SelectionRequest` while the focus root is a Wayland surface is refused. A `SelectionRequest` for any other selection, including `XdndSelection`, is logged and left unanswered.

### MIME types

From X to Wayland, `UTF8_STRING` is `text/plain;charset=utf-8`, `STRING` is `text/plain`, and any other atom whose name contains `/` is that name. Other atoms are skipped. Duplicates are dropped and the order is kept.

From Wayland to X, `text/plain;charset=utf-8` is `UTF8_STRING` and `TEXT`, `text/plain` is `STRING`, and any other MIME type is an interned atom of that name. `TEXT` and `UTF8_STRING` both carry the UTF-8 bytes. A `SelectionRequest` that arrives while one of those atoms is still being interned waits, and is answered when the last intern for that offer completes.

### `TARGETS`, `TIMESTAMP`, and refusal

A request whose property is `None` is answered on the target atom. `TARGETS` is a `ChangeProperty` of type `ATOM` and format 32: `TARGETS`, `TIMESTAMP`, then the offered atoms. `TIMESTAMP` is type `INTEGER`, format 32, and the single word `own_timestamp`. Each is followed by `SelectionNotify` naming that property.

`MULTIPLE`, a target that was not offered, and a missing offer are refused, and so is a request without X focus. The refusal is `SendEvent` of `SelectionNotify` with property `None`, propagate 0, destination the requestor, and an empty event mask.

### Transfers

One X-to-Wayland conversion runs per selection. Further `receive` calls wait in a FIFO, and the front entry is the one in flight. The data `ConvertSelection` uses time 0 and the same `_WL_SELECTION` property. The bytes are written to the pipe the Wayland client passed. A property of type `INCR` switches that transfer to incremental mode. The deleting `GetProperty` has already told the owner to start, and the header itself is left out of the pipe. Each later `NewValue` is fetched with another deleting `GetProperty` after the previous chunk's pipe write has completed. An empty value ends the transfer.

Wayland to X reads the source through one `pipe2` pipe. The write end is handed to the source once. The read is `ReadSome` of 65536 bytes, and a zero-length read is end of file. A value that fits in one chunk is one `ChangeProperty` of format 8 and the target type, then `SelectionNotify`.

The chunk is `min(65536, maximum_request_length * 4 - 24)` bytes. When the buffer reaches that size before end of file, a requestor that is already tracked already has `PropertyChange`, and any other requestor receives `ChangeWindowAttributes` with that mask. The WM then writes an `INCR` property of format 32 holding the chunk size, sends `SelectionNotify`, and waits. Each `Delete` on that property writes the next chunk. The next pipe read is issued when the buffer holds less than a chunk and end of file has not arrived. The `Delete` after end of file and the last chunk writes a zero-length property and ends the transfer. `DestroyNotify` of the requestor drops its transfers.

### Idle timeout and teardown

`src/xwayland/xwm/timer.rs` keeps one timerfd. Every transfer step, in both directions and including an `INCR` wait, sets a 5 second deadline. A transfer runs to any size, and a step that stays idle expires. An incoming timeout drops that receive and starts the next queued one. An outgoing timeout before `SelectionNotify` refuses the request. After `SelectionNotify`, it cancels the read and drops the transfer.

`Wm::stop` cancels every pipe read and write, drops those fds, removes the `XSelectionSourceObject`s, and clears a clipboard or primary slot that still holds one. That clear publishes `selection(null)` to the focused data device. The transfer timer then stops, and `release_managed` from section 20 runs. The next run starts with empty selection windows and takes ownership of the Wayland selection that is on the seat.

### Manual check

Run a normal session from a terminal. The display number is in section 20.

1. `echo hi | xclip -selection clipboard` with an X window focused, then `wl-paste` prints `hi`.
2. `wl-copy hello`, then `xclip -o -selection clipboard` from a focused `xterm` prints `hello`.
3. Selecting text in an `xterm` and middle-clicking in a GTK Wayland app pastes it. The reverse works too.
4. `head -c 5M /dev/urandom | wl-copy`, then `xclip -o -selection clipboard | wc -c` prints 5242880. The reverse goes through `xclip -i` and `wl-paste | wc -c`.
5. With focus on a Wayland window, `xclip -o` from a background X client returns nothing.
6. `wl-copy --clear` makes `xclip -o -selection clipboard` fail.

## 25. Xwayland scaling

`[xwayland] scaling_mode` is `default` or `downscaled`. `default` keeps X coordinates equal to tema's logical coordinates, and X apps are upscaled by the compositor on a HiDPI output. `downscaled` gives the Xwayland client one integer wire scale S. Everything tema sends to that client is multiplied by S, and everything it receives is divided by S. X apps render at S times their logical size, and tema reads their buffers as buffer scale S. On an output whose scale is lower than S, those buffers are downscaled. The helpers live in `src/wire_scale.rs`. A normal Wayland client stays at scale 1.

Rootless Xwayland builds its RandR CRTCs and the root window from `zxdg_output_v1.logical_position` and `logical_size`, read as X pixels. It ignores `wl_output.mode` once xdg_output is bound, and it ignores `wl_output.scale`. Multiplying the xdg_output position and size by S is what makes the X screen S times the logical layout, with the same gaps and the same output order. `wl_output` geometry x and y are multiplied the same way. Its `scale` event is 1 while S is greater than 1, and the legacy scale otherwise. Its `mode` stays the physical mode. `preferred_buffer_scale` is 1 and `wp_fractional_scale_v1.preferred_scale` is 120, which is scale 1 in that protocol's units of 1/120.

### Choosing S

S is the largest `ceil(scale)` among enabled outputs, and at least 1. Scales 1.25 and 1.5 both give 2. A disabled output is left out. `default`, downscaled with no enabled outputs, and downscaled on all-1x outputs all give 1. With S equal to 1 every conversion is the identity. `State::refresh_xwayland_wire_scale` stores the value and logs `Xwayland wire scale is now N` when it changes. The Xwayland client receives that value at adopt, before its first `wl_output` bind.

### Rounding

Logical values become wire values by exact multiplication. `i32` multiplication saturates. The way back is in `src/wire_scale.rs`:

| Conversion | Rule | Used for |
|---|---|---|
| wire position to logical | floor division (`div_euclid`) | override-redirect position, attach offset, subsurface position, hotspot |
| wire size to logical | ceiling division | surface size, viewport destination, X window width and height, min size hint |
| wire max size hint to logical | floor division, then at least the logical min already converted | `WM_NORMAL_HINTS` max, one axis at a time |
| wire rect outward | floor x1/y1, ceil x2/y2 | surface damage, input region, X damage |
| wire rect inward | ceil x1/y1, floor x2/y2, drop when empty | opaque region |
| wire `f64` to logical | exact division | viewport source, locked-pointer hint |

An X damage rect `(100, 51, 300, 200)` at S = 2 is logical `(50, 25, 150, 101)`, because the far edge of the odd y coordinate rounds outward.

### Commit normalization

The pending commit is normalized once, in `wl_surface.rs`, when the committing client's wire scale is greater than 1. The rest of the compositor then sees logical units. The effective buffer scale is the client's buffer scale times S, so a 200×100 X buffer becomes a 100×50 logical surface. Viewport source fields are divided exactly and the destination uses ceiling division. Surface damage and the input region convert outward. The opaque region converts inward. The attach offset and `wl_subsurface.set_position` use floor division. Buffer damage is left alone, because it already converts through the effective buffer scale.

The raw client values and the scale used are stored as `WireInputs` on the on-screen state. A commit at scale 1 stores none. A later scale change rebuilds the logical size, regions, and viewport from those raw values, or from the on-screen state when the commit happened at scale 1.

### Input

Pointer enter, motion, and warp, the continuous axis value, and both relative-motion pairs are multiplied by S on the way to the client. `axis_discrete` and `axis_value120` stay in protocol steps. A cursor hotspot, a locked-pointer hint, and a lock or confine region are divided on the way back. `wl_touch` is not advertised.

### The X window manager boundary

`XWindow.geometry` and `XWindow.pending` stay in X pixels. Conversion happens where a value meets the compositor or where a compositor value is sent back. `place` uses floor division. X damage uses the outward rect. A floating window's centered size and `floating_size` are logical. Size hints are stored both raw and logical. The raw min and max decide "fixed size, so float". The logical min is the ceiling division, and the logical max is the floor division clamped up to that min.

`ConfigureWindow` sends X pixels. Tiled and fullscreen content uses `rect_to_wire`. A floating window multiplies the logical root position and the logical size. `configured` is the last rect sent, in X pixels, so a new S changes the target and the window is configured again. The 1×1 and `pending` configures on map request send `pending` unchanged. `ConfigureRequest` after map stays ignored.

### A live change of S

S is recomputed when an output scale changes, when an output is removed, when a `wl_output` global is added, and when a reload changes `scaling_mode`. The Xwayland client's `SetWireScale` is queued before that client's output events. The handler stores the scale, resends every bound `wl_output` and `xdg_output`, resends preferred buffer and fractional scale, recomputes each on-screen surface, and dispatches `Reconfigure` and `RefreshHints`.

`state.xwayland_wire_scale` moves before Xwayland has processed the new output events. For that short window the XWM may convert a stale X geometry with the new S. `Wm::reconfigure` is what converges it: it refreshes logical size hints, sends `ConfigureWindow` for managed windows, and places every shown override-redirect window again from its unchanged X geometry. Xwayland is left running.

### `RESOURCE_MANAGER` and the root cursor

In `downscaled` mode the WM reads the root `RESOURCE_MANAGER` property (`STRING`, format 8) and merges `Xft.dpi: 96*S` and `Xcursor.size: cursor_size*S`. Other lines, comments, and continuation lines stay. Missing keys are appended. `default` mode leaves the property alone until a previous downscaled write has to be put back, and then merges `Xft.dpi: 96` and `Xcursor.size: cursor_size` once. The code is `src/xwayland/xwm/hints.rs`.

The root cursor is the theme's `default` image, or `left_ptr`, at `cursor_size * S` pixels. The seat's own cursor stays at `cursor_size`. A change of S, of `cursor_size`, or of `cursor_theme` runs the upload again. `RefreshHints` is that path, from `SetWireScale` and from `apply_general_settings`.

### Known limits

An X window on an output whose scale is lower than S is downscaled, which costs a scale and is slightly soft. A fractional output renders at the next integer. GTK needs `GDK_SCALE`. Qt needs `QT_SCALE_FACTOR`, or it can follow `Xft.dpi`. `wl_output.mode` stays the physical mode for the Xwayland client too. Drag-and-drop coordinates are M10's job.

### Manual check

Run tema on a 2× output, or set `[output.scales]` for that output's name to 2. The display number is in section 20. `cursor_size` 24 is the example below.

1. `scaling_mode = "default"`. `xterm` and `xeyes` look as they do today, upscaled. `xdpyinfo | grep dimensions` shows the logical size.
2. `scaling_mode = "downscaled"`. `xdpyinfo` shows twice the logical size. `xrandr` lists outputs at doubled positions. `xterm` text is sharp and small. `xrdb -query` shows `Xft.dpi: 192` and `Xcursor.size: 48`. `GDK_SCALE=2 GDK_BACKEND=x11 gtk3-demo` looks native size and sharp.
3. `xev` reports coordinates that match the pointer. Clicks hit buttons in a tiled window and in a floating window. Menus open next to the click.
4. Tiling, floating, fullscreen, and an interactive resize give X sizes of twice the logical size (`xwininfo`).
5. With a 1× output and a 2× output, X windows on the 1× output are downscaled. The X root spans both. Moving a window between outputs keeps it correct.
6. Change that output's scale from 2 to 1 and reload. X windows reconfigure to 1× without restarting Xwayland. `xrdb -query` shows `Xft.dpi: 96` and `Xcursor.size: 24`.
7. Switch `scaling_mode` on reload both ways while `xterm` is open. It reconfigures, and Xwayland stays running.
8. `scaling_mode = "downscaled"` on all-1× outputs behaves as `default` does.

## 26. IPC

Clients that need compositor state Wayland does not expose use a Unix socket. The code is `src/ipc/`. `proto.rs` is the newline-delimited JSON protocol, version 1. `temactl` compiles that file by path and does not link the compositor. `snapshot.rs` copies `&State` into the protocol objects. It does no I/O and does not use the dispatcher.

`IpcServer` is one runner model. It binds `$XDG_RUNTIME_DIR/tema-0.ipc.sock`, or `/tmp/tema-0.ipc.sock` when `XDG_RUNTIME_DIR` is unset, removes a stale file at that path, and sets the mode to `0600`. It ignores `TEMA_SOCK`. That variable is how a client finds the socket. `session_variables` in `src/process/mod.rs` puts the bound path into the forker environment and into the activation environment from section 16. A bind failure is logged and startup continues with `state.ipc_enabled` false. `Stop` unlinks the socket.

Each accepted connection is an `IpcConnection` model of its own. `version`, `outputs`, `workspaces`, and `focused_window` are answered from a fresh `snapshot::build`. `subscribe` stores the requested topics. An empty list means all of them. The connection writes `{"ok":"subscribed"}` and then one event per topic from that snapshot. A later request on the same connection gets `{"error":"connection is subscribed"}`. The connection also sends `IpcServerAction::Subscribed`, and the server keeps the topic set. An empty stored set means every topic.

`ipc::mark_dirty` sets `state.ipc_dirty` and queues one `IpcServerAction::Refresh`. Another call in the same turn does nothing until `Refresh` clears the flag. When `ipc_enabled` is false the call returns without dispatching. The runner would otherwise log that the action has no model. `Refresh` builds a new snapshot and compares it with the one stored on the server. The comparison order is the focused window, then workspaces, then outputs. Each topic that differs is encoded once and sent to every subscriber of that topic. With no subscribers the snapshot is stored and nothing is encoded. Pointer motion is not a hook. `State::active_output_id` follows the pointer and is not part of the snapshot.

The calls are in `ClientManager`, on `WorkspaceStateChanged`, `OutputStateChanged`, and `OutputRemoved`, and in `src/capture.rs`: `announce_toplevel`, `close_toplevel`, `note_window_state`, `apply_activated` when the activated set changes, and `update_window_detail` after the title or the app id changes. Floating and fullscreen keybindings, drags, and workspace publication arrive as `WorkspaceStateChanged`.

`Window.tag`, `Window.description`, and `Window.icon` come from the xdg toplevel. An empty tag or description is stored as absent. `icon` is a name plus the copied sizes, and is absent when the toplevel has none. `note_window_metadata` marks the snapshot dirty after a tag, description, or icon change.

`Window.modal` is true when the focused window is an xdg dialog with `set_modal` and a parent that still exists. `xdg_dialog_v1` requests and every `xdg_toplevel.destroy` mark the snapshot dirty.

`bell` is a topic with no snapshot field. `ipc::bell` sends `IpcServerAction::Bell` at once. The event is `{"event":"bell","window":N}`, or `"window":null` when the ring names no window. Subscribers of `bell`, and subscribers whose topic set is empty, receive it. The initial subscribe dump does not include one, because the snapshot has no bell. The same window, and a ring with no surface, are each limited to one event per 100 ms. `destroy_window` drops that window's throttle entry. There is no sound and no urgency hint.

`window_icon` is a request. `size` defaults to 32 and `scale` to 1. The reply picks the pixmap whose `size * scale` equals the request, else the next larger product, else the largest smaller one, and among those the scale closest to the request. `data` is standard base64 of little-endian premultiplied BGRA. No pixmap leaves `data`, `size`, and `scale` null and still returns the name. An unknown id is `{"error":"no such window"}`. `temactl window-icon` is this request. `--size` and `--scale` select the pixmap, `-o FILE` writes a PNG, and `-j` prints the ok value.

A snapshot topic needs a field on `IpcSnapshot`, filled in by `build`, a `Topic` variant, and an `Event` variant. Call `mark_dirty` where that state changes. `bell` is the exception: it has a `Topic`, an `Event`, and its own server action, and it is not compared. A query also needs a `Request` variant and a `Response` variant. `temactl subscribe` accepts any `Topic` name. `temactl waybar window` follows only the focused window.

### Manual check

Programs tema starts have `TEMA_SOCK` set. `socat` is the wire. `temactl` is `target/release/temactl`.

1. `temactl version` prints the client version, `compositor 0.1.0`, and `protocol 1`.
2. `printf '%s\n' '{"request":"workspaces"}' | socat - UNIX-CONNECT:"$TEMA_SOCK"` prints one `{"ok":{"workspaces":[…]}}` line.
3. `temactl subscribe focused_window` prints the current window, then another line when focus or the title changes, and nothing while the pointer only moves.
4. Waybar `custom/window` as in the README shows the focused title. Stopping tema clears it. Starting tema again fills it back in without restarting Waybar.
5. `temactl subscribe bell` prints nothing until a client rings `xdg_system_bell_v1`. The line is `{"event":"bell","window":null}` or `{"event":"bell","window":N}`. A second ring for that same window within 100 ms prints nothing.
6. `temactl focused-window` shows `tag`, `description`, and `icon` when the focused window set them. `temactl window-icon ID` prints the name and the chosen size. `temactl window-icon ID -o /tmp/icon.png` writes a PNG.

## 27. xdg staging protocols

Six version-1 globals from wayland-protocols staging. The interfaces are generated from the host XML and are not vendored. Bell, tag, icon, toplevel drag, and dialog are registered in `src/globals.rs`. The session manager is added and removed on its own, as described below. X windows do not implement any of them. A protocol error uses the spec's own code and disconnects the client.

### `xdg-system-bell-v1`

`xdg_system_bell_v1` is `src/wayland_objects/xdg_system_bell_v1.rs`. `ring` with no surface, or with a surface that is not a window, emits the IPC `bell` from section 26 with a null window. Any other surface uses its root window. The object lives until `destroy` or the client disconnects. `ring` itself has no protocol error. Nothing is played.

### Manual check

1. `temactl subscribe bell`, then a client `ring` with no surface. One `{"event":"bell","window":null}` line appears. A repeat within 100 ms adds nothing.

### `xdg-toplevel-tag-v1`

`xdg_toplevel_tag_manager_v1` is `src/wayland_objects/xdg_toplevel_tag_manager_v1.rs`. `set_toplevel_tag` and `set_toplevel_description` write `Option<String>` on `XdgToplevel` immediately. The spec does not double-buffer them. An empty string is stored as absent. A change updates the IPC window through `note_window_metadata`. The strings die with the toplevel. An empty string is not a protocol error. An unknown toplevel disconnects through the usual object lookup. Nothing matches windows by tag or description.

### Manual check

1. A client sets a tag on the focused window. `temactl focused-window` shows `tag`. An empty tag removes the field.

### `xdg-toplevel-icon-v1`

The manager and the icon object are `src/wayland_objects/xdg_toplevel_icon_manager_v1.rs` and `xdg_toplevel_icon_v1.rs`. Bind sends `icon_size` for 16, 24, 32, 48, 64, and 128, then `done`. `set_name` stores the name. An empty name is absent. Names are not resolved through an icon theme. `add_buffer` copies square `wl_shm` `ARGB8888` or `XRGB8888` pixels at that moment. `XRGB8888` alpha is forced to opaque. At most 16 images are kept, and an edge larger than 256 is ignored. A larger image or any other shm format is ignored and logged at debug. A non-shm buffer, a non-square buffer, a destroyed buffer, a bad stride, or `scale < 1` is `invalid_buffer` and disconnects. A seventeenth new size is ignored. The same size and scale replaces the previous copy. `no_buffer` is never raised, because the pixels are already copied. `set_icon` on the manager stores a pending icon, or clears it when the icon argument is null. The toplevel's next `wl_surface.commit` freezes that icon into an `Rc` and publishes it. After that freeze the icon object is immutable: `set_name` and `add_buffer` are `immutable` and disconnect. `destroy` is still allowed. The published icon stays on the toplevel until a later commit replaces it. The IPC summary and the `window_icon` request are in section 26.

### Manual check

1. A client commits an icon. `temactl focused-window` shows `icon.name` and `icon.sizes`. `temactl window-icon ID --size 32 --scale 1` prints that image. A buffer that is not square disconnects the client.

### `xdg-session-management-v1`

The live sessions are `src/session.rs`. The file and the flush timer are `src/session_store/`. The protocol objects are `src/wayland_objects/xdg_session_manager_v1.rs`, `xdg_session_v1.rs`, and `xdg_toplevel_session_v1.rs`.

The file is `$XDG_STATE_HOME/tema/sessions.json` when that variable is set and not empty, otherwise `$HOME/.local/state/tema/sessions.json`. If neither variable yields a path, `state.session_store` stays unset and the global is not advertised. The store is loaded at startup. A missing file is an empty store. An unreadable file, an unsupported version, or invalid JSON is quarantined, logged, and replaced by an empty store. `[session_management] enabled` defaults to true and only controls whether `xdg_session_manager_v1` is advertised. A reload adds or withdraws that global. Withdrawing it leaves the file in place. Saves are written five seconds after the last change, and once at shutdown.

Each toplevel record stores the output connector name, the workspace name, fullscreen, and floating geometry relative to that output's origin. Tiled geometry is null. The place in the tile tree is not stored. A workspace is identified by the connector name together with the workspace name.

`get_session` with an empty id, or with an id that is not in the store, creates a session and sends `created`. A known id sends `restored`. `invalid_session_id` is not raised. Invalid UTF-8 never reaches this request. Another client that already holds the id receives `replaced`, and its session object becomes inert. The same client asking again is `in_use`. An unknown reason is `invalid_reason`.

`add_toplevel` and `restore_toplevel` register the name on the live session. An empty name is `invalid_name`. A toplevel that already belongs to a session is `already_added`. A name already live in this session is `name_in_use`. A name that exists only in the store is replaced by the next sample. `restore_toplevel` after the initial commit is `already_mapped`. Restore runs on that initial commit, before the first configure. A child that was tiled and not fullscreen is left unrestored. Launch and session-restore move the window only when the saved workspace is still active. Recover uses the saved workspace even when it is not active. Otherwise the active workspace of the saved output is used, when that output is still enabled. tema does not create a workspace for a restore and does not activate one. Floating geometry is placed from the saved output origin. Fullscreen is applied. A tiled window then takes part in the normal tile layout.

A layout change re-samples every live toplevel. `remove` deletes the session from the store and does not sample. Destroy and client disconnect sample first, then drop the live session. X windows are not recorded.

### Manual check

1. With `[session_management] enabled = true`, `wayland-info` lists `xdg_session_manager_v1`. Setting `enabled = false` and saving the config removes it from a newly started client. `$XDG_STATE_HOME/tema/sessions.json` is still there.
2. Quit an app that restores its windows, then start it again. A floating window returns to its output and its place on that output. A tiled window returns to that workspace and is tiled with the others. Its old split is not restored.
3. Two clients restoring the same session id: the first receives `replaced`.

### `xdg-toplevel-drag-v1`

The protocol objects are `src/wayland_objects/xdg_toplevel_drag_manager_v1.rs` and `xdg_toplevel_drag_v1.rs`. The seat state is `src/input/toplevel_drag.rs`. `get_xdg_toplevel_drag` requires a `wl_data_source` that has not been used and that has no drag yet. Anything else is `invalid_source` on the manager. `attach` before `start_drag` is stored on the drag object and applied when the drag starts. `attach` during the drag places the window. A toplevel that already belongs to another drag, or a second mapped toplevel while one is still attached, is `toplevel_attached`. `destroy` of the drag object during the drag is `ongoing_drag`.

A window that was already tiled is picked up immediately as tema's tiled-drag preview. The client's offset is ignored. The window stays on its own workspace, including when the pointer is over another output. A drop inserts it into the tile under the pointer. A button release over no accepting target is the same drop. Cancel, including a session lock, restores the previous split. Unmapping during the drag does not put the window back into the tree.

A window that was already floating follows the pointer at the client's offset. A drop leaves it where it is. Cancel puts it back at the position it had when it was attached. Its workspace does not change.

A window that was not mapped at attach becomes floating and is placed under the pointer when it maps. A drop moves it to the active workspace of the output under the pointer. When the drag's source window was floating, the new window stays floating. Otherwise it is tiled at the pointer, or it becomes the only tile when that workspace has none. Cancel leaves it floating where it is. Destroying it during the drag places nothing.

This path does not take the compositor pointer grab and does not change the cursor. The dragged window is left out of the drop-target pick. Escape does not cancel the drag. A session pause cancels it before button releases are replayed.

### Manual check

1. Drag a tab out of a tiled browser window onto another tile. The new window tiles there. Drag a tab out of a floating window. The new window floats where it was dropped.
2. Start the drag, then lock the session. A window that was already tiled is back in its old split. A window that appeared during the drag is still floating.

### `xdg-dialog-v1`

`xdg_wm_dialog_v1` and `xdg_dialog_v1` are `src/wayland_objects/xdg_dialog_v1.rs`. `get_xdg_dialog` attaches a dialog to an `xdg_toplevel`. A second dialog for a toplevel that still has one fails with `already_used`. After the dialog is destroyed, a new one may be created. Modal is a stored hint, exposed as IPC `Window.modal` (section 26). It has no focus, stacking, or input effect. A never-mapped toplevel with a dialog and a parent floats at its initial commit, and is centered over its parent (or over its output when the parent is not on the same workspace) on the next map. Remaps and later requests never change floating.

## 28. Commit queue

A `wl_surface.commit` runs in three steps. The check runs at request time and changes nothing. The capture moves everything the commit changes into one `ContentUpdate`. The apply puts that update on screen. An update whose apply is not yet allowed waits in its surface's queue. The surface's own state is in `src/wayland_objects/wl_surface.rs`. The update type is `wl_surface/content_update.rs`, and the queue is `wl_surface/commit_queue.rs`. The xdg and layer roles split their parts the same way in `xdg_surface.rs` and `zwlr_layer_surface_v1.rs`.

### Check, capture, apply

`WlSurface::on_commit` runs the role prologue first. The xdg check (`XdgSurface::check_commit`) and the layer check (`ZwlrLayerSurfaceV1::check_commit`) are read-only. They send their protocol errors there. Then the wl_surface checks run for mapping, explicit sync and representation. Only when all of them pass does the role capture (`take_commit_capture`) run, followed by `capture_commit`. A rejected commit leaves the xdg and layer state untouched.

`capture_commit` builds the payload from the pending state. It takes the pending frame callbacks, presentation feedback, pointer constraint region and hint, and the xwayland serial. It takes the `cached_update` of each synchronized child in the pending stack, and the pending stack and child positions. These go into `ContentUpdate.children` and `ContentUpdate.tree`. It also numbers the commit with `next_update_seq`.

Request-time checks read only state the capture maintains: `WlSurface.latest` (the newest committed buffer, color representation and regions, `LatestCommitted`), `latest_xwayland_serial`, the xdg surface's `initial_commit_requested`, a popup's `map_requested`, and a layer surface's `committed` and `initial_commit_requested`. They never read applied state. A null-buffer commit resets the role's `last_acked` configure at capture. The reset that happens at apply does not touch `last_acked`, so a buffer attached right after an unmap is checked against the reset state.

`apply_update` runs the role part first when the update's role epoch still matches the surface's. It then runs `apply_content`. That applies the xwayland serial and the pointer constraint, then the captured subsurface tree, then the carried children (`apply_children`), then the surface content, in the order of the commit tail before this change.

### Sequence and discard

`next_update_seq` numbers commits from 1. An update covers `first_seq..=last_seq`, which spans more than one commit only after synchronized commits are merged. `applied_update_seq` is the highest `last_seq` that was applied or discarded for the surface.

An update is ready only when `applied_update_seq + 1 >= first_seq`. The same check applies to every child update it carries. So a surface's updates apply in commit order, whether they sit in its own queue or inside a parent's update. A parent update waits for an earlier own update of a child, and a child's own update waits for the parent update that carries its earlier state. Surfaces with no subsurface relation do not depend on each other.

Every path that drops an update calls `discard_update`. It raises `applied_update_seq` to the update's `last_seq`, releases the buffer, completes the release callbacks, discards the presentation feedback, and completes the frame callbacks with `done` and `delete_id`. The buffer's pixels stay valid, because the attached buffer holds its content by `Rc`. Discarding a parent also discards the children it carries.

`dropped_through_seq` is set to `next_update_seq` by `drop_queued_updates`. That runs on destroy, on parent loss (which includes `wl_subsurface.destroy`), on role dismissal, and on detach of a subsurface tree. The surface's queue is emptied, its `cached_update` is discarded, its acquire waiter is stopped, and `latest.buffer` becomes `None`. A carried child update whose `last_seq` is at or below `dropped_through_seq` is discarded at apply and never applied. So an older parent update cannot map a subsurface whose role was destroyed.

### Synchronized subsurfaces

A commit on an effectively synchronized surface is captured and merged into `cached_update` by `merge_cached_update`. A merge keeps the newest payload, frame callbacks, presentation feedback, pointer constraint region and hint. It records the last sequence number and the role epoch of the newest commit. It always retires the buffer it replaces, so that buffer gets its `wl_buffer.release` and its explicit-sync release point signals.

A parent's next commit carries each child's cached update, and `apply_children` applies them after the parent's tree. A grandchild's cached state is captured by the child's commit. A grandchild commit made after the child's last commit waits for the child's next commit. A child destroyed while its update is carried is discarded and its buffer is released.

`wl_subsurface.set_desync` submits a released cached update as a normal update of the child, through `submit_cached_if_parent_desynchronized`. That runs the full commit tail for the released state, including the output refresh and the root geometry update.

### Role epoch

`WlSurface.role_epoch` changes whenever the role object changes. The role setters call `bump_role_epoch` on success. So do `destroy_toplevel_role` and `destroy_popup_role` when the role matched. `set_xwayland_surface_object` and `clear_xwayland_surface_object` do not change it. An update captured under another epoch applies its surface content and skips its xdg or layer part. A destroyed toplevel therefore does not receive a configure from an update captured before it was destroyed.

The xwayland serial is not gated by the epoch. It applies with its update, and destroying the `xwayland_surface_v1` object leaves an association that was already made alone. A pointer constraint update records the constraint it was captured for. At apply, a different constraint drops the region and the hint.

### Explicit sync

An update waits while its acquire point (`payload.sync.acquire`, or that of a carried child) is not signaled. The xdg, layer and lock parts of that commit wait with it. Each surface has at most one acquire waiter, for the first unsignaled point of its queue head (`WlSurface.acquire_waiter`). When the waiter fires, it sends `ClientAction::AcquireWaitDone`. That clears the waiter and flushes the client. A `SyncFile` acquire has no waiter, so the queue head is checked again on every flush.

If the waiter cannot be created, or it reports a failure, the head update is applied with `acquire_forced` set, and a warning is logged. The renderer still waits on the fence before scanout. A newer commit never drops a queued update, so the single blocked-buffer slot of the earlier design is gone.

### Limits

A client may have at most 256 updates queued across all its surfaces (`MAX_QUEUED_UPDATES_PER_CLIENT`). The 257th is discarded. The client receives `wl_display.error` with `no_memory` on that surface and is disconnected. Queued updates hold no file descriptors. A waiter exists only per surface whose queue head waits on a timeline point. `ClientAction::FlushCommitQueues` flushes one client's queues. Closing the client removes its queue index.

### Behavior changes

1. Explicit sync: an update is no longer dropped when a newer commit arrives before its fence signals. Updates apply in order.
2. Explicit sync: the xdg, layer and lock parts of a commit wait for the fence together with the buffer.
3. `wl_subsurface.set_desync` runs the full commit tail for the released cached state, not only the content apply.
4. The xwayland serial association is sent when its update is applied.
5. Nested synchronized subsurfaces: a grandchild's cached state is captured by the child's commit, not by the parent's. A grandchild commit made after the child's last commit waits for the child's next commit.
6. A synchronized subsurface's pointer constraint region and hint apply when its cached state is applied with the parent, not at the child's commit.
7. Unchanged: `xdg_surface.ack_configure` still applies its configure effects at request time. With a queued buffer, the configure can take effect before the buffer is shown.
8. Merging synchronized commits always retires the replaced cached buffer. Before, only a buffer with release callbacks was retired.

### Tests

The queue regression tests are in `wl_surface.rs` `mod tests`. They keep an update queued with `test_hold_next_commit`, and apply it with `test_release_head` followed by `ClientAction::FlushCommitQueues`. They cover a carried update of a destroyed subsurface, cursor offsets, the xwayland serial, pointer constraints, lock, layer and xdg surfaces, frame callbacks and presentation feedback.

### Manual check

1. Run a Vulkan application with explicit sync, such as `vkcube`, and resize its window while it runs. Frames are neither dropped nor shown out of order, and the log has no new warnings.

## 29. Frame pacing: fifo and commit timing

`wp_fifo_v1` and `wp_commit_timer_v1` add two reasons for an update to wait in its surface's queue (section 28). `Readiness::WaitFifo` holds an update that waits for a barrier. `Readiness::WaitTime` holds an update that has a target time. Nothing holds a whole client. The gate state, the timerfd model and the latch hooks are in `src/commit_gate.rs`. The objects are `src/wayland_objects/wp_fifo_v1.rs`, `wp_fifo_manager_v1.rs`, `wp_commit_timer_v1.rs` and `wp_commit_timing_manager_v1.rs`. Both managers are advertised to every client, Xwayland included.

### Barrier lifetime

`wp_fifo_v1.set_barrier` sets a barrier on the surface when its update applies. The barrier is replaced by a newer one on the same surface. Each barrier has a sequence number, and a latch acts only on the barrier it saw. An update with `wait_barrier` waits in the queue while its surface holds a barrier.

A hard latch clears the barrier. A hard latch is a frame submitted to KMS without async flip on the surface's active output (or on any output when the surface has none), with the surface in the frame's callback surfaces. A soft latch shortens the barrier's deadline to `min(deadline, now + one period of that output)`. A soft latch is one of three things: an async (tearing) submission with the surface in the frame; a `NoPresent` frame with the surface in its frame callback surfaces; or any latch of the surface's active output while the surface is not in that frame (occluded). A tearing frame never clears a barrier. `note_latched` runs inside the DRM submit path and reads only the gate: it records each barrier's sequence number and whether its surface was in the frame. The `OutputLatched` action reads the surfaces' active outputs and applies the rules above. So while any barrier exists, every latch of every output queues one `OutputLatched` action, and a surface's active output is the one it has when the action runs.

A barrier also has a fallback deadline, set when the barrier is set. It is 25 ms when the surface has no active output, when that output has no frame scheduler snapshot, when the session is inactive, or when the snapshot's clock mode is `Passive`. Otherwise it is `max(100 ms, 4 × period)`. The long fallback never ends a barrier before a real latch of a visible surface. An occluded surface on the output gets soft latches instead.

Clearing or expiring a barrier sends `ClientAction::FlushCommitQueues` to the surface's client. The commit gate timerfd wakes at the earliest barrier or timed-wait deadline. A destroyed surface, or a closed client, loses its barrier and its timed wait.

### Ready time

A commit with a target time (`wp_commit_timer_v1.set_timestamp`) is ready at `ready_at_with_now`. The target is used as is unless the output's frame clock can predict its vblanks. Prediction needs all of these: the surface is on one output or none, its clock mode is `Fixed` and it is not tearing, the last present has `refresh_is_constant`, is not an async flip and has a kernel timestamp, and that present is no more than 8 periods old. Any other case uses the target.

When prediction applies, the period comes from the nominal refresh and the margin is `min(period / 4, 2 ms)`. Let `base` be the last present's time. If `target + margin <= base`, the update is ready at `base`. Otherwise `k` is the smallest count that makes `base + k × period` at least `target + margin`, and the update is ready at `base + (k − 1) × period + margin`. So the ready time is one margin after the vblank before the target vblank. This holds even when that vblank is the last present. A grid that is late by less than the margin cannot show the update early. Late is allowed, and early is not. No lead is subtracted from the target.

Timestamps are `CLOCK_MONOTONIC` values converted to `Instant`. A timestamp more than 24 h ahead is clamped to 24 h from now, including one that converts successfully. A timestamp that cannot be converted is clamped the same way when it is in the future, and to now when it is in the past. `tv_nsec` above 999 999 999, or `tv_sec_hi` above `0x7fffffff`, is an `invalid_timestamp` error.

The frame clock records what the ready time needs. `PresentFlags` carries `async_flip` and `kernel_timestamp`. `FrameClock::update_mode` forgets the last present when the clock mode or the nominal refresh changes.

### Children's targets

A synchronized child's target time is folded into its parent's update with `max`, and so is each carried child's. The parent update is ready only when the latest of these targets is ready. A child's barrier set flag is carried too: when the parent's update applies, the barrier is set on the child. A synchronized child's wait-barrier flag is cleared before its commit is cached, so the child never waits for a barrier while it is synchronized. The flag stays cleared if the child later becomes desynchronized and its cached state is submitted on its own.

Merging two commits keeps the stronger constraints: set and wait flags are OR-ed, and the later target time wins.

### Xwayland

The two managers are ordinary globals at version 1, advertised to every client, Xwayland included. Xwayland surfaces use the same barrier and target-time paths as other surfaces. Their latches come from the same DRM submit paths.

### Non-goals

tema does not move a variable-refresh output toward the target time. For a variable-refresh output, the target is used as is. Targets are not predicted across more than one output: with two or more outputs the target is used as is. The fallback deadlines are not configurable, and there is no config option for either protocol. A barrier does not make a surface wait for another surface's barrier.
