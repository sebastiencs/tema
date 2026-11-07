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
6. The compositor renders the scene into an output buffer allocated through GBM.
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
   Rebuild the attach/commit model.
8. `src/wayland_objects/wl_buffer.rs` and `src/wayland_objects/wl_shm*.rs`
   See where client image data enters the compositor.
9. `src/render/`
   See how a DRM device chooses OpenGL ES or Vulkan, then how that backend imports a buffer.
10. `src/drm.rs`
   Rebuild KMS resources, framebuffers, and display output.
11. `src/gbm/gbm.rs`
   Understand output buffer allocation.
12. `src/input.rs`, `src/udev.rs`, `src/dbus.rs`
   Rebuild device discovery and session/device access.

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
- direct scanout: scanning out a client buffer directly without an extra composition step
- connector: output endpoint such as HDMI or DP
- CRTC: display timing and scanout engine
- plane: hardware image layer attached to a CRTC

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

On shutdown the runner stops Xwayland, then `ClientManager`, then `ProcessManager`. Stopping the process manager drops the socket. The forker sees the close and exits 0.

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
- `_JAVA_AWT_WM_NONREPARENTING=1`

`DISPLAY` is not sent again, so a compositor nested under X does not pass that outer display to new programs.

A keybinding runs `sh -lc <command>` with no extra environment and no extra descriptors. Failure is a warning: `failed to spawn`, the command in backticks, then the error. A non-zero exit, including death by signal, is a debug line. A zero exit is quiet.

### Degraded mode

The compositor keeps running when the forker is missing.

- `spawn_forker` fails at startup: one error in the log. `ProcessManager` has no socket.
- The forker's pidfd poll fires, a receive fails, or a read returns no bytes: an error in the log, the poll is removed, and the socket is dropped. A spawn that has not reached `Spawned` fails with `forker died`. A child that was already running is marked lost. Its exit will not arrive, and a keybinding does not log that again.
- Any later spawn fails immediately with the warning `forker is unavailable`.

`Stop` uses the same notification with `forker stopped`. That is a normal shutdown, not a death.

### Manual check

Run a normal session from a terminal, with the example bindings (`Super+Return` spawns `alacritty`).

1. `ps -o pid,ppid,pgid,sid,comm` shows `tema reaper` as the parent of both `tema` and `tema forker`. The forker's session id is its own pid.
2. `Super+Return`. The new process is in its own process group, and `ls -l /proc/<pid>/fd/0 /proc/<pid>/fd/1 /proc/<pid>/fd/2` points all three at `/dev/null`.
3. Ctrl+C in the terminal. The session exits 130. The forker is gone. `ps` shows no leftover `tema` zombie.
4. Start again and `kill -9` the compositor pid. The reaper exits 137 and the forker dies.
5. Start again and `kill -9` the forker pid. The compositor logs `forker died` and stays up. Another `Super+Return` warns `forker is unavailable`.
6. Spawn `sleep 300 &` from a keybinding (`sh -lc` is added by the compositor, so the command itself is `sleep 300 &`). `ps -o pid,ppid,comm` shows that `sleep` is a child of `tema reaper`.

## 17. Xwayland

Xwayland is enabled by default. At startup the compositor claims one X display and publishes it to the forker as `DISPLAY=:N`. No Xwayland process exists yet. The first connection to the listening socket is what spawns it. The compositor's own environment stays as it was, including when tema is nested under an outer X server. Only programs the forker starts receive `:N`.

```toml
[xwayland]
enabled = true
```

`enabled = false` logs `Xwayland disabled` and skips the display. A reload that flips `enabled` logs the requested value and the running value, then keeps the running one. Changing it takes a restart.

```text
Start
  disabled, or no display available → stay down, no DISPLAY
  otherwise                         → Waiting
                                      /tmp/.X11-unix/XN and /tmp/.XN-lock
                                      forker DISPLAY=:N
                                      poll the socket, no Xwayland process

connection
  → Launching
      Spawned → Running
                read displayfd and stderr
                adopt the Wayland socket as the Xwayland client
      Failed  → count an early failure
  displayfd text → ready, log "Xwayland ready on :N"
  Wayland client drops → SIGKILL that run; the exit follows

exit of a good run → Waiting again, same :N, same files
early failure      → Waiting again, until the third in a row
third early failure, a listen error, or a lost forker
                   → GivenUp
                     unlink both files, clear forker DISPLAY

Stop → unlink both files, SIGKILL a running server, leave DISPLAY set
```

A completion tagged with an older launch is ignored. A pid that arrives after `Stop` is killed and not adopted.

### File descriptors

Xwayland is started as `Xwayland -terminate -rootless -verbose 10 -displayfd 3 -listenfd 4 -wm 5`. Descriptors 0 and 1 stay `/dev/null`. The spawn also sets `WAYLAND_SOCKET=6` and removes `LIBEI_SOCKET`.

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

Dropping the display unlinks both files. Restart keeps them. Give-up and `Stop` drop them.

### Restart

A run fails early when it never becomes ready, or when it became ready and then exits with a non-zero code or a signal in under ten seconds. A ready exit with code 0 is a clean run at any age. So is a ready run that lasted at least ten seconds, whatever the status. Three early failures in a row give up. Any other run clears the count.

A failed `exec` counts, because the server never became ready. The listening socket still has the unaccepted connection, so the new poll wakes at once and the next attempt follows immediately. A listen error, and a lost forker, give up on the spot. The lost-forker path kills the running server first.

