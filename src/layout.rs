//! Pane layout: a binary split tree with one-cell separators between panes.

use crate::keys::Direction;
use ratatui::layout::Rect;

pub type PaneId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// `a` on the left, `b` on the right.
    Horizontal,
    /// `a` on top, `b` below.
    Vertical,
}

#[derive(Clone, Debug)]
pub enum Node {
    Leaf(PaneId),
    Split { axis: Axis, ratio: f32, a: Box<Node>, b: Box<Node> },
}

/// A separator line between two halves of a split.
#[derive(Clone, Debug)]
pub struct Separator {
    pub rect: Rect,
    pub axis: Axis,
    /// The area of the split this separator divides.
    pub area: Rect,
    /// Path from the root to the split: `false` = `a`, `true` = `b`.
    pub path: Vec<bool>,
}

#[derive(Clone, Debug, Default)]
pub struct Geometry {
    pub panes: Vec<(PaneId, Rect)>,
    pub seps: Vec<Separator>,
}

impl Geometry {
    pub fn rect_of(&self, id: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == id).map(|(_, r)| *r)
    }

    pub fn pane_at(&self, x: u16, y: u16) -> Option<(PaneId, Rect)> {
        self.panes.iter().copied().find(|(_, r)| contains(*r, x, y))
    }

    pub fn sep_at(&self, x: u16, y: u16) -> Option<&Separator> {
        self.seps.iter().find(|s| contains(s.rect, x, y))
    }
}

pub fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    EvenHorizontal,
    EvenVertical,
    MainVertical,
    MainHorizontal,
    Tiled,
}

impl Preset {
    pub fn next(self) -> Preset {
        match self {
            Preset::EvenHorizontal => Preset::EvenVertical,
            Preset::EvenVertical => Preset::MainVertical,
            Preset::MainVertical => Preset::MainHorizontal,
            Preset::MainHorizontal => Preset::Tiled,
            Preset::Tiled => Preset::EvenHorizontal,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Preset::EvenHorizontal => "even-horizontal",
            Preset::EvenVertical => "even-vertical",
            Preset::MainVertical => "main-vertical",
            Preset::MainHorizontal => "main-horizontal",
            Preset::Tiled => "tiled",
        }
    }
}

const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 1;

enum Found {
    No,
    Pending,
    Done,
}

/// Split `total` cells into (a, separator, b).
fn divide(total: u16, ratio: f32, min_a: u16, min_b: u16) -> (u16, u16, u16) {
    if total < 3 {
        return (total, 0, 0);
    }
    let usable = total - 1;
    let want = (usable as f32 * ratio).round() as i32;
    let (lo, hi) =
        if min_a + min_b <= usable { (min_a as i32, (usable - min_b) as i32) } else { (1, usable as i32 - 1) };
    let a = want.clamp(lo, hi) as u16;
    (a, 1, usable - a)
}

