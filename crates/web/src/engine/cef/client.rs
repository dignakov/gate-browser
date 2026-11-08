//! CEF's side of a page: the client and its handlers. They run on the main
//! thread inside CEF calls; they keep CEF's state in [`Shared`] and report
//! what the page does as [`PageEvent`]s.
//!
//! Never hold a borrow of [`Shared`] while calling into CEF: CEF may call
//! back synchronously (e.g. `was_resized` asks `view_rect`).

use crate::engine::{Events, PageEvent};
use crate::{Ask, Playback};
use cef::{rc::Rc as _, *};
use gpui::{CursorStyle, RenderImage};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

fn parse_playback(state: &str) -> Playback {
    match state {
        "playing" => Playback::Playing,
        "paused" => Playback::Paused,
        _ => Playback::None,
    }
}

/// How to answer an [`Ask`].
pub(crate) enum Reply {
    Dialog(JsdialogCallback),
    /// The callback and the permissions asked for.
    Media(MediaAccessCallback, u32),
    /// The callback and the prompt's id.
    Prompt(PermissionPromptCallback, u64),
}

/// What a web view's CEF side and GPUI side share.
pub(crate) struct Shared {
    /// Set once CEF created the browser.
    pub browser: Option<Browser>,
    /// The view's size in logical pixels and the display's scale, as GPUI
    /// last laid it out. CEF reads them through `view_rect`/`screen_info`.
    pub view_size: (i32, i32),
    pub scale: f32,
    /// What the view last asked for, applied once the browser exists.
    pub hidden: bool,
    pub focused: bool,
    /// The page's address, as last reported (a replacement browser loads it).
    pub url: String,
    /// The window's atlas, which frames go into as they arrive.
    pub frames: Option<super::frames::Frames>,
    pub page: super::frames::Surface,
    pub popup: super::frames::Surface,
    /// GPU frames (Linux): set while this page takes them.
    #[cfg(target_os = "linux")]
    pub gpu: Option<super::gpu_frames::GpuFrames>,
    /// GPU frames failed: the page falls back to CPU frames (in `pump`).
    pub gpu_failed: bool,
    /// Where the popup sits, in logical view pixels, while shown.
    pub popup_rect: Option<Rect>,
    pub cursor: CursorStyle,
    /// The question the page is waiting on, if any (one at a time).
    pub ask: Option<(Ask, Reply)>,
    /// The favicon asked for last.
    pub favicon_url: Option<String>,
    /// Frames with media, by frame identifier, and what it's doing.
    pub media: HashMap<String, Playback>,
    pub events: Events,
    /// The page's frames, paced by display refreshes.
    pub pacing: super::pacing::Pacing,
    /// A drag the page started (HTML drag and drop), until the button is
    /// released.
    pub drag: Option<Drag>,
}

/// A drag within the page: Chromium is both its source and its target.
pub(crate) struct Drag {
    /// What the source allows.
    pub allowed: DragOperationsMask,
    /// What the target under the pointer would do with a drop.
    pub operation: DragOperationsMask,
}

impl Shared {
    pub fn new(url: &str, events: Events) -> Self {
        Self {
            browser: None,
            view_size: (800, 600),
            scale: 1.0,
            hidden: false,
            focused: false,
            url: url.to_string(),
            frames: None,
            page: Default::default(),
            popup: Default::default(),
            #[cfg(target_os = "linux")]
            gpu: None,
            gpu_failed: false,
            popup_rect: None,
            cursor: CursorStyle::Arrow,
            ask: None,
            favicon_url: None,
            media: HashMap::new(),
            events,
            pacing: Default::default(),
            drag: None,
        }
    }

    /// The page's media: playing if any frame plays, paused if any waits.
    pub(crate) fn playback(&self) -> Playback {
        self.media.values().copied().max().unwrap_or_default()
    }

    /// Records what `frame`'s media does; tells the view when the page's
    /// changes.
    fn set_media(&mut self, frame: String, state: Playback) {
        let before = self.playback();
        if state == Playback::None {
            self.media.remove(&frame);
        } else {
            self.media.insert(frame, state);
        }
        let now = self.playback();
        if now != before {
            self.push(PageEvent::Playback(now));
        }
    }

    pub(crate) fn push(&self, event: PageEvent) {
        // The view is gone if nobody listens: nothing to tell.
        let _ = self.events.try_send(event);
    }
}

type SharedRef = Rc<RefCell<Shared>>;

