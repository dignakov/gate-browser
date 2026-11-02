//! Runs CEF's message loop work on GPUI's main thread.
//!
//! CEF asks for work with a delay (`on_schedule_message_pump_work`, any
//! thread). We run it when due, and at least every [`MAX_DELAY`] as CEF's own
//! external-pump sample (cefclient's MainMessageLoopExternalPump) does, since
//! some work is never announced. After each round, views whose callbacks
//! queued events get them.
//!
//! glib's default context (Chromium's X11 watcher) is dispatched here too,
//! whenever its fds are ready (see [`crate::glib`]).

use gpui::{App, AsyncApp};
use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const MAX_DELAY: Duration = Duration::from_millis(1000 / 30);

enum Msg {
    /// CEF wants work done in this many ms (<= 0: now).
    Schedule(i64),
    /// A callback queued events for a view.
    Wake,
    /// glib has fds ready or a timeout due.
    Glib,
    Stop,
}

static SENDER: OnceLock<async_channel::Sender<Msg>> = OnceLock::new();

thread_local! {
    /// Views to hand queued events to after a round of work.
    static DRAINS: RefCell<Vec<Box<dyn Fn(&mut App) -> bool>>> = const { RefCell::new(Vec::new()) };
    /// Inside `do_message_loop_work`: CEF must not be re-entered.
    static WORKING: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn schedule(delay_ms: i64) {
    if let Some(sender) = SENDER.get() {
        let _ = sender.try_send(Msg::Schedule(delay_ms));
    }
}

pub(crate) fn wake() {
    if let Some(sender) = SENDER.get() {
        let _ = sender.try_send(Msg::Wake);
    }
}

pub(crate) fn stop() {
    if let Some(sender) = SENDER.get() {
        let _ = sender.try_send(Msg::Stop);
    }
}

/// Registers a view's drain: called on the main thread after CEF work with
/// the app; returns false once the view is gone.
pub(crate) fn add_drain(drain: impl Fn(&mut App) -> bool + 'static) {
    DRAINS.with_borrow_mut(|d| d.push(Box::new(drain)));
}

pub(crate) fn start(cx: &mut App) {
    let (tx, rx) = async_channel::unbounded();
    if SENDER.set(tx).is_err() {
        return;
    }
    crate::glib::start(|| {
        if let Some(sender) = SENDER.get() {
            let _ = sender.try_send(Msg::Glib);
        }
    });
    cx.spawn(async move |cx: &mut AsyncApp| {
        use futures::FutureExt as _;
        let mut due: Option<Instant> = None;
        loop {
            let now = Instant::now();
            let wait = due.map_or(MAX_DELAY, |at| at.saturating_duration_since(now).min(MAX_DELAY));
            let timer = cx.background_executor().timer(wait);
            futures::select_biased! {
                msg = rx.recv().fuse() => match msg {
                    Ok(Msg::Schedule(ms)) => {
                        let at = Instant::now() + Duration::from_millis(ms.max(0) as u64);
                        due = Some(due.map_or(at, |d| d.min(at)));
                    }
                    Ok(Msg::Wake) => {}
                    Ok(Msg::Glib) => crate::glib::dispatch(),
                    Ok(Msg::Stop) | Err(_) => break,
                },
                _ = timer.fuse() => due = Some(Instant::now()),
            }
            if due.is_some_and(|at| at <= Instant::now()) {
                due = None;
                work();
            }
            cx.update(drain);
        }
    })
    .detach();
}

fn work() {
    if WORKING.replace(true) {
        return;
    }
    cef::do_message_loop_work();
    WORKING.set(false);
    // CEF's work may have changed what glib waits on.
    crate::glib::dispatch();
}

fn drain(cx: &mut App) {
    // A drain may create views (and register drains): run the current set.
    let drains = DRAINS.take();
    let mut kept: Vec<_> = drains.into_iter().filter(|drain| drain(cx)).collect();
    DRAINS.with_borrow_mut(|added| {
        kept.append(added);
        *added = kept;
    });
}
