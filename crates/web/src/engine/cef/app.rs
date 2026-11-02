//! The CEF application object, shared by the browser and helper processes.

use cef::{rc::Rc as _, *};

#[derive(Clone)]
struct Opus;

wrap_app! {
    struct OpusApp {
        inner: Opus,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            process_type: Option<&CefString>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else { return };
            // The browser process (no type) loads the extensions.
            let browser = process_type.is_none_or(|t| t.to_string().is_empty());
            // GPU frames (Linux): Chromium shares dma-bufs when it draws with
            // Vulkan (through ANGLE); GLX, X11's default, can't. An explicit
            // --use-angle wins.
            #[cfg(target_os = "linux")]
            if browser && super::gpu_frames_wanted() && command_line.has_switch(Some(&"use-angle".into())) == 0 {
                command_line.append_switch_with_value(Some(&"use-angle".into()), Some(&"vulkan".into()));
                command_line.append_switch_with_value(
                    Some(&"enable-features".into()),
                    Some(&"Vulkan,VulkanFromANGLE,DefaultANGLEVulkan".into()),
                );
            }
            let extensions = super::EXTENSIONS.get().filter(|e| browser && !e.is_empty());
            if let Some(extensions) = extensions {
                // Chromium splits the list on commas.
                let list: Vec<_> = extensions.iter().map(|p| p.to_string_lossy()).collect();
                command_line.append_switch_with_value(Some(&"load-extension".into()), Some(&list.join(",").as_str().into()));
            }
            // No keyring prompts: CEF profiles keep their own secrets.
            // Name and value go separately: a "name=value" string passed to
            // append_switch becomes a switch literally named that.
            command_line.append_switch_with_value(Some(&"password-store".into()), Some(&"basic".into()));
            command_line.append_switch(Some(&"hide-crash-restore-bubble".into()));
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(OpusBrowserProcess::new(self.inner.clone()))
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(super::renderer::handler())
        }
    }
}

wrap_browser_process_handler! {
    struct OpusBrowserProcess {
        _inner: Opus,
    }

    impl BrowserProcessHandler {
        // Any thread: the pump turns it into work on the main thread.
        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            super::pump::schedule(delay_ms);
        }

        // Another launch on our profile: it hands us its arguments and exits.
        fn on_already_running_app_relaunch(
            &self,
            command_line: Option<&mut CommandLine>,
            _current_directory: Option<&CefString>,
        ) -> ::std::os::raw::c_int {
            let mut list = CefStringList::new();
            if let Some(command_line) = command_line {
                command_line.arguments(Some(&mut list));
            }
            super::relaunched(list.into_iter().collect());
            1
        }
    }
}

pub(crate) fn build() -> cef::App {
    OpusApp::new(Opus)
}