impl Node {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(p) => *p == id,
            Node::Split { a, b, .. } => a.contains(id) || b.contains(id),
        }
    }

    pub fn count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, .. } => a.count() + b.count(),
        }
    }

    /// Smallest extent along `axis` at which every pane stays visible.
    fn min_extent(&self, axis: Axis) -> u16 {
        match self {
            Node::Leaf(_) => match axis {
                Axis::Horizontal => MIN_COLS,
                Axis::Vertical => MIN_ROWS,
            },
            Node::Split { axis: ax, a, b, .. } => {
                let (ma, mb) = (a.min_extent(axis), b.min_extent(axis));
                if *ax == axis {
                    ma + 1 + mb
                } else {
                    ma.max(mb)
                }
            }
        }
    }

    fn split_area(axis: Axis, ratio: f32, a: &Node, b: &Node, area: Rect) -> (Rect, Rect, Rect) {
        match axis {
            Axis::Horizontal => {
                let (wa, ws, wb) = divide(area.width, ratio, a.min_extent(axis), b.min_extent(axis));
                (
                    Rect::new(area.x, area.y, wa, area.height),
                    Rect::new(area.x + wa, area.y, ws, area.height),
                    Rect::new(area.x + wa + ws, area.y, wb, area.height),
                )
            }
            Axis::Vertical => {
                let (ha, hs, hb) = divide(area.height, ratio, a.min_extent(axis), b.min_extent(axis));
                (
                    Rect::new(area.x, area.y, area.width, ha),
                    Rect::new(area.x, area.y + ha, area.width, hs),
                    Rect::new(area.x, area.y + ha + hs, area.width, hb),
                )
            }
        }
    }

    pub fn geometry(&self, area: Rect) -> Geometry {
        let mut geom = Geometry::default();
        let mut path = Vec::new();
        self.place(area, &mut path, &mut geom);
        geom
    }

    fn place(&self, area: Rect, path: &mut Vec<bool>, geom: &mut Geometry) {
        match self {
            Node::Leaf(id) => geom.panes.push((*id, area)),
            Node::Split { axis, ratio, a, b } => {
                let (ra, rs, rb) = Self::split_area(*axis, *ratio, a, b, area);
                if rs.width > 0 && rs.height > 0 {
                    geom.seps.push(Separator { rect: rs, axis: *axis, area, path: path.clone() });
                }
                path.push(false);
                a.place(ra, path, geom);
                path.pop();
                path.push(true);
                b.place(rb, path, geom);
                path.pop();
            }
        }
    }

    /// Split `target` in two; the new pane goes to the right / below.
    pub fn split(&mut self, target: PaneId, axis: Axis, new: PaneId) -> bool {
        match self {
            Node::Leaf(id) if *id == target => {
                let old = Node::Leaf(*id);
                *self = Node::Split { axis, ratio: 0.5, a: Box::new(old), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(target, axis, new) || b.split(target, axis, new),
        }
    }

    /// Remove `target`, letting its sibling take over the space. Returns
    /// false when `target` is not below this node or *is* this node.
    pub fn remove(&mut self, target: PaneId) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                if matches!(**a, Node::Leaf(id) if id == target) {
                    let keep = std::mem::replace(&mut **b, Node::Leaf(0));
                    *self = keep;
                    return true;
                }
                if matches!(**b, Node::Leaf(id) if id == target) {
                    let keep = std::mem::replace(&mut **a, Node::Leaf(0));
                    *self = keep;
                    return true;
                }
                a.remove(target) || b.remove(target)
            }
        }
    }

    pub fn swap(&mut self, x: PaneId, y: PaneId) {
        match self {
            Node::Leaf(id) => {
                if *id == x {
                    *id = y;
                } else if *id == y {
                    *id = x;
                }
            }
            Node::Split { a, b, .. } => {
                a.swap(x, y);
                b.swap(x, y);
            }
        }
    }

    /// Move the border next to `target` by `amount` cells in direction `dir`.
    pub fn resize(&mut self, area: Rect, target: PaneId, dir: Direction, amount: u16) -> bool {
        let amount = amount as i32;
        let (axis, delta) = match dir {
            Direction::Left => (Axis::Horizontal, -amount),
            Direction::Right => (Axis::Horizontal, amount),
            Direction::Up => (Axis::Vertical, -amount),
            Direction::Down => (Axis::Vertical, amount),
        };
        // Prefer the border on the far side (right / bottom) of the pane,
        // fall back to the near one for panes on the edge.
        matches!(self.adjust(area, target, axis, delta, true), Found::Done)
            || matches!(self.adjust(area, target, axis, delta, false), Found::Done)
    }

    fn adjust(&mut self, area: Rect, target: PaneId, axis: Axis, delta: i32, want_a: bool) -> Found {
        match self {
            Node::Leaf(id) => {
                if *id == target {
                    Found::Pending
                } else {
                    Found::No
                }
            }
            Node::Split { axis: ax, ratio, a, b } => {
                let (ra, _, rb) = Self::split_area(*ax, *ratio, a, b, area);
                let (res, in_a) = match a.adjust(ra, target, axis, delta, want_a) {
                    Found::No => (b.adjust(rb, target, axis, delta, want_a), false),
                    r => (r, true),
                };
                match res {
                    Found::Pending if *ax == axis && in_a == want_a => {
                        let (total, cur) = match ax {
                            Axis::Horizontal => (area.width, ra.width),
                            Axis::Vertical => (area.height, ra.height),
                        };
                        if total < 3 {
                            return Found::Pending;
                        }
                        let usable = (total - 1) as i32;
                        let (min_a, min_b) = (a.min_extent(axis) as i32, b.min_extent(axis) as i32);
                        if min_a + min_b > usable {
                            return Found::Pending;
                        }
                        let new = (cur as i32 + delta).clamp(min_a, usable - min_b);
                        if new == cur as i32 {
                            // Already against the limit; let an outer split try.
                            return Found::Pending;
                        }
                        *ratio = new as f32 / usable as f32;
                        Found::Done
                    }
                    r => r,
                }
            }
        }
    }

    /// Place the separator of the split at `path` so that `a` is `cells` wide/tall.
    pub fn set_split(&mut self, path: &[bool], area: Rect, cells: i32) {
        let mut node = self;
        for step in path {
            node = match node {
                Node::Split { a, b, .. } => {
                    if *step {
                        b
                    } else {
                        a
                    }
                }
                Node::Leaf(_) => return,
            };
        }
        if let Node::Split { axis, ratio, a, b } = node {
            let total = match axis {
                Axis::Horizontal => area.width,
                Axis::Vertical => area.height,
            };
            if total < 3 {
                return;
            }
            let usable = (total - 1) as i32;
            let (min_a, min_b) = (a.min_extent(*axis) as i32, b.min_extent(*axis) as i32);
            if min_a + min_b > usable {
                return;
            }
            *ratio = cells.clamp(min_a, usable - min_b) as f32 / usable as f32;
        }
    }

    pub fn preset(kind: Preset, panes: &[PaneId]) -> Node {
        let leaves = |ids: &[PaneId]| ids.iter().map(|id| Node::Leaf(*id)).collect::<Vec<_>>();
        match kind {
            _ if panes.len() <= 1 => Node::Leaf(panes.first().copied().unwrap_or(0)),
            Preset::EvenHorizontal => chain(Axis::Horizontal, leaves(panes)),
            Preset::EvenVertical => chain(Axis::Vertical, leaves(panes)),
            Preset::MainVertical => Node::Split {
                axis: Axis::Horizontal,
                ratio: 0.6,
                a: Box::new(Node::Leaf(panes[0])),
                b: Box::new(chain(Axis::Vertical, leaves(&panes[1..]))),
            },
            Preset::MainHorizontal => Node::Split {
                axis: Axis::Vertical,
                ratio: 0.6,
                a: Box::new(Node::Leaf(panes[0])),
                b: Box::new(chain(Axis::Horizontal, leaves(&panes[1..]))),
            },
            Preset::Tiled => {
                let n = panes.len();
                let cols = (n as f64).sqrt().ceil() as usize;
                let rows: Vec<Node> = panes.chunks(cols).map(|row| chain(Axis::Horizontal, leaves(row))).collect();
                chain(Axis::Vertical, rows)
            }
        }
    }
}

