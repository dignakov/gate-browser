# zui, vendored

GPUI as zeron ships it: https://github.com/zeronsh/zui, at commit
`b564a34413` ("Wayland: keep frames coming after a present outside the frame
loop"). Plain files, not a submodule: fixes to GPUI land here, in this repo's
history.

Our changes on top of that commit are in `../zui-opus.patch` (regenerate it
when they change). To move to a newer zui: replace this folder's contents
with the new snapshot (keep this file), apply the patch, resolve conflicts.

## Our changes

- `gpui_linux/src/linux/platform.rs`: `quit()` during launch is honoured
  (the `quitting` flag); before, a second instance that quit while starting
  hung instead of exiting.
- `gpui/src/window.rs`: `Window::set_opaque_content`. A window that is
  transparent only at its rounded corners keeps subpixel text, which GPUI
  otherwise turns off for any transparent window.
- `gpui/src/window.rs`, `gpui/src/style.rs`, `gpui/src/elements/div.rs`: a div
  with rounded corners that hides its overflow clips its children to those
  corners (`Style::overflow_clip_radius`, `Window::with_overflow_clip`), as
  CSS does; before, children were clipped to a plain rectangle.
- `gpui_wgpu/src/wgpu_context.rs`, `gpui_wgpu/src/wgpu_renderer.rs`: the GPU
  context tries Vulkan alone first and GL only if Vulkan can't draw to the
  window. Enumerating GL starts EGL, which on NVIDIA (with Mesa's EGL beside
  it) printed "libEGL warning: egl: failed to create dri2 screen" at every
  start, and cost startup time, on machines where Vulkan works.
- `gpui/build.rs` (and `gpui/resources/windows/gpui.rc`, removed): the
  Windows manifest's resource script is written into OUT_DIR with the
  manifest's absolute path. The checked-in script named it relative to the
  crate, which llvm-rc (cross-building from Linux) resolves from OUT_DIR
  and couldn't find.
