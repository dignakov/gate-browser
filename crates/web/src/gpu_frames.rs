//! Pages' frames as GPU buffers (CEF's shared textures, Linux): Chromium
//! hands each frame over as a dma-buf, which the GPU copies into the page's
//! atlas tile before the callback returns the buffer to Chromium's pool.
//!
//! Off by default (the `gpu_frames` setting). With NVIDIA's driver 580,
//! Chromium's EGL can't import its own buffers (EGL_BAD_MATCH); CEF fixed
//! its side in 156 (CEF #4237), and NVIDIA frames can still arrive half
//! drawn until Chromium 158 (CEF #4313).

use crate::frames::{Frames, Surface, regions};
use anyhow::{Context as _, Result, bail, ensure};
use cef::{AcceleratedPaintInfo, ColorType, Rect};
use gpui::{AtlasTileUpdate, DevicePixels, DmabufId, DmabufImport, point, size};
use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicBool, Ordering};

/// DRM_FORMAT_ARGB8888 ("AR24"): bytes B, G, R, A, as CEF's BGRA frames.
const AR24: u32 = u32::from_le_bytes(*b"AR24");
/// Imports not used for this many frames are dropped (Chromium cycles a
/// small pool of buffers).
const STALE_AFTER: u64 = 240;

/// Set once a GPU frame could not be used: views fall back to CPU frames.
static BROKEN: AtomicBool = AtomicBool::new(false);

pub(crate) fn broken() -> bool {
    BROKEN.load(Ordering::Relaxed)
}

/// A view's imports of Chromium's frame buffers.
#[derive(Default)]
pub(crate) struct GpuFrames {
    imports: Vec<Imported>,
    frame: u64,
}

struct Imported {
    /// The buffer's identity: its dma-buf inode.
    key: (u64, u64),
    import: DmabufImport,
    used: u64,
}

impl GpuFrames {
    /// Copies a frame into `surface`'s tile, finished on the GPU before this
    /// returns (CEF reuses the buffer then). On failure GPU frames are marked
    /// broken for every view.
    pub(crate) fn take(&mut self, frames: &Frames, surface: &mut Surface, info: &AcceleratedPaintInfo, dirty: &[Rect]) -> bool {
        match self.copy(frames, surface, info, dirty) {
            Ok(()) => true,
            Err(error) => {
                if !BROKEN.swap(true, Ordering::Relaxed) {
                    tracing::warn!("GPU page frames don't work here ({error:#}); using CPU frames");
                }
                false
            }
        }
    }

    fn copy(&mut self, frames: &Frames, surface: &mut Surface, info: &AcceleratedPaintInfo, dirty: &[Rect]) -> Result<()> {
        ensure!(info.plane_count == 1, "{} planes", info.plane_count);
        ensure!(info.format == ColorType::BGRA_8888, "format {:?}", info.format);
        let visible = &info.extra.visible_rect;
        ensure!(visible.width > 0 && visible.height > 0, "empty frame");
        self.frame += 1;
        let wanted = size(DevicePixels(visible.width), DevicePixels(visible.height));
        let (image, whole) = frames.prepare(surface, wanted)?;
        let buffer = self.import(frames, info)?;
        for region in regions(dirty, wanted, whole) {
            let update = AtlasTileUpdate::Dmabuf {
                buffer,
                origin: point(DevicePixels(visible.x + region.origin.x.0), DevicePixels(visible.y + region.origin.y.0)),
            };
            if !frames.atlas.update_image(&image, 0, region, update, None)? {
                bail!("the atlas took no dma-buf copy");
            }
        }
        // Done before the buffer goes back to Chromium.
        frames.atlas.flush(true);
        self.imports.retain(|i| self.frame - i.used < STALE_AFTER);
        Ok(())
    }

    /// The frame's buffer, imported once per buffer of Chromium's pool.
    fn import(&mut self, frames: &Frames, info: &AcceleratedPaintInfo) -> Result<DmabufId> {
        let plane = &info.planes[0];
        // SAFETY: CEF keeps the descriptor open for the callback.
        let fd = unsafe { BorrowedFd::borrow_raw(plane.fd) };
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        ensure!(unsafe { libc::fstat(plane.fd, &mut stat) } == 0, "fstat on the frame's dma-buf failed");
        let key = (stat.st_dev, stat.st_ino);
        if let Some(found) = self.imports.iter_mut().find(|i| i.key == key) {
            found.used = self.frame;
            return Ok(found.import.id());
        }
        let coded = &info.extra.coded_size;
        let import = frames
            .atlas
            .import_dmabuf(&gpui::Dmabuf {
                fd,
                width: coded.width as u32,
                height: coded.height as u32,
                fourcc: AR24,
                modifier: info.modifier,
                offset: plane.offset as u32,
                stride: plane.stride,
            })
            .context("importing the frame's dma-buf")?;
        let id = import.id();
        self.imports.push(Imported { key, import, used: self.frame });
        Ok(id)
    }
}