fn text(s: Option<&CefString>) -> String {
    s.map(|s| s.to_string()).unwrap_or_default()
}

/// The strings of a list CEF lends to a callback. Each value is read into
/// a buffer we own (a default `CefString` is an empty borrow, not a
/// writable buffer) and cleared once copied.
fn strings(list: &mut CefStringList) -> Vec<String> {
    use cef_dll_sys::{cef_string_list_size, cef_string_list_value, cef_string_t, cef_string_utf16_clear};
    let raw: *mut cef_dll_sys::_cef_string_list_t = list.into();
    if raw.is_null() {
        return Vec::new();
    }
    // SAFETY: `raw` is the live list CEF passed to this callback, and
    // `value` is a zeroed string CEF fills and we clear.
    unsafe {
        (0..cef_string_list_size(raw))
            .filter_map(|i| {
                let mut value: cef_string_t = std::mem::zeroed();
                let ok = cef_string_list_value(raw, i, &mut value) != 0;
                let out = ok.then(|| CefString::from(std::ptr::from_ref(&value)).to_string());
                cef_string_utf16_clear(&mut value);
                out
            })
            .collect()
    }
}

wrap_render_handler! {
    struct Render {
        shared: SharedRef,
    }

    impl RenderHandler {
        fn view_rect(&self, _browser: Option<&mut Browser>, rect: Option<&mut Rect>) {
            let Some(rect) = rect else { return };
            let (w, h) = self.shared.borrow().view_size;
            // CEF requires a non-empty view.
            *rect = Rect { x: 0, y: 0, width: w.max(1), height: h.max(1) };
        }

        fn screen_info(&self, _browser: Option<&mut Browser>, info: Option<&mut ScreenInfo>) -> ::std::os::raw::c_int {
            let Some(info) = info else { return 0 };
            info.device_scale_factor = self.shared.borrow().scale;
            1
        }

        fn on_popup_show(&self, _browser: Option<&mut Browser>, show: ::std::os::raw::c_int) {
            let mut shared = self.shared.borrow_mut();
            if show == 0 {
                shared.popup_rect = None;
                let shared = &mut *shared;
                if let Some(frames) = &shared.frames {
                    frames.release(&mut shared.popup);
                }
            }
            shared.push(PageEvent::Repaint);
        }

        fn on_popup_size(&self, _browser: Option<&mut Browser>, rect: Option<&Rect>) {
            if let Some(rect) = rect {
                let mut shared = self.shared.borrow_mut();
                shared.popup_rect = Some(rect.clone());
                shared.push(PageEvent::Repaint);
            }
        }

        // GPU frames (Linux): copied on the GPU before CEF takes the buffer
        // back, which is when this returns.
        fn on_accelerated_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            dirty_rects: Option<&[Rect]>,
            info: Option<&AcceleratedPaintInfo>,
        ) {
            #[cfg(target_os = "linux")]
            {
                let mut shared = self.shared.borrow_mut();
                let shared = &mut *shared;
                shared.pacing.painted(std::time::Instant::now());
                let (Some(gpu), Some(frames), Some(info)) = (shared.gpu.as_mut(), shared.frames.as_ref(), info) else { return };
                let surface = if type_ == PaintElementType::POPUP { &mut shared.popup } else { &mut shared.page };
                if gpu.take(frames, surface, info, dirty_rects.unwrap_or(&[])) {
                    shared.push(PageEvent::Repaint);
                } else {
                    shared.gpu_failed = true;
                    shared.push(PageEvent::Pump);
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = (type_, dirty_rects, info);
        }

        /// A drag (HTML drag and drop) begins. Gate has no drags of its own
        /// between apps yet, so it plays the system's part within the page:
        /// the page is the drop target too, fed by the pointer (see
        /// `CefPage::input`) until the button is released.
        fn start_dragging(
            &self,
            browser: Option<&mut Browser>,
            drag_data: Option<&mut DragData>,
            allowed_ops: DragOperationsMask,
            x: ::std::os::raw::c_int,
            y: ::std::os::raw::c_int,
        ) -> ::std::os::raw::c_int {
            let (Some(host), Some(drag_data)) = (browser.and_then(|b| b.host()), drag_data) else { return 0 };
            self.shared.borrow_mut().drag = Some(Drag { allowed: allowed_ops, operation: DragOperationsMask::default() });
            let event = MouseEvent { x, y, modifiers: super::keys::LEFT_BUTTON };
            host.drag_target_drag_enter(Some(drag_data), Some(&event), allowed_ops);
            host.drag_target_drag_over(Some(&event), allowed_ops);
            1
        }

        fn update_drag_cursor(&self, _browser: Option<&mut Browser>, operation: DragOperationsMask) {
            if let Some(drag) = self.shared.borrow_mut().drag.as_mut() {
                drag.operation = operation;
            }
        }

        fn on_paint(
            &self,
            _browser: Option<&mut Browser>,
            type_: PaintElementType,
            dirty_rects: Option<&[Rect]>,
            buffer: *const u8,
            width: ::std::os::raw::c_int,
            height: ::std::os::raw::c_int,
        ) {
            let mut shared = self.shared.borrow_mut();
            let shared = &mut *shared;
            shared.pacing.painted(std::time::Instant::now());
            let Some(frames) = shared.frames.as_ref() else { return };
            let surface = if type_ == PaintElementType::POPUP { &mut shared.popup } else { &mut shared.page };
            // Uploaded now, straight from CEF's buffer (valid for this call).
            if let Err(error) = frames.take_pixels(surface, dirty_rects.unwrap_or(&[]), buffer, width, height) {
                tracing::warn!("web frame upload: {error:#}");
            }
            shared.push(PageEvent::Repaint);
        }
    }
}

wrap_display_handler! {
    struct Display {
        shared: SharedRef,
    }

    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if frame.is_some_and(|f| f.is_main() == 1) {
                let mut shared = self.shared.borrow_mut();
                shared.url = text(url);
                shared.push(PageEvent::Url(shared.url.clone()));
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            self.shared.borrow().push(PageEvent::Title(text(title)));
        }

        fn on_favicon_urlchange(&self, browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>) {
            let first = icon_urls.and_then(|list| strings(list).into_iter().next());
            if let Some(browser) = browser {
                fetch_favicon(&self.shared, browser, first);
            }
        }

        fn on_loading_progress_change(&self, _browser: Option<&mut Browser>, progress: f64) {
            self.shared.borrow().push(PageEvent::Progress(progress));
        }

        fn on_media_access_change(
            &self,
            _browser: Option<&mut Browser>,
            has_video_access: ::std::os::raw::c_int,
            has_audio_access: ::std::os::raw::c_int,
        ) {
            let (camera, microphone) = (has_video_access == 1, has_audio_access == 1);
            self.shared.borrow().push(PageEvent::Media { camera, microphone });
        }

        fn on_fullscreen_mode_change(&self, _browser: Option<&mut Browser>, fullscreen: ::std::os::raw::c_int) {
            self.shared.borrow().push(PageEvent::Fullscreen(fullscreen == 1));
        }

        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: cef::sys::cef_cursor_handle_t,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> ::std::os::raw::c_int {
            let mut shared = self.shared.borrow_mut();
            let cursor = super::keys::cursor(type_);
            if shared.cursor != cursor {
                shared.cursor = cursor;
                shared.push(PageEvent::Cursor(cursor));
            }
            1
        }
    }
}

