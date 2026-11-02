//! A page's frames into its atlas tile, as CEF's paint callbacks deliver
//! them: CPU frames are uploaded straight from CEF's buffer (no copy of our
//! own), GPU frames (Linux, see `gpu_frames`) are copied on the GPU. Painting
//! then only draws the tile.

use anyhow::{Result, bail, ensure};
use cef::Rect;
use gpui::{AtlasHandle, AtlasTileUpdate, Bounds, DevicePixels, RenderImage, Size, point, size};
use std::sync::Arc;

/// One painted surface: the page, or a popup (a `<select>`'s list).
#[derive(Default)]
pub(crate) struct Surface {
    /// Its atlas tile's image; replaced when the size changes.
    pub image: Option<Arc<RenderImage>>,
}

/// A view's way into its window's atlas.
pub(crate) struct Frames {
    pub atlas: AtlasHandle,
}

impl Frames {
    /// `surface`'s image at `wanted`, its tile allocated, and whether all of
    /// it needs new content (new, resized, or dropped by the atlas).
    pub(crate) fn prepare(&self, surface: &mut Surface, wanted: Size<DevicePixels>) -> Result<(Arc<RenderImage>, bool)> {
        let mut whole = false;
        if surface.image.as_ref().map(|i| i.size(0)) != Some(wanted) {
            self.release(surface);
            surface.image = Some(Arc::new(RenderImage::blank(wanted)));
            whole = true;
        }
        let image = surface.image.clone().expect("set above");
        whole |= self.atlas.insert_blank_image(&image, 0)?;
        Ok((image, whole))
    }

    /// Frees a surface's tile.
    pub(crate) fn release(&self, surface: &mut Surface) {
        if let Some(image) = surface.image.take() {
            self.atlas.remove_image(&image, 0);
        }
    }

    /// Uploads a CPU frame's changed regions straight from CEF's buffer
    /// (`width` × `height` BGRA, valid for the callback): the atlas copies
    /// them before this returns.
    pub(crate) fn take_pixels(&self, surface: &mut Surface, dirty: &[Rect], buffer: *const u8, width: i32, height: i32) -> Result<()> {
        ensure!(!buffer.is_null() && width > 0 && height > 0, "empty frame");
        let wanted = size(DevicePixels(width), DevicePixels(height));
        let (image, whole) = self.prepare(surface, wanted)?;
        let stride = width as usize * 4;
        // SAFETY: CEF passes a `width * height` BGRA buffer valid for this call.
        let pixels = unsafe { std::slice::from_raw_parts(buffer, stride * height as usize) };
        for region in regions(dirty, wanted, whole) {
            let start = region.origin.y.0 as usize * stride + region.origin.x.0 as usize * 4;
            let update = AtlasTileUpdate::Pixels { bytes: &pixels[start..], bytes_per_row: stride as u32 };
            if !self.atlas.update_image(&image, 0, region, update, None)? {
                bail!("the atlas took no upload");
            }
        }
        Ok(())
    }
}

/// The regions of a frame to copy: all of it, or its damage within it.
pub(crate) fn regions(dirty: &[Rect], frame: Size<DevicePixels>, whole: bool) -> Vec<Bounds<DevicePixels>> {
    let all = Bounds::new(point(DevicePixels(0), DevicePixels(0)), frame);
    if whole || dirty.is_empty() {
        return vec![all];
    }
    dirty
        .iter()
        .map(|r| Bounds::new(point(DevicePixels(r.x), DevicePixels(r.y)), size(DevicePixels(r.width), DevicePixels(r.height))).intersect(&all))
        .filter(|r| r.size.width.0 > 0 && r.size.height.0 > 0)
        .collect()
}
