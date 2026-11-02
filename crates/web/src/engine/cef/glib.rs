//! Runs glib's default main context from our pump, woken by its own file
//! descriptors.
//!
//! Chromium on Linux watches its X11 connection (clipboard, selections)
//! through glib's default context, which a Chromium-owned message loop would
//! iterate. With CEF's external pump nothing does unless we do. A clipboard
//! read is a chain of X11 round trips, each waiting on that dispatch, so it
//! must happen as soon as an fd is ready, not on a timer.
//!
//! The main thread owns the context: it dispatches what is ready, then asks
//! glib which fds (and timeout) to wait on next. A watcher thread polls those
//! and wakes the pump, which dispatches again on the main thread.

use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

/// glib's `GPollFD`, laid out as `struct pollfd` on Unix.
#[repr(C)]
#[derive(Clone, Copy)]
struct PollFd {
    fd: i32,
    events: u16,
    revents: u16,
}

#[link(name = "glib-2.0")]
unsafe extern "C" {
    fn g_main_context_default() -> *mut c_void;
    fn g_main_context_acquire(context: *mut c_void) -> i32;
    fn g_main_context_release(context: *mut c_void);
    fn g_main_context_prepare(context: *mut c_void, priority: *mut i32) -> i32;
    fn g_main_context_query(context: *mut c_void, max_priority: i32, timeout: *mut i32, fds: *mut PollFd, n_fds: i32) -> i32;
    fn g_main_context_check(context: *mut c_void, max_priority: i32, fds: *mut PollFd, n_fds: i32) -> i32;
    fn g_main_context_dispatch(context: *mut c_void);
}

/// What the watcher waits on next, and the eventfd that hands it over.
struct Watch {
    next: Mutex<Option<(Vec<PollFd>, i32)>>,
    armed: i32,
}

static WATCH: OnceLock<Watch> = OnceLock::new();

/// Starts the watcher; `wake` runs on its thread when glib has work.
pub(crate) fn start(wake: impl Fn() + Send + 'static) {
    // SAFETY: plain eventfd creation.
    let armed = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
    if armed < 0 || WATCH.set(Watch { next: Mutex::new(None), armed }).is_err() {
        return;
    }
    std::thread::Builder::new()
        .name("glib-watch".into())
        .spawn(move || watch(wake))
        .expect("spawning the glib watcher");
}

fn watch(wake: impl Fn()) {
    let w = WATCH.get().unwrap();
    loop {
        // Wait to be armed with a fresh set.
        let mut count = 0u64;
        // SAFETY: reading 8 bytes from our eventfd.
        if unsafe { libc::read(w.armed, (&raw mut count).cast(), 8) } != 8 {
            continue;
        }
        let Some((mut fds, timeout)) = w.next.lock().unwrap().take() else { continue };
        // One of Chromium's sources asks to be polled again at once (timeout
        // 0) even when it has nothing; honoring that spins. Wait for an fd
        // or a timer actually due later; the pump's regular round (at least
        // every 33ms) dispatches the rest.
        let timeout = if timeout == 0 { -1 } else { timeout };
        // Also watch the eventfd: a newer set replaces this one.
        fds.push(PollFd { fd: w.armed, events: libc::POLLIN as u16, revents: 0 });
        // SAFETY: `PollFd` matches `struct pollfd`.
        let ready = unsafe { libc::poll(fds.as_mut_ptr().cast(), fds.len() as libc::nfds_t, timeout) };
        let rearmed = fds.last().is_some_and(|f| f.revents != 0);
        if ready >= 0 && !rearmed {
            wake();
        }
    }
}

/// Dispatches everything glib has ready (main thread), then arms the
/// watcher for what it waits on next.
pub(crate) fn dispatch() {
    let Some(w) = WATCH.get() else { return };
    // SAFETY: the default context, used on the main thread only.
    unsafe {
        let context = g_main_context_default();
        if g_main_context_acquire(context) == 0 {
            return;
        }
        let mut fds: Vec<PollFd> = Vec::new();
        let mut timeout = -1;
        // Bounded: sources that stay ready can't hold the main thread.
        for _ in 0..16 {
            let mut priority = 0;
            g_main_context_prepare(context, &mut priority);
            let (wanted, t) = query(context, priority);
            fds = wanted;
            timeout = t;
            libc::poll(fds.as_mut_ptr().cast(), fds.len() as libc::nfds_t, 0);
            if g_main_context_check(context, priority, fds.as_mut_ptr(), fds.len() as i32) == 0 {
                break;
            }
            g_main_context_dispatch(context);
            timeout = 0;
        }
        g_main_context_release(context);
        for f in &mut fds {
            f.revents = 0;
        }
        *w.next.lock().unwrap() = Some((fds, timeout));
        let one = 1u64;
        libc::write(w.armed, (&raw const one).cast(), 8);
    }
}

/// The fds and timeout glib wants polled at `priority`.
unsafe fn query(context: *mut c_void, priority: i32) -> (Vec<PollFd>, i32) {
    let mut timeout = -1;
    let mut fds = vec![PollFd { fd: -1, events: 0, revents: 0 }; 8];
    loop {
        // SAFETY: `fds` has room for `fds.len()` entries.
        let n = unsafe { g_main_context_query(context, priority, &mut timeout, fds.as_mut_ptr(), fds.len() as i32) };
        if n as usize <= fds.len() {
            fds.truncate(n as usize);
            return (fds, timeout);
        }
        fds.resize(n as usize, PollFd { fd: -1, events: 0, revents: 0 });
    }
}