wrap_load_handler! {
    struct Load {
        shared: SharedRef,
    }

    impl LoadHandler {
        // A new document: until it names favicons, assume the site's
        // default one, as browsers do (CEF only reports declared icons).
        fn on_load_start(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, _transition_type: TransitionType) {
            let (Some(browser), Some(frame)) = (browser, frame.filter(|f| f.is_main() == 1)) else { return };
            let url = CefString::from(&frame.url()).to_string();
            fetch_favicon(&self.shared, browser, default_favicon(&url));
        }

        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            self.shared.borrow().push(PageEvent::Loading {
                loading: is_loading == 1,
                can_go_back: can_go_back == 1,
                can_go_forward: can_go_forward == 1,
            });
        }
    }
}

wrap_life_span_handler! {
    struct LifeSpan {
        shared: SharedRef,
    }

    impl LifeSpanHandler {
        fn on_before_popup(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            _popup_id: ::std::os::raw::c_int,
            target_url: Option<&CefString>,
            _target_frame_name: Option<&CefString>,
            target_disposition: WindowOpenDisposition,
            _user_gesture: ::std::os::raw::c_int,
            _popup_features: Option<&PopupFeatures>,
            _window_info: Option<&mut WindowInfo>,
            _client: Option<&mut Option<Client>>,
            _settings: Option<&mut BrowserSettings>,
            _extra_info: Option<&mut Option<DictionaryValue>>,
            _no_javascript_access: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            // We own tabs: every new window becomes one of our tabs.
            let background = target_disposition == WindowOpenDisposition::NEW_BACKGROUND_TAB;
            self.shared.borrow().push(PageEvent::OpenTab { url: text(target_url), background, alternate: false });
            1
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let (hidden, focused) = {
                let mut shared = self.shared.borrow_mut();
                shared.browser = browser.as_deref().cloned();
                (shared.hidden, shared.focused)
            };
            // What the view asked for before the browser existed.
            let Some(host) = browser.and_then(|b| b.host()) else { return };
            host.notify_screen_info_changed();
            host.was_resized();
            if focused {
                host.set_focus(1);
            }
            if hidden {
                host.was_hidden(1);
            }
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            let mut shared = self.shared.borrow_mut();
            // A browser replaced by a new one (see `WebView::recreate`) may
            // close after its successor arrived.
            let closing = browser.map(|b| b.identifier());
            if shared.browser.as_ref().map(|b| b.identifier()) != closing {
                return;
            }
            shared.browser = None;
            shared.push(PageEvent::Closed);
        }
    }
}

