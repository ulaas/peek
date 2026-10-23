//! Framebuffer with dirty-region tracking, shared between a protocol backend
//! (writer) and the UI / recorder (readers).
//!
//! Pixels are stored as `u32` in RGBA byte order (on little-endian hosts
//! `0xAABBGGRR`), which matches `egui::Color32` and RGBA8 textures, so the UI
//! can upload a sub-rectangle with a plain copy.

use parking_lot::{Mutex, MutexGuard};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Pack an opaque pixel.
#[inline(always)]
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    u32::from_le_bytes([r, g, b, 0xFF])
}

/// Pack a pixel with alpha.
#[inline(always)]
pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> u32 {
    u32::from_le_bytes([r, g, b, a])
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    #[inline]
    pub const fn right(&self) -> u32 {
        self.x + self.w
    }

    #[inline]
    pub const fn bottom(&self) -> u32 {
        self.y + self.h
    }

    #[inline]
    pub const fn area(&self) -> u64 {
        self.w as u64 * self.h as u64
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    pub fn union(&self, o: &Rect) -> Rect {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(
            x,
            y,
            self.right().max(o.right()) - x,
            self.bottom().max(o.bottom()) - y,
        )
    }

    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        if r <= x || b <= y {
            Rect::default()
        } else {
            Rect::new(x, y, r - x, b - y)
        }
    }

    /// True if the rects overlap or share an edge.
    pub fn touches(&self, o: &Rect) -> bool {
        self.x <= o.right() && o.x <= self.right() && self.y <= o.bottom() && o.y <= self.bottom()
    }

    pub fn clamp_to(&self, width: u32, height: u32) -> Rect {
        self.intersect(&Rect::new(0, 0, width, height))
    }
}

/// Accumulates damaged areas between two reads.
#[derive(Debug, Default)]
pub struct DirtyRegion {
    rects: Vec<Rect>,
    full: bool,
}

impl DirtyRegion {
    /// Above this many rects we merge everything into one bounding box;
    /// uploading a slightly larger area is cheaper than many tiny uploads.
    const MAX_RECTS: usize = 48;

    pub fn add(&mut self, r: Rect) {
        if self.full || r.is_empty() {
            return;
        }
        // Cheap coalescing against the last rect: decoders emit tiles in
        // scan order, so neighbours usually arrive back to back.
        if let Some(last) = self.rects.last_mut()
            && last.touches(&r)
        {
            let u = last.union(&r);
            if u.area() <= last.area() + r.area() + r.area() / 2 {
                *last = u;
                return;
            }
        }
        self.rects.push(r);
        if self.rects.len() > Self::MAX_RECTS {
            let bb = self.rects.iter().fold(Rect::default(), |a, r| a.union(r));
            self.rects.clear();
            self.rects.push(bb);
        }
    }

    pub fn mark_full(&mut self) {
        self.full = true;
        self.rects.clear();
    }

    pub fn is_empty(&self) -> bool {
        !self.full && self.rects.is_empty()
    }

    /// Take the accumulated region, clamped to the given size.
    pub fn take(&mut self, width: u32, height: u32) -> Vec<Rect> {
        let full = std::mem::take(&mut self.full);
        let rects = std::mem::take(&mut self.rects);
        if full {
            return vec![Rect::new(0, 0, width, height)];
        }
        let mut out: Vec<Rect> = rects
            .into_iter()
            .map(|r| r.clamp_to(width, height))
            .filter(|r| !r.is_empty())
            .collect();
        // If the damage covers most of its bounding box, send the box.
        if out.len() > 1 {
            let bb = out.iter().fold(Rect::default(), |a, r| a.union(r));
            let sum: u64 = out.iter().map(Rect::area).sum();
            if sum * 10 >= bb.area() * 7 {
                out = vec![bb];
            }
        }
        out
    }
}

/// A plain RGBA framebuffer.
#[derive(Clone)]
pub struct Framebuffer {
    width: u32,
    height: u32,
    data: Vec<u32>,
}