Give-up logs `Xwayland gave up: …`, unlinks both files, and removes `DISPLAY` from the forker. `Stop` leaves `DISPLAY` set, because the forker is stopping too.

### `WM_S0`

`-wm 5` is always passed, and our end of that socket stays open. Xwayland accepts clients on the listen socket only once a window manager owns the `WM_S0` selection. Nothing here claims it yet. That arrives in M5. Until then `xterm` starts Xwayland, the server becomes ready, and the X client waits.

### Xwayland-only globals

A global may answer `xwayland_only`. The default is no, so every current global stays visible to every client. An Xwayland-only global is offered only to the Xwayland client, both in the first registry dump and when the global appears later. A normal client that binds the name is disconnected with `invalid global name`, the same response as an unknown name. Removal is sent only to registries that were offered the name. No such global is registered yet. M4 adds `xwayland_shell_v1`.

### Manual check

Run a normal session from a terminal. Note the `:N` in `Allocated display :N`.

1. `ls /tmp/.X11-unix/XN /tmp/.XN-lock` shows both files. `pgrep -x Xwayland` prints nothing.
2. Spawn a terminal with the usual keybinding. `echo $DISPLAY` prints `:N`.
3. Run `xterm`. The log shows Xwayland starting, `new client_id=… xwayland=true`, one or more `Xwayland:` lines, and `Xwayland ready on :N`. `xterm` waits. That lasts until M5.
4. `kill -9 $(pgrep -x Xwayland)`. The log shows `Xwayland exited with signal 9`. A new `xterm` starts Xwayland again on the same `:N`, and both files are still there. A signal within ten seconds of becoming ready counts as an early failure, so one kill relaunches, and a third early failure gives up.
5. Start a session whose `PATH` can see `sh` and the keybinding's terminal, and cannot see `Xwayland`. `mkdir -p /tmp/no-xwayland`, symlink those two programs into it, and run tema with `PATH=/tmp/no-xwayland`. From a normal shell, run `xterm` with `DISPLAY=:N`. The connection stays queued, so each failed launch tries again at once. The third logs `Xwayland gave up: repeated early failures`. Both files are gone. A terminal spawned after that has an empty `DISPLAY`.
6. `[xwayland] enabled = false`, then start tema. The log says `Xwayland disabled`. Neither file exists, and a spawned terminal has an empty `DISPLAY`.
7. Quit tema during a normal session. No `Xwayland` process remains, and both files are gone.

## 18. Windows

A managed toplevel is a window. The layout, workspaces, grabs, closing snapshots, keybindings, and capture talk about a `WindowId` and reach shell behavior through the `Toplevel` trait. `XdgToplevel` is the only implementor. The types live in `src/window.rs`. The layout tree lives in `src/tiling.rs`.

### Lifetime

`xdg_surface.get_toplevel` allocates the `WindowId` and calls `insert_window`. The first id is 1. Each value is used once.

`WindowEntry` stores the root `wl_surface` and the `ObjectId` of the role object. `windows` is keyed by `WindowId`. `window_by_root` finds the window that currently owns a root. A root hosts at most one window.

`insert_window` also creates the `ManagedWindowState`: tiled, waiting for its first layout, on the workspace the root already belongs to. `destroy_window` drops that state and takes the window out of the workspace layout.

The id is released when that `xdg_toplevel` is destroyed, when its client disconnects, or when the root is unregistered. Unregistering a root is what a deleted `wl_surface` does.

### A new toplevel on the same surface

Destroying the `xdg_toplevel` drops the window and frees the root. The next `get_toplevel` on that same `wl_surface` receives a new `WindowId` and a fresh `ManagedWindowState`. It is tiled and placed like a window that just appeared. Capture does the same for foreign-toplevel handles: `by_window` maps the `WindowId` to a `ToplevelKey`, and a later map receives a new key.

### The trait and the grab rule

`State::toplevel` and `toplevel_mut` read `window_role_object`, then downcast through `Object::as_toplevel`. They panic when that role object is already grabbed. A request handler that holds the role uses `self`. `window_role_object` only reads the map, so it stays safe during the grab. Capture updates that only touch the capture registry are safe from inside the handler. `map_window` calls `state.toplevel`, so it runs from the `wl_surface` commit, where the toplevel is free.

The trait covers the title, the app id, the parent window, size limits, the maximize and fullscreen requests, the decoration mode, close, interactive resize, and a fullscreen request made by the compositor. The xdg protocol actions that still name an `xdg_toplevel` are close, interactive resize configure, and the move and resize requests.

### What stays on the surface

Keyboard focus is a `SurfaceFocus` holding the root surface. `focused_window` reads that surface and looks up the window.

These maps stay keyed by the root surface: the geometry cache, output membership, presentation, process info, `Workspace.root_surfaces`, and `root_surface_workspaces`. Popups, layer surfaces, cursors, and role-less roots stay surfaces. They have no `WindowId`.

`WorkspaceLayout` is keyed by `WindowId`: tiled leaves, the floating stack, and fullscreen. The scene still lists root surfaces. It calls `window_for_root` when it needs the window's layout or animation state.

### X windows

M6 adds X windows on this same path. The X role object stays in `state.objects` and implements `Toplevel`. Creating the window calls `insert_window`. Destroying it calls `destroy_window`. Layout, grabs, snapshots, keybindings, and capture then see it as a window.