/// Lay `nodes` out evenly along `axis`.
fn chain(axis: Axis, mut nodes: Vec<Node>) -> Node {
    let n = nodes.len();
    if n == 1 {
        return nodes.remove(0);
    }
    let first = nodes.remove(0);
    Node::Split { axis, ratio: 1.0 / n as f32, a: Box::new(first), b: Box::new(chain(axis, nodes)) }
}

/// The pane to move to from `from` in direction `dir`. Wraps around at the
/// window edge. Among several candidates the one with the highest `rank`
/// (most recently used) wins.
pub fn neighbor(
    panes: &[(PaneId, Rect)],
    area: Rect,
    from: PaneId,
    dir: Direction,
    rank: impl Fn(PaneId) -> u64,
) -> Option<PaneId> {
    let cur = panes.iter().find(|(id, _)| *id == from)?.1;
    let right = |r: Rect| r.x + r.width;
    let bottom = |r: Rect| r.y + r.height;
    let overlap_v = |r: Rect| r.y < bottom(cur) && cur.y < bottom(r);
    let overlap_h = |r: Rect| r.x < right(cur) && cur.x < right(r);
    let candidates = panes.iter().filter(|(id, r)| *id != from && r.width > 0 && r.height > 0).filter(|(_, r)| {
        let r = *r;
        match dir {
            Direction::Left => {
                overlap_v(r) && if cur.x == area.x { right(r) == right(area) } else { right(r) + 1 == cur.x }
            }
            Direction::Right => {
                overlap_v(r) && if right(cur) == right(area) { r.x == area.x } else { r.x == right(cur) + 1 }
            }
            Direction::Up => {
                overlap_h(r) && if cur.y == area.y { bottom(r) == bottom(area) } else { bottom(r) + 1 == cur.y }
            }
            Direction::Down => {
                overlap_h(r) && if bottom(cur) == bottom(area) { r.y == area.y } else { r.y == bottom(cur) + 1 }
            }
        }
    });
    candidates.max_by_key(|(id, _)| rank(*id)).map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 100, 40)
    }

    /// Every cell is either inside exactly one pane or on a separator.
    fn assert_tiles(node: &Node, area: Rect) {
        let geom = node.geometry(area);
        let mut grid = vec![0u8; area.width as usize * area.height as usize];
        let mut mark = |r: Rect| {
            for y in r.y..r.y + r.height {
                for x in r.x..r.x + r.width {
                    grid[(y - area.y) as usize * area.width as usize + (x - area.x) as usize] += 1;
                }
            }
        };
        for (_, r) in &geom.panes {
            assert!(r.width > 0 && r.height > 0, "empty pane in {geom:?}");
            mark(*r);
        }
        for s in &geom.seps {
            mark(s.rect);
        }
        assert!(grid.iter().all(|c| *c == 1), "layout does not tile the area");
    }

    fn three() -> Node {
        // 1 | 2
        //   | -
        //   | 3
        let mut n = Node::Leaf(1);
        assert!(n.split(1, Axis::Horizontal, 2));
        assert!(n.split(2, Axis::Vertical, 3));
        n
    }

    #[test]
    fn split_and_tile() {
        let n = three();
        assert_eq!(n.panes(), vec![1, 2, 3]);
        assert_tiles(&n, area());
        let g = n.geometry(area());
        assert_eq!(g.rect_of(1), Some(Rect::new(0, 0, 50, 40)));
        assert_eq!(g.rect_of(2), Some(Rect::new(51, 0, 49, 20)));
        assert_eq!(g.rect_of(3), Some(Rect::new(51, 21, 49, 19)));
        assert_eq!(g.seps.len(), 2);
    }

    #[test]
    fn remove_collapses() {
        let mut n = three();
        assert!(n.remove(2));
        assert_eq!(n.panes(), vec![1, 3]);
        assert_tiles(&n, area());
        assert!(n.remove(1));
        assert_eq!(n.panes(), vec![3]);
        assert!(!n.remove(3));
    }

    #[test]
    fn resize_moves_the_right_border() {
        let mut n = three();
        assert!(n.resize(area(), 1, Direction::Right, 10));
        assert_eq!(n.geometry(area()).rect_of(1).unwrap().width, 60);
        // Pane 2 sits on the right edge: its only horizontal border is the left one.
        assert!(n.resize(area(), 2, Direction::Left, 20));
        assert_eq!(n.geometry(area()).rect_of(1).unwrap().width, 40);
        assert!(n.resize(area(), 2, Direction::Down, 5));
        assert_eq!(n.geometry(area()).rect_of(2).unwrap().height, 25);
        assert!(n.resize(area(), 3, Direction::Up, 5));
        assert_eq!(n.geometry(area()).rect_of(2).unwrap().height, 20);
        // Cannot push a border through a pane.
        assert!(n.resize(area(), 1, Direction::Left, 500));
        assert_eq!(n.geometry(area()).rect_of(1).unwrap().width, 2);
        assert!(!n.resize(area(), 1, Direction::Left, 1));
        assert!(!n.resize(area(), 1, Direction::Up, 1));
        assert_tiles(&n, area());
    }

    #[test]
    fn neighbors() {
        let n = three();
        let g = n.geometry(area());
        let rank = |_| 0;
        assert_eq!(neighbor(&g.panes, area(), 2, Direction::Left, rank), Some(1));
        assert_eq!(neighbor(&g.panes, area(), 2, Direction::Down, rank), Some(3));
        assert_eq!(neighbor(&g.panes, area(), 3, Direction::Up, rank), Some(2));
        // Wrap-around.
        assert_eq!(neighbor(&g.panes, area(), 2, Direction::Up, rank), Some(3));
        assert_eq!(neighbor(&g.panes, area(), 3, Direction::Right, rank), Some(1));
        // Two candidates: most recently used wins.
        assert_eq!(neighbor(&g.panes, area(), 1, Direction::Right, |id| if id == 3 { 9 } else { 1 }), Some(3));
        assert_eq!(neighbor(&g.panes, area(), 1, Direction::Right, |id| if id == 2 { 9 } else { 1 }), Some(2));
        assert_eq!(neighbor(&[(1, area())], area(), 1, Direction::Left, rank), None);
    }

    #[test]
    fn presets_tile() {
        for n in 1..=9u32 {
            let ids: Vec<PaneId> = (1..=n).collect();
            for kind in [
                Preset::EvenHorizontal,
                Preset::EvenVertical,
                Preset::MainVertical,
                Preset::MainHorizontal,
                Preset::Tiled,
            ] {
                let node = Node::preset(kind, &ids);
                assert_eq!(node.panes(), ids);
                assert_tiles(&node, Rect::new(0, 1, 120, 40));
            }
        }
    }

    #[test]
    fn tiny_areas_do_not_panic() {
        let n = Node::preset(Preset::Tiled, &[1, 2, 3, 4, 5]);
        for w in 0..12 {
            for h in 0..6 {
                let g = n.geometry(Rect::new(0, 0, w, h));
                assert_eq!(g.panes.len(), 5);
            }
        }
    }

    #[test]
    fn drag_separator() {
        let mut n = three();
        let g = n.geometry(area());
        let sep = g.seps.iter().find(|s| s.axis == Axis::Horizontal).unwrap().clone();
        n.set_split(&sep.path, sep.area, 30);
        assert_eq!(n.geometry(area()).rect_of(1).unwrap().width, 30);
        let sep = g.seps.iter().find(|s| s.axis == Axis::Vertical).unwrap().clone();
        n.set_split(&sep.path, sep.area, 1000);
        assert_eq!(n.geometry(area()).rect_of(3).unwrap().height, 1);
        assert_tiles(&n, area());
    }
}