impl Framebuffer {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![rgb(0, 0, 0); width as usize * height as usize],
        }
    }

    #[inline]
    pub fn width(&self) -> u32 {
        self.width
    }

    #[inline]
    pub fn height(&self) -> u32 {
        self.height
    }

    #[inline]
    pub fn pixels(&self) -> &[u32] {
        &self.data
    }

    #[inline]
    pub fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.data
    }

    pub fn as_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.data)
    }

    /// Resize, keeping the overlapping top-left content.
    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.width && height == self.height {
            return;
        }
        let mut data = vec![rgb(0, 0, 0); width as usize * height as usize];
        let cw = self.width.min(width) as usize;
        for y in 0..self.height.min(height) as usize {
            let src = y * self.width as usize;
            let dst = y * width as usize;
            data[dst..dst + cw].copy_from_slice(&self.data[src..src + cw]);
        }
        self.width = width;
        self.height = height;
        self.data = data;
    }

    #[inline]
    pub fn row_mut(&mut self, y: u32, x: u32, w: u32) -> &mut [u32] {
        let start = y as usize * self.width as usize + x as usize;
        &mut self.data[start..start + w as usize]
    }

    #[inline]
    pub fn row(&self, y: u32, x: u32, w: u32) -> &[u32] {
        let start = y as usize * self.width as usize + x as usize;
        &self.data[start..start + w as usize]
    }

    pub fn fill_rect(&mut self, r: Rect, px: u32) {
        let r = r.clamp_to(self.width, self.height);
        for y in r.y..r.bottom() {
            self.row_mut(y, r.x, r.w).fill(px);
        }
    }

    /// Copy `src` (row-major, `r.w` pixels per row) into `r`.
    /// Parts of `r` outside the framebuffer are skipped.
    pub fn put_rect(&mut self, r: Rect, src: &[u32]) {
        let c = r.clamp_to(self.width, self.height);
        if c.is_empty() {
            return;
        }
        let ox = (c.x - r.x) as usize;
        for y in c.y..c.bottom() {
            let s = (y - r.y) as usize * r.w as usize + ox;
            self.row_mut(y, c.x, c.w)
                .copy_from_slice(&src[s..s + c.w as usize]);
        }
    }

    /// Copy the area of `dst` size from (`sx`,`sy`) to `dst`. Handles overlap.
    pub fn copy_rect(&mut self, sx: u32, sy: u32, dst: Rect) {
        let src = Rect::new(sx, sy, dst.w, dst.h).clamp_to(self.width, self.height);
        let dst = Rect::new(dst.x, dst.y, src.w, src.h).clamp_to(self.width, self.height);
        let (w, h) = (src.w.min(dst.w) as usize, src.h.min(dst.h));
        if w == 0 || h == 0 {
            return;
        }
        let stride = self.width as usize;
        let mut copy_row = |row: u32| {
            let s = (src.y + row) as usize * stride + src.x as usize;
            let d = (dst.y + row) as usize * stride + dst.x as usize;
            self.data.copy_within(s..s + w, d);
        };
        if dst.y > src.y {
            (0..h).rev().for_each(&mut copy_row);
        } else {
            (0..h).for_each(&mut copy_row);
        }
    }

    /// Copy a sub-rectangle out as tightly packed RGBA bytes.
    pub fn read_rect(&self, r: Rect, out: &mut Vec<u8>) {
        let r = r.clamp_to(self.width, self.height);
        out.clear();
        out.reserve(r.area() as usize * 4);
        for y in r.y..r.bottom() {
            out.extend_from_slice(bytemuck::cast_slice(self.row(y, r.x, r.w)));
        }
    }
}

struct FrameState {
    fb: Framebuffer,
    dirty: DirtyRegion,
}

type Waker = Arc<dyn Fn() + Send + Sync>;

/// A framebuffer shared between one writer and any number of readers.
///
/// Writers take a [`FrameWriter`], draw, and on drop the registered waker
/// (usually `egui::Context::request_repaint`) is called once.
pub struct SharedFrame {
    state: Mutex<FrameState>,
    waker: Mutex<Option<Waker>>,
    /// Bumped on every write batch; readers can use it to skip work.
    generation: AtomicU64,
}

impl SharedFrame {
    pub fn new(width: u32, height: u32) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FrameState {
                fb: Framebuffer::new(width, height),
                dirty: DirtyRegion::default(),
            }),
            waker: Mutex::new(None),
            generation: AtomicU64::new(0),
        })
    }

    pub fn set_waker(&self, w: impl Fn() + Send + Sync + 'static) {
        *self.waker.lock() = Some(Arc::new(w));
    }

    pub fn wake(&self) {
        let w = self.waker.lock().clone();
        if let Some(w) = w {
            w();
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn size(&self) -> (u32, u32) {
        let s = self.state.lock();
        (s.fb.width, s.fb.height)
    }

    pub fn write(&self) -> FrameWriter<'_> {
        FrameWriter {
            owner: self,
            g: Some(self.state.lock()),
            touched: false,
        }
    }

    /// Run `f` with the framebuffer and the damage accumulated since the
    /// last call. Returns `None` without calling `f` when nothing changed.
    pub fn read_dirty<R>(&self, f: impl FnOnce(&Framebuffer, &[Rect]) -> R) -> Option<R> {
        let mut s = self.state.lock();
        if s.dirty.is_empty() {
            return None;
        }
        let (w, h) = (s.fb.width, s.fb.height);
        let rects = s.dirty.take(w, h);
        Some(f(&s.fb, &rects))
    }

    /// Read the whole framebuffer without consuming damage.
    pub fn read<R>(&self, f: impl FnOnce(&Framebuffer) -> R) -> R {
        f(&self.state.lock().fb)
    }

    /// Mark everything dirty, e.g. after the UI lost its texture.
    pub fn invalidate(&self) {
        self.state.lock().dirty.mark_full();
        self.wake();
    }
}

