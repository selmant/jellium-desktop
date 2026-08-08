use std::ffi::c_void;
use std::ptr::NonNull;

use thiserror::Error;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use jfn_gpu_paint::WindowTarget;

use crate::wl_state::FrameBuffer;

/// Success outcome of a present/enqueue. A `Skipped` is a deliberate no-op, not
/// a failure, so it must never be mapped to an `Err`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Present {
    Committed,
    Skipped,
}

#[derive(Debug, Error)]
pub(crate) enum PresentError {
    #[error("invalid frame dimensions: {0}x{1}")]
    BadDimensions(i32, i32),
    #[error("pixel buffer too small: have {have}, need {need}")]
    ShortBuffer { have: usize, need: usize },
    #[error("gpu paint failed: {0}")]
    Gpu(#[from] jfn_gpu_paint::SurfaceLost),
    #[error("shm buffer allocation failed")]
    ShmAlloc,
    #[error("dmabuf buffer creation failed")]
    DmabufCreate,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) struct ViewportState {
    pub(crate) lw: i32,
    pub(crate) lh: i32,
    pub(crate) pw: i32,
    pub(crate) ph: i32,
}

pub(crate) struct LayerSurface {
    conn: Connection,
    surface: WlSurface,
    viewport: Option<WpViewport>,
}

impl LayerSurface {
    pub(crate) fn new(conn: Connection, surface: WlSurface, viewport: Option<WpViewport>) -> Self {
        Self {
            conn,
            surface,
            viewport,
        }
    }

    pub(crate) fn window_target(&self) -> Option<WindowTarget> {
        let display = NonNull::new(self.conn.backend().display_ptr().cast::<c_void>())?;
        let surface = NonNull::new(self.surface.id().as_ptr().cast::<c_void>())?;
        Some(WindowTarget::Wayland { display, surface })
    }

    pub(crate) fn attach_none(&self) {
        self.surface.attach(None, 0, 0);
    }

    pub(crate) fn set_viewport(&self, src_w: i32, src_h: i32, dst_w: i32, dst_h: i32) {
        let Some(viewport) = self.viewport.as_ref() else {
            return;
        };
        // A source belongs to the attached buffer. Callers that only have a
        // destination resize pass zero and deliberately leave that source
        // untouched; manufacturing a 1x1 source would stretch one pixel over
        // the layer if a popup forces an intermediate commit.
        if viewport_source(src_w, src_h).is_some() {
            viewport.set_source(0.0, 0.0, src_w as f64, src_h as f64);
        }
        if dst_w > 0 && dst_h > 0 {
            viewport.set_destination(dst_w, dst_h);
        }
    }

    pub(crate) fn present(&self, frame: FrameCommit<'_>) {
        self.set_viewport(frame.src_w, frame.src_h, frame.dst_w, frame.dst_h);
        frame.buf.attach_to(&self.surface);
        self.surface.damage_buffer(0, 0, frame.buf_w, frame.buf_h);
        self.surface.commit();
    }

    pub(crate) fn commit(&self) {
        self.surface.commit();
    }

    pub(crate) fn flush(&self) {
        let _ = self.conn.flush();
    }
}

fn viewport_source(src_w: i32, src_h: i32) -> Option<(i32, i32)> {
    (src_w > 0 && src_h > 0).then_some((src_w, src_h))
}

pub(crate) struct FrameCommit<'a> {
    buf: FrameBuffer<'a>,
    buf_w: i32,
    buf_h: i32,
    src_w: i32,
    src_h: i32,
    dst_w: i32,
    dst_h: i32,
}

impl<'a> FrameCommit<'a> {
    /// Clamps `src_*` to the buffer dimensions: a `wp_viewport` source larger
    /// than the attached buffer is a fatal protocol error that kills the client.
    pub(crate) fn new(
        buf: FrameBuffer<'a>,
        buf_w: i32,
        buf_h: i32,
        src_w: i32,
        src_h: i32,
        dst_w: i32,
        dst_h: i32,
    ) -> Self {
        Self {
            buf,
            buf_w,
            buf_h,
            src_w: src_w.min(buf_w),
            src_h: src_h.min(buf_h),
            dst_w,
            dst_h,
        }
    }
}

pub(crate) struct SurfaceRef {
    surface: WlSurface,
    viewport: Option<WpViewport>,
}

impl SurfaceRef {
    pub(crate) fn new(surface: WlSurface, viewport: Option<WpViewport>) -> Self {
        Self { surface, viewport }
    }

    pub(crate) fn as_arg(&self) -> &WlSurface {
        &self.surface
    }

    pub(crate) fn destroy(self) {
        if let Some(viewport) = self.viewport {
            viewport.destroy();
        }
        self.surface.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_only_reapply_does_not_invent_a_one_pixel_source() {
        assert_eq!(viewport_source(0, 0), None);
        assert_eq!(viewport_source(1920, 1080), Some((1920, 1080)));
    }
}