wrap_jsdialog_handler! {
    struct Dialogs {
        shared: SharedRef,
    }

    impl JsdialogHandler {
        fn on_jsdialog(
            &self,
            _browser: Option<&mut Browser>,
            _origin_url: Option<&CefString>,
            dialog_type: JsdialogType,
            message_text: Option<&CefString>,
            default_prompt_text: Option<&CefString>,
            callback: Option<&mut JsdialogCallback>,
            suppress_message: Option<&mut ::std::os::raw::c_int>,
        ) -> ::std::os::raw::c_int {
            let mut shared = self.shared.borrow_mut();
            let Some(callback) = callback.filter(|_| shared.ask.is_none()) else {
                // One question at a time: a second dialog is dropped (as
                // if cancelled), like a browser suppressing dialog spam.
                if let Some(suppress) = suppress_message {
                    *suppress = 1;
                }
                return 0;
            };
            let message = text(message_text);
            let ask = match dialog_type {
                JsdialogType::CONFIRM => Ask::Confirm { message },
                JsdialogType::PROMPT => Ask::Prompt { message, default: text(default_prompt_text) },
                _ => Ask::Alert { message },
            };
            shared.push(PageEvent::Ask(Some(ask.clone())));
            shared.ask = Some((ask, Reply::Dialog(callback.clone())));
            1
        }

        fn on_before_unload_dialog(
            &self,
            _browser: Option<&mut Browser>,
            _message_text: Option<&CefString>,
            is_reload: ::std::os::raw::c_int,
            callback: Option<&mut JsdialogCallback>,
        ) -> ::std::os::raw::c_int {
            let mut shared = self.shared.borrow_mut();
            let Some(callback) = callback.filter(|_| shared.ask.is_none()) else { return 0 };
            let ask = Ask::Leave { reload: is_reload == 1 };
            shared.push(PageEvent::Ask(Some(ask.clone())));
            shared.ask = Some((ask, Reply::Dialog(callback.clone())));
            1
        }

        fn on_reset_dialog_state(&self, _browser: Option<&mut Browser>) {
            // Navigation cancelled any pending dialog; its callback is void.
            let mut shared = self.shared.borrow_mut();
            if matches!(shared.ask, Some((_, Reply::Dialog(_)))) {
                shared.ask = None;
                shared.push(PageEvent::Ask(None));
            }
        }
    }
}

wrap_permission_handler! {
    struct Permissions {
        shared: SharedRef,
    }

    impl PermissionHandler {
        fn on_request_media_access_permission(
            &self,
            _browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut MediaAccessCallback>,
        ) -> ::std::os::raw::c_int {
            let mut shared = self.shared.borrow_mut();
            // Busy or no way to answer: CEF's default, which denies.
            let Some(callback) = callback.filter(|_| shared.ask.is_none()) else { return 0 };
            let ask = Ask::Permission { origin: text(requesting_origin), what: media_names(requested_permissions) };
            shared.push(PageEvent::Ask(Some(ask.clone())));
            shared.ask = Some((ask, Reply::Media(callback.clone(), requested_permissions)));
            1
        }

        fn on_show_permission_prompt(
            &self,
            _browser: Option<&mut Browser>,
            prompt_id: u64,
            requesting_origin: Option<&CefString>,
            requested_permissions: u32,
            callback: Option<&mut PermissionPromptCallback>,
        ) -> ::std::os::raw::c_int {
            let mut shared = self.shared.borrow_mut();
            let Some(callback) = callback.filter(|_| shared.ask.is_none()) else { return 0 };
            let ask = Ask::Permission { origin: text(requesting_origin), what: permission_names(requested_permissions) };
            shared.push(PageEvent::Ask(Some(ask.clone())));
            shared.ask = Some((ask, Reply::Prompt(callback.clone(), prompt_id)));
            1
        }

        fn on_dismiss_permission_prompt(
            &self,
            _browser: Option<&mut Browser>,
            prompt_id: u64,
            _result: PermissionRequestResult,
        ) {
            let mut shared = self.shared.borrow_mut();
            if matches!(shared.ask, Some((_, Reply::Prompt(_, id))) if id == prompt_id) {
                shared.ask = None;
                shared.push(PageEvent::Ask(None));
            }
        }
    }
}