/// Write access to a [`SharedFrame`]. Every drawing call marks damage.
pub struct FrameWriter<'a> {
    owner: &'a SharedFrame,
    g: Option<MutexGuard<'a, FrameState>>,
    touched: bool,
}

impl FrameWriter<'_> {
    #[inline]
    fn st(&mut self) -> &mut FrameState {
        self.touched = true;
        self.g.as_mut().expect("guard alive")
    }

    pub fn fb(&self) -> &Framebuffer {
        &self.g.as_ref().expect("guard alive").fb
    }

    /// Raw access; the caller must call [`Self::damage`] for what it changes.
    pub fn fb_mut(&mut self) -> &mut Framebuffer {
        &mut self.st().fb
    }

    pub fn damage(&mut self, r: Rect) {
        self.st().dirty.add(r);
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        let s = self.st();
        s.fb.resize(w, h);
        s.dirty.mark_full();
    }

    pub fn fill_rect(&mut self, r: Rect, px: u32) {
        let s = self.st();
        s.fb.fill_rect(r, px);
        s.dirty.add(r);
    }

    pub fn put_rect(&mut self, r: Rect, src: &[u32]) {
        let s = self.st();
        s.fb.put_rect(r, src);
        s.dirty.add(r);
    }

    pub fn copy_rect(&mut self, sx: u32, sy: u32, dst: Rect) {
        let s = self.st();
        s.fb.copy_rect(sx, sy, dst);
        s.dirty.add(dst);
    }
}

impl Drop for FrameWriter<'_> {
    fn drop(&mut self) {
        drop(self.g.take());
        if self.touched {
            self.owner.generation.fetch_add(1, Ordering::AcqRel);
            self.owner.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_rect_overlapping_down() {
        let mut fb = Framebuffer::new(4, 4);
        for (i, p) in fb.pixels_mut().iter_mut().enumerate() {
            *p = i as u32;
        }
        fb.copy_rect(0, 0, Rect::new(0, 1, 4, 3));
        assert_eq!(fb.row(1, 0, 4), &[0, 1, 2, 3]);
        assert_eq!(fb.row(3, 0, 4), &[8, 9, 10, 11]);
    }

    #[test]
    fn copy_rect_overlapping_right() {
        let mut fb = Framebuffer::new(4, 1);
        fb.pixels_mut().copy_from_slice(&[1, 2, 3, 4]);
        fb.copy_rect(0, 0, Rect::new(1, 0, 3, 1));
        assert_eq!(fb.pixels(), &[1, 1, 2, 3]);
    }

    #[test]
    fn dirty_merges_neighbours() {
        let mut d = DirtyRegion::default();
        d.add(Rect::new(0, 0, 16, 16));
        d.add(Rect::new(16, 0, 16, 16));
        assert_eq!(d.take(100, 100), vec![Rect::new(0, 0, 32, 16)]);
        assert!(d.is_empty());
    }

    #[test]
    fn dirty_keeps_far_apart_rects() {
        let mut d = DirtyRegion::default();
        d.add(Rect::new(0, 0, 4, 4));
        d.add(Rect::new(90, 90, 4, 4));
        assert_eq!(d.take(100, 100).len(), 2);
    }

    #[test]
    fn put_rect_clips() {
        let mut fb = Framebuffer::new(2, 2);
        fb.put_rect(Rect::new(1, 1, 2, 2), &[7, 8, 9, 10]);
        assert_eq!(fb.pixels()[3], 7);
    }

    #[test]
    fn resize_keeps_content() {
        let mut fb = Framebuffer::new(2, 2);
        fb.pixels_mut().copy_from_slice(&[1, 2, 3, 4]);
        fb.resize(3, 1);
        assert_eq!(fb.pixels(), &[1, 2, rgb(0, 0, 0)]);
    }
}
