//! One Gate per profile on Windows: the first launch listens on a named pipe
//! named for the profile; later launches hand it their arguments there and
//! exit (as `wpe/instance.rs` does with a socket).

use std::io::{Read as _, Write as _};
use std::os::windows::io::FromRawHandle as _;
use std::path::Path;
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};
use windows::core::HSTRING;

/// The profile's pipe, named for its directory.
fn pipe_name(dir: &Path) -> String {
    use sha2::{Digest as _, Sha256};
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let hash = Sha256::digest(dir.as_os_str().as_encoded_bytes());
    let name: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!(r"\\.\pipe\gate-{name}")
}

/// Hands `args` to the Gate listening on `name`, if one does.
fn hand_over(name: &str, args: &[String]) -> bool {
    let Ok(mut pipe) = std::fs::OpenOptions::new().write(true).open(name) else { return false };
    let message = serde_json::to_vec(args).unwrap_or_default();
    pipe.write_all(&message).is_ok()
}

fn listen(name: &str, first: bool) -> Option<HANDLE> {
    let mut mode = PIPE_ACCESS_INBOUND;
    if first {
        mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: plain Win32 call; the name outlives it.
    let handle = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(name),
            mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            0,
            64 * 1024,
            0,
            None,
        )
    };
    (!handle.is_invalid()).then_some(handle)
}

/// Claims the profile in `dir`: the channel later launches' arguments arrive
/// on, or `None` when another process has it and took `args`.
pub(super) fn claim(dir: &Path, args: &[String]) -> std::io::Result<Option<async_channel::Receiver<Vec<String>>>> {
    let name = pipe_name(dir);
    if hand_over(&name, args) {
        return Ok(None);
    }
    let Some(first) = listen(&name, true) else {
        // Another launch claimed it a moment ago.
        if hand_over(&name, args) {
            return Ok(None);
        }
        return Err(std::io::Error::last_os_error());
    };
    let (sender, receiver) = async_channel::unbounded();
    // HANDLE holds a raw pointer; the thread is its only user from here.
    let first = first.0 as usize;
    std::thread::Builder::new().name("gate-instance".into()).spawn(move || {
        let mut next = Some(HANDLE(first as *mut _));
        loop {
            let Some(handle) = next.take().or_else(|| listen(&name, false)) else { break };
            // SAFETY: `handle` is a pipe this thread created.
            let connected = unsafe { ConnectNamedPipe(handle, None) };
            if let Err(error) = connected
                && error.code() != ERROR_PIPE_CONNECTED.to_hresult()
            {
                // SAFETY: closing the pipe this thread created.
                drop(unsafe { std::fs::File::from_raw_handle(handle.0) });
                continue;
            }
            // SAFETY: the File takes over the handle and closes it.
            let pipe = unsafe { std::fs::File::from_raw_handle(handle.0) };
            let mut message = Vec::new();
            if pipe.take(1 << 20).read_to_end(&mut message).is_err() {
                continue;
            }
            if let Ok(args) = serde_json::from_slice::<Vec<String>>(&message)
                && sender.send_blocking(args).is_err()
            {
                break;
            }
        }
    })?;
    Ok(Some(receiver))
}
