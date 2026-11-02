//! One Gate per profile: the first launch listens on the profile's socket;
//! later launches hand it their arguments there and exit.

use std::io::{Read as _, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// The profile's socket: in the user's runtime directory (socket paths are
/// short), named for the profile.
fn socket(dir: &Path) -> PathBuf {
    use sha2::{Digest as _, Sha256};
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let hash = Sha256::digest(dir.as_os_str().as_encoded_bytes());
    let name: String = hash[..8].iter().map(|b| format!("{b:02x}")).collect();
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    runtime.join(format!("gate-{name}.sock"))
}

/// Claims the profile in `dir`: the channel later launches' arguments arrive
/// on, or `None` when another process has it and took `args`.
pub(super) fn claim(dir: &Path, args: &[String]) -> std::io::Result<Option<async_channel::Receiver<Vec<String>>>> {
    let path = socket(dir);
    if let Ok(mut other) = UnixStream::connect(&path) {
        let message = serde_json::to_vec(args).map_err(std::io::Error::other)?;
        other.write_all(&message)?;
        return Ok(None);
    }
    // Nothing listens: a socket left behind by a Gate that crashed.
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    let (sender, receiver) = async_channel::unbounded();
    std::thread::Builder::new().name("gate-instance".into()).spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let mut message = Vec::new();
            if stream.take(1 << 20).read_to_end(&mut message).is_err() {
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

/// Gives the profile up (at exit).
pub(super) fn release(dir: &Path) {
    let _ = std::fs::remove_file(socket(dir));
}
