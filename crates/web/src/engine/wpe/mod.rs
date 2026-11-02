//! WPE WebKit (Linux). Pages live in a helper process (`helper.c`, built by
//! build.rs) that is their WPEPlatform: they render into dma-bufs on GPUI's
//! GPU, and GPUI copies each frame's damage into the page's atlas tile
//! ([`present`]). One helper serves every page of the profile; packets and
//! buffer descriptors cross a Unix socket.

mod instance;
mod page;
mod present;

pub(crate) use page::WpePage;

use crate::engine::{Events, Menu, MenuItem, PageEvent};
use crate::{Ask, Playback, Started};
use anyhow::{Context as _, Result};
use present::{Buffer, Frame, Outbox, Reader, Surface};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read as _;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

/// The helper the build made (development runs).
const BUILT_HELPER: &str = env!("GATE_WPE_HELPER");

/// The helper: beside the executable in a packaged Gate, else the one built.
fn helper_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("gate-wpe")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(BUILT_HELPER))
}

/// Where WebKit keeps the profile's cookies, cache and storage.
static PROFILE: OnceLock<PathBuf> = OnceLock::new();

/// The running helper, started with the first page.
static WORKER: Mutex<Option<Arc<Worker>>> = Mutex::new(None);

/// Starts the engine: claims the profile for this process (or hands this
/// launch to the process that has it).
pub(crate) fn init(profile: &Path, cx: &mut gpui::App) -> Result<Started> {
    let dir = profile.join("wpe");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    let Some(relaunches) = instance::claim(&dir, &args)? else {
        return Ok(Started::Elsewhere);
    };
    let _ = PROFILE.set(dir);
    cx.spawn(async move |cx| {
        while let Ok(args) = relaunches.recv().await {
            let handler = RELAUNCH.with_borrow(Clone::clone);
            if let Some(handler) = handler {
                cx.update(|cx| handler(args, cx));
            }
        }
    })
    .detach();
    Ok(Started::Here)
}

thread_local! {
    static RELAUNCH: RefCell<Option<Rc<dyn Fn(Vec<String>, &mut gpui::App)>>> = const { RefCell::new(None) };
}

/// See [`crate::on_relaunch`].
pub(crate) fn on_relaunch(handler: impl Fn(Vec<String>, &mut gpui::App) + 'static) {
    RELAUNCH.set(Some(Rc::new(handler)));
}

