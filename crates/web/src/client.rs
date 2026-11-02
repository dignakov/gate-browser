//! CEF's side of a web view: the client and its handlers. They run on the
//! main thread inside CEF calls, so they only record what happened in
//! [`Shared`] and wake the pump; the view applies it with GPUI's context.
//!
//! Never hold a borrow of [`Shared`] while calling into CEF: CEF may call
//! back synchronously (e.g. `was_resized` asks `view_rect`).

use crate::view::WebEvent;
use cef::{rc::Rc as _, *};
use gpui::{Bounds, CursorStyle, DevicePixels, RenderImage, point, size};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Something the page waits on the user for: a JavaScript dialog or a
/// permission request. The page stays usable; the question waits in its pane.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    Alert { message: String },
    Confirm { message: String },
    Prompt { message: String, default: String },
    /// "Leave site?" before navigating away or reloading.
    Leave { reload: bool },
    Permission { origin: String, what: Vec<&'static str> },
}

/// What a page's media is doing. Media counts once it played audibly.
/// Ordered by precedence: a page with any media playing is playing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Playback {
    #[default]
    None,
    Paused,
    Playing,
}

impl Playback {
    fn parse(state: &str) -> Self {
        match state {
            "playing" => Playback::Playing,
            "paused" => Playback::Paused,
            _ => Playback::None,
        }
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

/// Pixels CEF painted for one surface (the page, or a `<select>` popup).
#[derive(Default)]
pub(crate) struct Pixmap {
    pub width: i32,
    pub height: i32,
    /// BGRA, `width * 4` bytes per row.
    pub pixels: Vec<u8>,
    /// Regions changed since the GPU copy last took them.
    pub damage: Vec<Bounds<DevicePixels>>,
}

impl Pixmap {
    fn take_paint(&mut self, dirty: &[Rect], buffer: *const u8, width: i32, height: i32) {
        if buffer.is_null() || width <= 0 || height <= 0 {
            return;
        }
        let len = width as usize * height as usize * 4;
        // SAFETY: CEF passes a `width * height` BGRA buffer valid for this call.
        let src = unsafe { std::slice::from_raw_parts(buffer, len) };
        let whole = Rect { x: 0, y: 0, width, height };
        if self.width != width || self.height != height {
            self.width = width;
            self.height = height;
            self.pixels = src.to_vec();
            self.damage.clear();
            self.damage.push(to_bounds(&whole));
            return;
        }
        let stride = width as usize * 4;
        for rect in dirty {
            let r = clip(rect, &whole);
            if r.width <= 0 || r.height <= 0 {
                continue;
            }
            for row in r.y..r.y + r.height {
                let start = row as usize * stride + r.x as usize * 4;
                let end = start + r.width as usize * 4;
                self.pixels[start..end].copy_from_slice(&src[start..end]);
            }
            self.damage.push(to_bounds(&r));
        }
    }
}

fn clip(r: &Rect, to: &Rect) -> Rect {
    let x0 = r.x.max(to.x);
    let y0 = r.y.max(to.y);
    let x1 = (r.x + r.width).min(to.x + to.width);
    let y1 = (r.y + r.height).min(to.y + to.height);
    Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }
}

fn to_bounds(r: &Rect) -> Bounds<DevicePixels> {
    Bounds::new(point(DevicePixels(r.x), DevicePixels(r.y)), size(DevicePixels(r.width), DevicePixels(r.height)))
}

/// What a web view's CEF side and GPUI side share.
pub(crate) struct Shared {
    /// Set once CEF created the browser.
    pub browser: Option<Browser>,
    /// The view's size in logical pixels and the display's scale, as GPUI
    /// last laid it out. CEF reads them through `view_rect`/`screen_info`.
    pub view_size: (i32, i32),
    pub scale: f32,
    pub page: Pixmap,
    pub popup: Pixmap,
    /// Where the popup sits, in logical view pixels, while shown.
    pub popup_rect: Option<Rect>,
    pub cursor: CursorStyle,
    /// The question the page is waiting on, if any (one at a time).
    pub ask: Option<(Ask, Reply)>,
    /// The favicon asked for last, and its pixels once they arrived.
    pub favicon_url: Option<String>,
    pub favicon: Option<Arc<RenderImage>>,
    /// Frames with media, by frame identifier, and what it's doing.
    pub media: HashMap<String, Playback>,
    pub events: Vec<WebEvent>,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            browser: None,
            view_size: (800, 600),
            scale: 1.0,
            page: Pixmap::default(),
            popup: Pixmap::default(),
            popup_rect: None,
            cursor: CursorStyle::Arrow,
            ask: None,
            favicon_url: None,
            favicon: None,
            media: HashMap::new(),
            events: Vec::new(),
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
            self.push(WebEvent::Playback(now));
        }
    }

    pub(crate) fn push(&mut self, event: WebEvent) {
        self.events.push(event);
        crate::pump::wake();
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
                shared.popup = Pixmap::default();
            }
            shared.push(WebEvent::Repaint);
        }

        fn on_popup_size(&self, _browser: Option<&mut Browser>, rect: Option<&Rect>) {
            if let Some(rect) = rect {
                let mut shared = self.shared.borrow_mut();
                shared.popup_rect = Some(rect.clone());
                shared.push(WebEvent::Repaint);
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
            let dirty = dirty_rects.unwrap_or(&[]);
            if type_ == PaintElementType::POPUP {
                shared.popup.take_paint(dirty, buffer, width, height);
            } else {
                shared.page.take_paint(dirty, buffer, width, height);
            }
            shared.push(WebEvent::Repaint);
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
                self.shared.borrow_mut().push(WebEvent::Url(text(url)));
            }
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            self.shared.borrow_mut().push(WebEvent::Title(text(title)));
        }

        fn on_favicon_urlchange(&self, _browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>) {
            let first = icon_urls.and_then(|list| strings(list).into_iter().next());
            self.shared.borrow_mut().push(WebEvent::FaviconUrl(first));
        }

        fn on_loading_progress_change(&self, _browser: Option<&mut Browser>, progress: f64) {
            self.shared.borrow_mut().push(WebEvent::Progress(progress));
        }

        fn on_media_access_change(
            &self,
            _browser: Option<&mut Browser>,
            has_video_access: ::std::os::raw::c_int,
            has_audio_access: ::std::os::raw::c_int,
        ) {
            let (camera, microphone) = (has_video_access == 1, has_audio_access == 1);
            self.shared.borrow_mut().push(WebEvent::Media { camera, microphone });
        }

        fn on_fullscreen_mode_change(&self, _browser: Option<&mut Browser>, fullscreen: ::std::os::raw::c_int) {
            self.shared.borrow_mut().push(WebEvent::Fullscreen(fullscreen == 1));
        }

        fn on_cursor_change(
            &self,
            _browser: Option<&mut Browser>,
            _cursor: ::std::os::raw::c_ulong,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> ::std::os::raw::c_int {
            let mut shared = self.shared.borrow_mut();
            let cursor = crate::keys::cursor(type_);
            if shared.cursor != cursor {
                shared.cursor = cursor;
                shared.push(WebEvent::Repaint);
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
        fn on_load_start(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _transition_type: TransitionType) {
            let Some(frame) = frame.filter(|f| f.is_main() == 1) else { return };
            let url = CefString::from(&frame.url()).to_string();
            self.shared.borrow_mut().push(WebEvent::FaviconUrl(default_favicon(&url)));
        }

        fn on_loading_state_change(
            &self,
            _browser: Option<&mut Browser>,
            is_loading: ::std::os::raw::c_int,
            can_go_back: ::std::os::raw::c_int,
            can_go_forward: ::std::os::raw::c_int,
        ) {
            self.shared.borrow_mut().push(WebEvent::Loading {
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
            self.shared.borrow_mut().push(WebEvent::OpenTab { url: text(target_url), background });
            1
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let mut shared = self.shared.borrow_mut();
            shared.browser = browser.cloned();
            shared.push(WebEvent::Created);
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            let mut shared = self.shared.borrow_mut();
            shared.browser = None;
            shared.push(WebEvent::Closed);
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
            shared.ask = Some((ask, Reply::Dialog(callback.clone())));
            shared.push(WebEvent::Ask);
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
            shared.ask = Some((Ask::Leave { reload: is_reload == 1 }, Reply::Dialog(callback.clone())));
            shared.push(WebEvent::Ask);
            1
        }

        fn on_reset_dialog_state(&self, _browser: Option<&mut Browser>) {
            // Navigation cancelled any pending dialog; its callback is void.
            let mut shared = self.shared.borrow_mut();
            if matches!(shared.ask, Some((_, Reply::Dialog(_)))) {
                shared.ask = None;
                shared.push(WebEvent::Ask);
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
            shared.ask = Some((ask, Reply::Media(callback.clone(), requested_permissions)));
            shared.push(WebEvent::Ask);
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
            shared.ask = Some((ask, Reply::Prompt(callback.clone(), prompt_id)));
            shared.push(WebEvent::Ask);
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
                shared.push(WebEvent::Ask);
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
            shared.push(WebEvent::Crashed);
            // Its frames went with it.
            if !shared.media.is_empty() {
                shared.media.clear();
                shared.push(WebEvent::Playback(Playback::None));
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
            let mut shared = self.shared.borrow_mut();
            // A newer favicon was asked for meanwhile.
            if shared.favicon_url.as_deref() != Some(text(image_url).as_str()) {
                return;
            }
            shared.favicon = picture;
            shared.push(WebEvent::Favicon);
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

pub(crate) fn favicon_callback(shared: &SharedRef, scale: f32) -> DownloadImageCallback {
    FaviconLoaded::new(shared.clone(), scale)
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
            if CefString::from(&message.name()).to_string() != crate::renderer::MEDIA_STATE {
                return 0;
            }
            let state = message.argument_list().map(|args| CefString::from(&args.string(0)).to_string()).unwrap_or_default();
            let id = CefString::from(&frame.identifier()).to_string();
            self.shared.borrow_mut().set_media(id, Playback::parse(&state));
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