/// `/favicon.ico` at a web page's origin.
fn default_favicon(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().filter(|h| !h.is_empty())?;
    Some(format!("{scheme}://{host}/favicon.ico"))
}

/// What a media access request wants, in words.
fn media_names(bits: u32) -> Vec<&'static str> {
    use cef::sys::cef_media_access_permission_types_t as M;
    let mut out = Vec::new();
    if bits & (M::CEF_MEDIA_PERMISSION_DEVICE_VIDEO_CAPTURE as u32) != 0 {
        out.push("camera");
    }
    if bits & (M::CEF_MEDIA_PERMISSION_DEVICE_AUDIO_CAPTURE as u32) != 0 {
        out.push("microphone");
    }
    if bits & (M::CEF_MEDIA_PERMISSION_DESKTOP_VIDEO_CAPTURE as u32 | M::CEF_MEDIA_PERMISSION_DESKTOP_AUDIO_CAPTURE as u32) != 0 {
        out.push("your screen");
    }
    out
}

/// What a permission prompt wants, in words.
fn permission_names(bits: u32) -> Vec<&'static str> {
    use cef::sys::cef_permission_request_types_t as P;
    const NAMES: &[(P, &str)] = &[
        (P::CEF_PERMISSION_TYPE_CAMERA_STREAM, "camera"),
        (P::CEF_PERMISSION_TYPE_MIC_STREAM, "microphone"),
        (P::CEF_PERMISSION_TYPE_GEOLOCATION, "location"),
        (P::CEF_PERMISSION_TYPE_NOTIFICATIONS, "notifications"),
        (P::CEF_PERMISSION_TYPE_CLIPBOARD, "clipboard"),
        (P::CEF_PERMISSION_TYPE_MULTIPLE_DOWNLOADS, "multiple downloads"),
        (P::CEF_PERMISSION_TYPE_MIDI_SYSEX, "MIDI devices"),
        (P::CEF_PERMISSION_TYPE_LOCAL_FONTS, "local fonts"),
        (P::CEF_PERMISSION_TYPE_IDLE_DETECTION, "idle detection"),
        (P::CEF_PERMISSION_TYPE_WINDOW_MANAGEMENT, "window management"),
        (P::CEF_PERMISSION_TYPE_STORAGE_ACCESS, "storage access"),
        (P::CEF_PERMISSION_TYPE_TOP_LEVEL_STORAGE_ACCESS, "storage access"),
        (P::CEF_PERMISSION_TYPE_LOCAL_NETWORK, "local network"),
        (P::CEF_PERMISSION_TYPE_SENSORS, "motion sensors"),
    ];
    let mut out: Vec<&'static str> = Vec::new();
    for (flag, name) in NAMES {
        if bits & (*flag as u32) != 0 && !out.contains(name) {
            out.push(name);
        }
    }
    if out.is_empty() {
        out.push("a permission");
    }
    out
}

wrap_request_handler! {
    struct Requests {
        shared: SharedRef,
    }

    impl RequestHandler {
        fn on_render_process_terminated(
            &self,
            _browser: Option<&mut Browser>,
            _status: TerminationStatus,
            _error_code: ::std::os::raw::c_int,
            _error_string: Option<&CefString>,
        ) {
            let mut shared = self.shared.borrow_mut();
            shared.push(PageEvent::Crashed);
            // Its frames went with it.
            if !shared.media.is_empty() {
                shared.media.clear();
                shared.push(PageEvent::Playback(Playback::None));
            }
        }
    }
}