/// Stops the helper, letting WebKit write what it still holds.
pub(crate) fn shutdown() {
    let worker = WORKER.lock().unwrap().take();
    if let Some(worker) = worker {
        let _ = worker.outbox.send(0, json!({"cmd":"shutdown"}));
        let child = worker.child.lock().unwrap().take();
        if let Some(mut child) = child {
            for _ in 0..60 {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        worker.outbox.close();
    }
    if let Some(dir) = PROFILE.get() {
        instance::release(dir);
    }
}

/// What the view last heard of a page, to report only changes.
#[derive(Default, PartialEq)]
struct State {
    url: String,
    title: String,
    loading: bool,
    can_back: bool,
    can_forward: bool,
    progress: f64,
}

/// One page as the helper's reader sees it.
pub(super) struct Route {
    events: Events,
    surface: Mutex<Surface>,
    state: Mutex<State>,
    playback: Mutex<Playback>,
    /// The page's zoom level (1: none).
    zoom: Mutex<f64>,
}

impl Route {
    fn send(&self, event: PageEvent) {
        let _ = self.events.try_send(event);
    }
}

pub(super) struct Worker {
    outbox: Outbox,
    routes: Arc<Mutex<HashMap<u32, Weak<Route>>>>,
    next_id: AtomicU32,
    child: Arc<Mutex<Option<Child>>>,
    stopped: Arc<AtomicBool>,
}

impl Worker {
    pub(super) fn send(&self, id: u32, command: Value) {
        let _ = self.outbox.send(id, command);
    }

    /// Adds a page; `None` once the helper stopped.
    fn route(&self, events: Events) -> Option<(u32, Arc<Route>)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let route = Arc::new(Route {
            events,
            surface: Mutex::default(),
            state: Mutex::default(),
            playback: Mutex::default(),
            zoom: Mutex::new(1.0),
        });
        let mut routes = self.routes.lock().unwrap();
        // A helper that stopped before the route was in place would never
        // tell this page.
        if self.stopped.load(Ordering::Relaxed) {
            return None;
        }
        routes.insert(id, Arc::downgrade(&route));
        Some((id, route))
    }

    pub(super) fn forget(&self, id: u32) {
        self.routes.lock().unwrap().remove(&id);
    }
}

/// A new page on the running helper, starting one (for `gpu`, the GPU the
/// window draws with) when none runs.
pub(super) fn add_page(events: Events, gpu: Option<gpui::DmabufTarget>) -> (Arc<Worker>, u32, Arc<Route>) {
    let mut current = WORKER.lock().unwrap();
    if let Some(worker) = current.as_ref().filter(|w| !w.stopped.load(Ordering::Relaxed)) {
        if let Some((id, route)) = worker.route(events.clone()) {
            return (worker.clone(), id, route);
        }
    }
    let worker = start(gpu);
    *current = Some(worker.clone());
    let (id, route) = worker.route(events).expect("a new helper takes pages");
    (worker, id, route)
}

/// Starts a helper in the background. Commands wait in its outbox until it
/// runs; if it never does, its pages hear that it stopped.
fn start(gpu: Option<gpui::DmabufTarget>) -> Arc<Worker> {
    let (outbox, commands) = present::outbox();
    let worker = Arc::new(Worker {
        outbox: outbox.clone(),
        routes: Arc::default(),
        next_id: AtomicU32::new(1),
        child: Arc::default(),
        stopped: Arc::default(),
    });
    let routes = worker.routes.clone();
    let child_slot = worker.child.clone();
    let stopped = worker.stopped.clone();
    let profile = PROFILE.get().cloned();
    let spawned = std::thread::Builder::new().name("gate-wpe-commands".into()).spawn(move || {
        let (child, socket) = match spawn(profile.as_deref(), gpu) {
            Ok(started) => started,
            Err(error) => {
                tracing::error!("WPE WebKit could not start: {error:#}");
                helper_stopped(&routes, &stopped);
                return;
            }
        };
        *child_slot.lock().unwrap() = Some(child);
        let reading = socket.try_clone().map(|socket| {
            let routes = routes.clone();
            let outbox = outbox.clone();
            let stopped = stopped.clone();
            std::thread::Builder::new().name("gate-wpe-frames".into()).spawn(move || read(socket, &routes, &outbox, &stopped))
        });
        if !matches!(reading, Ok(Ok(_))) {
            tracing::error!("WPE WebKit: no thread to read the helper");
            helper_stopped(&routes, &stopped);
            return;
        }
        // This thread writes the helper's commands from now on.
        present::write_commands(socket, commands);
    });
    if let Err(error) = spawned {
        tracing::error!("WPE WebKit: no thread to start the helper: {error}");
        worker.stopped.store(true, Ordering::Relaxed);
    }
    worker
}

/// Starts the helper and waits for it to say it is ready.
fn spawn(profile: Option<&Path>, gpu: Option<gpui::DmabufTarget>) -> Result<(Child, UnixStream)> {
    let (socket, theirs) = UnixStream::pair()?;
    let mut command = Command::new(helper_path());
    if let Some(profile) = profile {
        command.arg("--profile").arg(profile);
    }
    // `GATE_WPE_FRAMES=shm` keeps frames in shared memory, rendered by the
    // CPU; `readback` renders them on the GPU and reads them back.
    let frames = std::env::var("GATE_WPE_FRAMES");
    if let Some(gpu) = gpu.filter(|_| frames.as_deref() != Ok("shm")) {
        command.arg("--render-node").arg(&gpu.render_node);
        // WebKit renders in one of these layouts where it can, so GPUI imports
        // frames without copies; the helper reads the others back.
        if !gpu.modifiers.is_empty() && frames.as_deref() != Ok("readback") {
            let modifiers: Vec<_> = gpu.modifiers.iter().map(|m| format!("{m:x}")).collect();
            command.arg("--modifiers").arg(modifiers.join(","));
        }
    }
    let input = OwnedFd::from(theirs.try_clone()?);
    let mut child = command
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(OwnedFd::from(theirs)))
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("running {}", helper_path().display()))?;
    // The command still holds the helper's end: closed, a helper that exits
    // ends the wait for it at once.
    drop(command);
    let mut startup = socket.try_clone()?;
    startup.set_read_timeout(Some(Duration::from_secs(15)))?;
    match read_startup(&mut startup) {
        Ok(ready) => {
            tracing::info!("WPE WebKit: {ready}");
            startup.set_read_timeout(None)?;
            Ok((child, socket))
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

/// The helper's first packet: `R` with what it runs on, or `E` with why it
/// cannot.
fn read_startup(socket: &mut UnixStream) -> Result<String> {
    let mut header = [0u8; 9];
    socket.read_exact(&mut header).context("the helper did not start")?;
    let length = u32::from_le_bytes(header[5..9].try_into().unwrap()) as usize;
    anyhow::ensure!(length <= 16384, "invalid startup packet");
    let mut message = vec![0; length];
    socket.read_exact(&mut message)?;
    let text = String::from_utf8_lossy(&message).into_owned();
    match header[0] {
        b'R' => Ok(text),
        b'E' => anyhow::bail!("{text}"),
        _ => anyhow::bail!("invalid startup packet"),
    }
}

/// Every page hears that the helper is gone; the next page starts another.
fn helper_stopped(routes: &Mutex<HashMap<u32, Weak<Route>>>, stopped: &AtomicBool) {
    let routes = routes.lock().unwrap();
    stopped.store(true, Ordering::Relaxed);
    for route in routes.values().filter_map(Weak::upgrade) {
        route.surface.lock().unwrap().vsync = false;
        route.send(PageEvent::Crashed);
    }
}

/// Reads the helper's packets until it goes.
fn read(socket: UnixStream, routes: &Mutex<HashMap<u32, Weak<Route>>>, outbox: &Outbox, stopped: &AtomicBool) {
    let connection = socket.try_clone().ok();
    let mut reader = Reader::new(socket);
    let result = (|| -> std::io::Result<()> {
        loop {
            let (kind, id, data) = reader.next()?;
            let route = routes.lock().unwrap().get(&id).and_then(Weak::upgrade);
            let Some(route) = route else {
                // A closed page: still take the descriptor it sent.
                if kind == b'B' {
                    reader.take_fd()?;
                }
                continue;
            };
            packet(&route, outbox, id, kind, &data, &mut reader)?;
        }
    })();
    if let Err(error) = result
        && error.kind() != std::io::ErrorKind::UnexpectedEof
    {
        tracing::warn!("WPE helper connection failed: {error}");
    }
    if let Some(connection) = connection {
        let _ = connection.shutdown(std::net::Shutdown::Both);
    }
    helper_stopped(routes, stopped);
}

fn packet(route: &Route, outbox: &Outbox, page: u32, kind: u8, data: &[u8], reader: &mut Reader) -> std::io::Result<()> {
    let json = || serde_json::from_slice::<Value>(data).ok();
    let text = || String::from_utf8_lossy(data).into_owned();
    match kind {
        b'B' => {
            let fd = reader.take_fd()?;
            if let Some((id, buffer)) = json().and_then(|info| Buffer::parse(&info, fd)) {
                route.surface.lock().unwrap().add_buffer(id, buffer);
            }
        }
        b'X' => {
            if let Some(id) = json().and_then(|v| v["buffer"].as_u64()) {
                route.surface.lock().unwrap().forget_buffer(id as u32);
            }
        }
        b'F' => {
            let frame = json().as_ref().and_then(Frame::parse);
            let frame = frame.ok_or_else(|| std::io::Error::other("invalid WPE frame"))?;
            route.surface.lock().unwrap().push_frame(frame, outbox, page);
            route.send(PageEvent::Repaint);
        }
        b'V' => {
            let on = json().is_some_and(|v| v["on"].as_bool() == Some(true));
            route.surface.lock().unwrap().vsync = on;
            // The paint after starts feeding vblanks.
            if on {
                route.send(PageEvent::Repaint);
            }
        }
        b'K' => route.send(PageEvent::Cursor(present::cursor_style(&text()))),
        b'S' => {
            if let Some(value) = json() {
                state(route, outbox, page, &value);
            }
        }
        b'G' => route.send(PageEvent::Favicon(favicon(data))),
        b'A' => route.send(PageEvent::Ask(json().as_ref().and_then(ask))),
        b'W' => {
            if let Some(v) = json() {
                route.send(PageEvent::Media {
                    camera: v["camera"].as_bool().unwrap_or(false),
                    microphone: v["microphone"].as_bool().unwrap_or(false),
                });
            }
        }
        b'P' => {
            let playback = match data {
                b"playing" => Playback::Playing,
                b"paused" => Playback::Paused,
                _ => Playback::None,
            };
            *route.playback.lock().unwrap() = playback;
            route.send(PageEvent::Playback(playback));
        }
        b'T' => route.send(PageEvent::Crashed),
        b'U' => route.send(PageEvent::Fullscreen(json().is_some_and(|v| v["on"].as_bool() == Some(true)))),
        b'N' => {
            if let Some(v) = json()
                && let Some(url) = v["url"].as_str()
            {
                route.send(PageEvent::OpenTab { url: url.to_string(), background: v["background"].as_bool().unwrap_or(false) });
            }
        }
        b'C' => route.send(PageEvent::Clipboard(text())),
        b'M' => route.send(PageEvent::Menu(json().as_ref().and_then(menu))),
        _ => {}
    }
    Ok(())
}

/// Reports what changed in the page's state, and keeps each site's zoom.
fn state(route: &Route, outbox: &Outbox, page: u32, value: &Value) {
    let new = State {
        url: value["url"].as_str().unwrap_or_default().to_string(),
        title: value["title"].as_str().unwrap_or_default().to_string(),
        loading: value["loading"].as_bool().unwrap_or(false),
        can_back: value["can_back"].as_bool().unwrap_or(false),
        can_forward: value["can_forward"].as_bool().unwrap_or(false),
        progress: value["progress"].as_f64().unwrap_or(0.0),
    };
    let mut old = route.state.lock().unwrap();
    if new == *old {
        return;
    }
    if new.url != old.url && !new.url.is_empty() {
        if page::host(&new.url) != page::host(&old.url) {
            let level = page::site_zoom(&new.url);
            *route.zoom.lock().unwrap() = level;
            let _ = outbox.send(page, json!({"cmd":"zoom","level":level}));
        }
        route.send(PageEvent::Url(new.url.clone()));
    }
    if new.title != old.title {
        route.send(PageEvent::Title(new.title.clone()));
    }
    if (new.loading, new.can_back, new.can_forward) != (old.loading, old.can_back, old.can_forward) {
        route.send(PageEvent::Loading { loading: new.loading, can_go_back: new.can_back, can_go_forward: new.can_forward });
    }
    if new.progress != old.progress {
        route.send(PageEvent::Progress(new.progress));
    }
    *old = new;
}

/// A favicon packet's pixels: width, height, then premultiplied BGRA.
fn favicon(data: &[u8]) -> Option<Arc<gpui::RenderImage>> {
    let width = u32::from_le_bytes(data.get(0..4)?.try_into().ok()?);
    let height = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?);
    let mut pixels = data.get(8..)?.to_vec();
    // GPUI blends straight alpha.
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        if alpha > 0 && alpha < 255 {
            for c in &mut pixel[..3] {
                *c = ((*c as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
    let buffer = image::RgbaImage::from_raw(width, height, pixels)?;
    Some(Arc::new(gpui::RenderImage::new([image::Frame::new(buffer)])))
}

/// The select menu an `M` packet shows; none for `null`.
fn menu(v: &Value) -> Option<Menu> {
    let items = v["items"]
        .as_array()?
        .iter()
        .map(|item| MenuItem {
            label: item["label"].as_str().unwrap_or_default().to_string(),
            enabled: item["enabled"].as_bool().unwrap_or(false),
            selected: item["selected"].as_bool().unwrap_or(false),
            group: item["group"].as_bool().unwrap_or(false),
        })
        .collect();
    Some(Menu {
        at: gpui::point(v["x"].as_f64()? as f32, v["y"].as_f64()? as f32),
        width: v["width"].as_f64().unwrap_or(0.0) as f32,
        items,
    })
}

/// The question an `A` packet asks; none for `null`.
fn ask(v: &Value) -> Option<Ask> {
    let message = || v["message"].as_str().unwrap_or_default().to_string();
    Some(match v["kind"].as_str()? {
        "alert" => Ask::Alert { message: message() },
        "confirm" => Ask::Confirm { message: message() },
        "prompt" => Ask::Prompt { message: message(), default: v["default"].as_str().unwrap_or_default().to_string() },
        "leave" => Ask::Leave { reload: false },
        "permission" => Ask::Permission {
            origin: v["origin"].as_str().unwrap_or_default().to_string(),
            what: v["what"].as_array()?.iter().filter_map(|w| permission_name(w.as_str()?)).collect(),
        },
        _ => return None,
    })
}

fn permission_name(name: &str) -> Option<&'static str> {
    const NAMES: &[&str] = &["camera", "microphone", "your screen", "location", "notifications", "clipboard", "protected media", "storage access"];
    NAMES.iter().copied().find(|n| *n == name)
}
