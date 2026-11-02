//! The renderer process's side: scripts that tell the browser process what
//! a page does which CEF doesn't report, such as playing media, and carry
//! out what the browser asks of them (play, pause).

use cef::{rc::Rc as _, *};
use std::cell::RefCell;
use std::collections::HashMap;

/// A frame's media state changed: one string argument, "playing",
/// "paused" or "none".
pub(crate) const MEDIA_STATE: &str = "opus.media.state";
/// The browser asks a frame to "play" or "pause" its media.
pub(crate) const MEDIA_CONTROL: &str = "opus.media.control";

const MEDIA_SCRIPT: &str = include_str!("../../js/media.js");

thread_local! {
    /// Each frame's media control (the watcher's returned function), by
    /// frame identifier, while its page context lives.
    static CONTROLS: RefCell<HashMap<String, V8Value>> = RefCell::new(HashMap::new());
}

fn frame_id(frame: &Frame) -> String {
    CefString::from(&frame.identifier()).to_string()
}

#[derive(Clone)]
pub(crate) struct Renderer;

wrap_render_process_handler! {
    pub(crate) struct OpusRenderer {
        _inner: Renderer,
    }

    impl RenderProcessHandler {
        // Before the page's scripts: hand the media watcher its reporter. The
        // page never sees the reporter or the control; they live in closures.
        fn on_context_created(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            let (Some(frame), Some(context)) = (frame, context) else { return };
            let mut watcher = None;
            let mut exception = None;
            let code = CefString::from(MEDIA_SCRIPT);
            let url = CefString::from("opus://media.js");
            if context.eval(Some(&code), Some(&url), 1, Some(&mut watcher), Some(&mut exception)) != 1 {
                tracing::warn!("media watcher did not load");
                return;
            }
            let Some(watcher) = watcher else { return };
            let mut handler = MediaReporter::new(Renderer);
            let report = v8_value_create_function(Some(&CefString::from("report")), Some(&mut handler));
            if let Some(control) = watcher.execute_function(None, Some(&[report])) {
                CONTROLS.with_borrow_mut(|c| c.insert(frame_id(frame), control));
            }
        }

        fn on_context_released(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _context: Option<&mut V8Context>) {
            if let Some(frame) = frame {
                CONTROLS.with_borrow_mut(|c| c.remove(&frame_id(frame)));
            }
        }

        fn on_process_message_received(
            &self,
            _browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            _source_process: ProcessId,
            message: Option<&mut ProcessMessage>,
        ) -> ::std::os::raw::c_int {
            let (Some(frame), Some(message)) = (frame, message) else { return 0 };
            if CefString::from(&message.name()).to_string() != MEDIA_CONTROL {
                return 0;
            }
            let command = message.argument_list().map(|a| CefString::from(&a.string(0))).unwrap_or_default();
            let control = CONTROLS.with_borrow(|c| c.get(&frame_id(frame)).cloned());
            if let (Some(control), Some(context)) = (control, frame.v8_context())
                && context.enter() == 1
            {
                control.execute_function(None, Some(&[v8_value_create_string(Some(&command))]));
                context.exit();
            }
            1
        }
    }
}

wrap_v8_handler! {
    struct MediaReporter {
        _inner: Renderer,
    }

    impl V8Handler {
        fn execute(
            &self,
            _name: Option<&CefString>,
            _object: Option<&mut V8Value>,
            arguments: Option<&[Option<V8Value>]>,
            _retval: Option<&mut Option<V8Value>>,
            _exception: Option<&mut CefString>,
        ) -> ::std::os::raw::c_int {
            let Some(state) = arguments.and_then(|a| a.first().cloned().flatten()) else { return 1 };
            let state = CefString::from(&state.string_value());
            let Some(frame) = v8_context_get_current_context().and_then(|c| c.frame()) else { return 1 };
            let Some(mut message) = process_message_create(Some(&CefString::from(MEDIA_STATE))) else { return 1 };
            if let Some(args) = message.argument_list() {
                args.set_string(0, Some(&state));
            }
            frame.send_process_message(ProcessId::BROWSER, Some(&mut message));
            1
        }
    }
}

pub(crate) fn handler() -> RenderProcessHandler {
    OpusRenderer::new(Renderer)
}