wrap_download_image_callback! {
    pub(crate) struct FaviconLoaded {
        shared: SharedRef,
        scale: f32,
    }

    impl DownloadImageCallback {
        fn on_download_image_finished(
            &self,
            image_url: Option<&CefString>,
            _http_status_code: ::std::os::raw::c_int,
            image: Option<&mut Image>,
        ) {
            // Read the pixels before borrowing: it calls into CEF.
            let picture = image.and_then(|image| bitmap(image, self.scale));
            let shared = self.shared.borrow();
            // A newer favicon was asked for meanwhile.
            if shared.favicon_url.as_deref() != Some(text(image_url).as_str()) {
                return;
            }
            shared.push(PageEvent::Favicon(picture));
        }
    }
}

/// An image's pixels at `scale`, as GPUI's BGRA.
fn bitmap(image: &Image, scale: f32) -> Option<Arc<RenderImage>> {
    let (mut width, mut height) = (0, 0);
    let data = image.as_bitmap(scale, ColorType::BGRA_8888, AlphaType::POSTMULTIPLIED, Some(&mut width), Some(&mut height))?;
    let mut bytes = vec![0u8; data.size()];
    data.data(Some(&mut bytes), 0);
    let buffer = image::RgbaImage::from_raw(width.max(0) as u32, height.max(0) as u32, bytes)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

/// Fetches the page's favicon (`None`: it has none); a
/// [`PageEvent::Favicon`] follows when the pixels arrive, sized for the
/// sidebar's tiles at the display's scale.
fn fetch_favicon(shared: &SharedRef, browser: &Browser, url: Option<String>) {
    let scale = {
        let mut state = shared.borrow_mut();
        if state.favicon_url == url {
            return;
        }
        state.favicon_url = url.clone();
        state.scale
    };
    let Some(url) = url else {
        shared.borrow().push(PageEvent::Favicon(None));
        return;
    };
    let mut callback = FaviconLoaded::new(shared.clone(), scale);
    let size = (32.0 * scale).ceil() as u32;
    if let Some(host) = browser.host() {
        host.download_image(Some(&CefString::from(url.as_str())), 1, size, 0, Some(&mut callback));
    }
}

wrap_frame_handler! {
    struct Frames {
        shared: SharedRef,
    }

    impl FrameHandler {
        // A removed frame (an embedded player, say) stops counting.
        fn on_frame_detached(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>) {
            let Some(frame) = frame else { return };
            let id = CefString::from(&frame.identifier()).to_string();
            self.shared.borrow_mut().set_media(id, Playback::None);
        }
    }
}

wrap_client! {
    struct OpusClient {
        render: RenderHandler,
        display: DisplayHandler,
        load: LoadHandler,
        life_span: LifeSpanHandler,
        dialogs: JsdialogHandler,
        permissions: PermissionHandler,
        requests: RequestHandler,
        frames: FrameHandler,
        shared: SharedRef,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(self.display.clone())
        }
        fn load_handler(&self) -> Option<LoadHandler> {
            Some(self.load.clone())
        }
        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(self.life_span.clone())
        }
        fn jsdialog_handler(&self) -> Option<JsdialogHandler> {
            Some(self.dialogs.clone())
        }
        fn permission_handler(&self) -> Option<PermissionHandler> {
            Some(self.permissions.clone())
        }
        fn request_handler(&self) -> Option<RequestHandler> {
            Some(self.requests.clone())
        }
        fn frame_handler(&self) -> Option<FrameHandler> {
            Some(self.frames.clone())
        }

        // Reports from our renderer-side scripts (see `renderer`).
        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let (Some(frame), Some(message)) = (frame, message) else { return 0 };
            if CefString::from(&message.name()).to_string() != super::renderer::MEDIA_STATE {
                return 0;
            }
            let state = message.argument_list().map(|args| CefString::from(&args.string(0)).to_string()).unwrap_or_default();
            let id = CefString::from(&frame.identifier()).to_string();
            self.shared.borrow_mut().set_media(id, parse_playback(&state));
            1
        }
    }
}

pub(crate) fn build(shared: &SharedRef) -> Client {
    OpusClient::new(
        Render::new(shared.clone()),
        Display::new(shared.clone()),
        Load::new(shared.clone()),
        LifeSpan::new(shared.clone()),
        Dialogs::new(shared.clone()),
        Permissions::new(shared.clone()),
        Requests::new(shared.clone()),
        Frames::new(shared.clone()),
        shared.clone(),
    )
}
