//! Collecting the parts of the screen that changed.
//!
//! IronRDP reports every decoded update as its own rectangle — RemoteFX alone
//! produces one per 64×64 tile, so a scrolling window easily yields hundreds
//! per second. Sending each of them to the page on its own would cost one IPC
//! crossing (and one `putImageData`) per tile. Instead the session collects
//! them here and flushes the *current* image contents of whatever is dirty at
//! most once per frame interval.
//!
//! Merging trades pixels for messages: two rectangles are merged when they
//! overlap or nearly touch and their union does not waste much area. Above
//! [`MAX_RECTS`] the whole region collapses into its bounding box, which is
//! what a busy screen converges to anyway.
//!
//! Nothing here ever loses an update: a rectangle only ever grows into a
//! bigger one that still covers it.

/// A rectangle in desktop pixels. `x + w` and `y + h` are exclusive edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Pixel count, in u64 so a full 8192×8192 desktop cannot overflow.
    pub fn area(&self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }

    fn right(&self) -> u32 {
        u32::from(self.x) + u32::from(self.w)
    }

    fn bottom(&self) -> u32 {
        u32::from(self.y) + u32::from(self.h)
    }

    /// The smallest rectangle covering both.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(x, y, span(x, right), span(y, bottom))
    }

    pub fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// Whether the two overlap once each is grown by `gap` pixels on every side.
    fn near(&self, other: &Rect, gap: u32) -> bool {
        u32::from(self.x) <= other.right() + gap
            && u32::from(other.x) <= self.right() + gap
            && u32::from(self.y) <= other.bottom() + gap
            && u32::from(other.y) <= self.bottom() + gap
    }

    /// Clips to a `width`×`height` desktop; the result may be empty.
    pub fn clip(&self, width: u16, height: u16) -> Rect {
        let x = self.x.min(width);
        let y = self.y.min(height);
        let right = self.right().min(u32::from(width));
        let bottom = self.bottom().min(u32::from(height));
        Rect::new(x, y, span(x, right), span(y, bottom))
    }
}

/// `end - start` for an edge that is known to fit, saturating instead of
/// panicking should that ever not hold.
fn span(start: u16, end: u32) -> u16 {
    u16::try_from(end.saturating_sub(u32::from(start))).unwrap_or(u16::MAX)
}

/// Above this many rectangles the region collapses into its bounding box.
pub const MAX_RECTS: usize = 32;

/// Rectangles closer than this are candidates for merging. Small enough that
/// two unrelated widgets stay separate, big enough to swallow the seams of a
/// tiled codec.
pub const MERGE_GAP: u32 = 8;

/// The dirty region of the desktop, as a short list of rectangles.
#[derive(Debug, Default, Clone)]
pub struct DirtyRegion {
    rects: Vec<Rect>,
}

impl DirtyRegion {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    pub fn clear(&mut self) {
        self.rects.clear();
    }

    /// Adds a changed rectangle, merging it into its neighbours where that is
    /// cheap.
    pub fn add(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        let mut current = rect;
        // A merge can make the rectangle reach new neighbours, so keep going
        // until nothing else wants to join.
        while let Some(index) = self.rects.iter().position(|r| should_merge(r, &current)) {
            let other = self.rects.swap_remove(index);
            current = current.union(&other);
        }
        self.rects.push(current);

        if self.rects.len() > MAX_RECTS {
            let bbox = self
                .rects
                .iter()
                .fold(Rect::new(0, 0, 0, 0), |acc, r| acc.union(r));
            self.rects.clear();
            self.rects.push(bbox);
        }
    }

    /// Takes the whole region, leaving it empty.
    pub fn take(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.rects)
    }
}

fn should_merge(a: &Rect, b: &Rect) -> bool {
    if a.contains(b) || b.contains(a) {
        return true;
    }
    if !a.near(b, MERGE_GAP) {
        return false;
    }
    // Merge only if the union wastes at most half of the larger rectangle
    // on pixels that did not change.
    let union = a.union(b).area();
    let budget = a.area() + b.area() + a.area().max(b.area()) / 2;
    union <= budget
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covered(region: &DirtyRegion, x: u16, y: u16) -> bool {
        region
            .rects()
            .iter()
            .any(|r| r.contains(&Rect::new(x, y, 1, 1)))
    }

    #[test]
    fn empty_rects_are_ignored() {
        let mut region = DirtyRegion::new();
        region.add(Rect::new(10, 10, 0, 5));
        region.add(Rect::new(10, 10, 5, 0));
        assert!(region.is_empty());
    }

    #[test]
    fn adjacent_tiles_become_one_rect() {
        let mut region = DirtyRegion::new();
        for ty in 0..4u16 {
            for tx in 0..4u16 {
                region.add(Rect::new(tx * 64, ty * 64, 64, 64));
            }
        }
        assert_eq!(region.rects(), &[Rect::new(0, 0, 256, 256)]);
    }

    #[test]
    fn contained_rect_is_absorbed() {
        let mut region = DirtyRegion::new();
        region.add(Rect::new(0, 0, 100, 100));
        region.add(Rect::new(10, 10, 5, 5));
        assert_eq!(region.rects(), &[Rect::new(0, 0, 100, 100)]);
    }

    #[test]
    fn distant_rects_stay_separate() {
        let mut region = DirtyRegion::new();
        region.add(Rect::new(0, 0, 10, 10));
        region.add(Rect::new(500, 500, 10, 10));
        assert_eq!(region.rects().len(), 2);
    }

    #[test]
    fn nearby_small_rects_merge_across_a_small_gap() {
        let mut region = DirtyRegion::new();
        region.add(Rect::new(0, 0, 100, 20));
        region.add(Rect::new(0, 24, 100, 20));
        assert_eq!(region.rects(), &[Rect::new(0, 0, 100, 44)]);
    }

    #[test]
    fn a_diagonal_pair_that_would_waste_area_stays_separate() {
        let mut region = DirtyRegion::new();
        // Touching corners: the union would be four times their combined area.
        region.add(Rect::new(0, 0, 100, 100));
        region.add(Rect::new(100, 100, 100, 100));
        assert_eq!(region.rects().len(), 2);
    }

    #[test]
    fn a_merge_that_reaches_a_third_rect_takes_it_along() {
        let mut region = DirtyRegion::new();
        region.add(Rect::new(0, 0, 50, 50));
        region.add(Rect::new(100, 0, 50, 50));
        assert_eq!(region.rects().len(), 2);
        // Bridges the two: now all three belong together.
        region.add(Rect::new(40, 0, 70, 50));
        assert_eq!(region.rects(), &[Rect::new(0, 0, 150, 50)]);
    }

    #[test]
    fn too_many_rects_collapse_into_the_bounding_box() {
        let mut region = DirtyRegion::new();
        for i in 0..(MAX_RECTS as u16 + 1) {
            // A diagonal of isolated dots, 40 px apart: nothing merges.
            region.add(Rect::new(i * 40, i * 40, 2, 2));
        }
        let last = MAX_RECTS as u16 * 40;
        assert_eq!(region.rects(), &[Rect::new(0, 0, last + 2, last + 2)]);
    }

    #[test]
    fn merging_never_loses_a_pixel() {
        // Pseudo-random rectangles; every pixel of every one must stay covered.
        let mut region = DirtyRegion::new();
        let mut seed = 0x2545_f491_u32;
        let mut added = Vec::new();
        for _ in 0..200 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let x = (seed % 900) as u16;
            let y = ((seed >> 10) % 700) as u16;
            let w = ((seed >> 20) % 60) as u16 + 1;
            let h = ((seed >> 4) % 60) as u16 + 1;
            let rect = Rect::new(x, y, w, h);
            region.add(rect);
            added.push(rect);
            assert!(region.rects().len() <= MAX_RECTS);
        }
        for rect in added {
            for (x, y) in [
                (rect.x, rect.y),
                (rect.x + rect.w - 1, rect.y),
                (rect.x, rect.y + rect.h - 1),
                (rect.x + rect.w - 1, rect.y + rect.h - 1),
            ] {
                assert!(covered(&region, x, y), "lost ({x}, {y}) of {rect:?}");
            }
        }
    }

    #[test]
    fn clip_cuts_at_the_desktop_edge() {
        assert_eq!(
            Rect::new(1000, 700, 100, 100).clip(1024, 768),
            Rect::new(1000, 700, 24, 68)
        );
        assert!(Rect::new(2000, 0, 10, 10).clip(1024, 768).is_empty());
    }

    #[test]
    fn take_empties_the_region() {
        let mut region = DirtyRegion::new();
        region.add(Rect::new(0, 0, 1, 1));
        assert_eq!(region.take().len(), 1);
        assert!(region.is_empty());
    }
}
