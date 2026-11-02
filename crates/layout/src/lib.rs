//! The split tree a tab arranges its panes in.
//!
//! A tree of rows and columns whose leaves are panes. Pure data with the
//! operations the window needs (split, close, move, swap, resize, focus by
//! direction, geometry), so the rules are tested without a window.
//!
//! Invariants, kept by every operation:
//! - a split has at least two children;
//! - a child split never has its parent's axis (it is flattened into it);
//! - weights are positive.

use serde::{Deserialize, Serialize};

pub type PaneId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    /// Children side by side.
    Row,
    /// Children stacked.
    Column,
}

/// A side of a pane: where a new or moved pane goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn axis(self) -> Axis {
        match self {
            Edge::Left | Edge::Right => Axis::Row,
            Edge::Top | Edge::Bottom => Axis::Column,
        }
    }

    /// Whether the new pane goes before the target along the axis.
    fn before(self) -> bool {
        matches!(self, Edge::Left | Edge::Top)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Node {
    Pane { pane: PaneId },
    Split { axis: Axis, children: Vec<Child> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Child {
    pub weight: f32,
    pub node: Node,
}

/// A rectangle in any unit (the window uses logical pixels).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }

    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// The gap between two neighbors of a split, which drags to resize them.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    /// Which split: child indices from the root.
    pub path: Vec<usize>,
    /// The divider sits after this child.
    pub index: usize,
    pub axis: Axis,
    /// The gap itself.
    pub rect: Rect,
    /// The two neighbors' combined extent along the axis, for turning a drag
    /// distance into weight.
    pub span: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<Divider>,
}

impl Geometry {
    pub fn rect(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r)
    }
}

/// Smallest share a pane keeps when a divider is dragged, of the two
/// neighbors' combined weight.
const MIN_SHARE: f32 = 0.08;

impl Node {
    pub fn pane(pane: PaneId) -> Self {
        Node::Pane { pane }
    }

    /// Every pane, in reading order.
    /// The same tree with every pane renamed by `rename` (`None` drops the
    /// pane; a split left with one child becomes that child). `None` if no
    /// pane is left.
    pub fn map_panes(&self, rename: &mut impl FnMut(PaneId) -> Option<PaneId>) -> Option<Node> {
        match self {
            Node::Pane { pane } => rename(*pane).map(Node::pane),
            Node::Split { axis, children } => {
                let mut kept: Vec<Child> = Vec::new();
                for c in children {
                    match c.node.map_panes(rename) {
                        None => {}
                        // A child that collapsed into a split along this
                        // axis joins this one (no same-axis nesting).
                        Some(Node::Split { axis: inner, children: grand }) if inner == *axis => {
                            let total: f32 = grand.iter().map(|g| g.weight).sum();
                            kept.extend(grand.into_iter().map(|g| Child { weight: c.weight * g.weight / total, node: g.node }));
                        }
                        Some(node) => kept.push(Child { weight: c.weight, node }),
                    }
                }
                match kept.len() {
                    0 => None,
                    1 => kept.pop().map(|c| c.node),
                    _ => Some(Node::Split { axis: *axis, children: kept }),
                }
            }
        }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Pane { pane } => out.push(*pane),
            Node::Split { children, .. } => children.iter().for_each(|c| c.node.collect(out)),
        }
    }

    pub fn contains(&self, pane: PaneId) -> bool {
        match self {
            Node::Pane { pane: p } => *p == pane,
            Node::Split { children, .. } => children.iter().any(|c| c.node.contains(pane)),
        }
    }

    /// Puts `new` beside `target` on `edge`. False if `target` is not here.
    pub fn split(&mut self, target: PaneId, edge: Edge, new: PaneId) -> bool {
        self.insert(target, edge, Node::pane(new))
    }

    fn insert(&mut self, target: PaneId, edge: Edge, new: Node) -> bool {
        let axis = edge.axis();
        match self {
            Node::Pane { pane } if *pane == target => {
                let old = std::mem::replace(self, Node::pane(0));
                let (a, b) = if edge.before() { (new, old) } else { (old, new) };
                *self = Node::Split {
                    axis,
                    children: vec![Child { weight: 1.0, node: a }, Child { weight: 1.0, node: b }],
                };
                true
            }
            Node::Pane { .. } => false,
            Node::Split { axis: split_axis, children } => {
                let Some(i) = children.iter().position(|c| c.node.contains(target)) else {
                    return false;
                };
                // Target is a direct pane child of a split along the same
                // axis: become its sibling, sharing its space.
                if *split_axis == axis && matches!(children[i].node, Node::Pane { pane } if pane == target) {
                    let half = children[i].weight / 2.0;
                    children[i].weight = half;
                    let at = if edge.before() { i } else { i + 1 };
                    children.insert(at, Child { weight: half, node: new });
                    return true;
                }
                let done = children[i].node.insert(target, edge, new);
                normalize_child(*split_axis, children, i);
                done
            }
        }
    }

    /// Removes `pane`. Returns false if it is not here or is the last pane
    /// (a tree is never empty: closing the last pane closes the tab).
    pub fn remove(&mut self, pane: PaneId) -> bool {
        if matches!(self, Node::Pane { .. }) {
            return false;
        }
        self.remove_inner(pane)
    }

    fn remove_inner(&mut self, pane: PaneId) -> bool {
        let Node::Split { axis, children } = self else { return false };
        let Some(i) = children.iter().position(|c| c.node.contains(pane)) else {
            return false;
        };
        if matches!(children[i].node, Node::Pane { pane: p } if p == pane) {
            let removed = children.remove(i);
            // Its space goes to the neighbor that takes its place (the one
            // before it when it was last).
            let neighbor = i.min(children.len() - 1);
            children[neighbor].weight += removed.weight;
        } else {
            children[i].node.remove_inner(pane);
            normalize_child(*axis, children, i);
        }
        if children.len() == 1 {
            *self = children.pop().unwrap().node;
        }
        true
    }

    /// Moves `pane` beside `target` on `edge` (a drag onto a pane's side).
    pub fn move_to(&mut self, pane: PaneId, target: PaneId, edge: Edge) -> bool {
        if pane == target || !self.contains(pane) || !self.contains(target) {
            return false;
        }
        if !self.remove(pane) {
            return false;
        }
        self.split(target, edge, pane)
    }

    /// Exchanges two panes' places.
    pub fn swap(&mut self, a: PaneId, b: PaneId) -> bool {
        if a == b || !self.contains(a) || !self.contains(b) {
            return false;
        }
        self.for_each_pane(&mut |p| {
            if *p == a {
                *p = b;
            } else if *p == b {
                *p = a;
            }
        });
        true
    }

    fn for_each_pane(&mut self, f: &mut impl FnMut(&mut PaneId)) {
        match self {
            Node::Pane { pane } => f(pane),
            Node::Split { children, .. } => children.iter_mut().for_each(|c| c.node.for_each_pane(f)),
        }
    }

    /// Drags the divider after child `index` of the split at `path` by
    /// `delta` (in the same unit as `span`, the two neighbors' extent).
    pub fn resize(&mut self, path: &[usize], index: usize, delta: f32, span: f32) -> bool {
        let Some(Node::Split { children, .. }) = self.at_mut(path) else { return false };
        if index + 1 >= children.len() || span <= 0.0 {
            return false;
        }
        let total = children[index].weight + children[index + 1].weight;
        let min = total * MIN_SHARE;
        let moved = delta / span * total;
        let a = (children[index].weight + moved).clamp(min, total - min);
        children[index].weight = a;
        children[index + 1].weight = total - a;
        true
    }

    /// Gives every child of the split at `path` the same share.
    pub fn equalize(&mut self, path: &[usize]) -> bool {
        let Some(Node::Split { children, .. }) = self.at_mut(path) else { return false };
        children.iter_mut().for_each(|c| c.weight = 1.0);
        true
    }

    fn at_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        let mut node = self;
        for &i in path {
            let Node::Split { children, .. } = node else { return None };
            node = &mut children.get_mut(i)?.node;
        }
        Some(node)
    }

    /// Lays the tree out in `area`, `gap` apart.
    pub fn geometry(&self, area: Rect, gap: f32) -> Geometry {
        let mut g = Geometry::default();
        self.lay_out(area, gap, &mut Vec::new(), &mut g);
        g
    }

    fn lay_out(&self, area: Rect, gap: f32, path: &mut Vec<usize>, g: &mut Geometry) {
        match self {
            Node::Pane { pane } => g.panes.push((*pane, area)),
            Node::Split { axis, children } => {
                let extent = match axis {
                    Axis::Row => area.w,
                    Axis::Column => area.h,
                };
                let free = (extent - gap * (children.len() - 1) as f32).max(0.0);
                let total: f32 = children.iter().map(|c| c.weight).sum();
                let mut at = 0.0;
                let mut spans = Vec::with_capacity(children.len());
                for (i, child) in children.iter().enumerate() {
                    let len = free * child.weight / total;
                    let rect = match axis {
                        Axis::Row => Rect::new(area.x + at, area.y, len, area.h),
                        Axis::Column => Rect::new(area.x, area.y + at, area.w, len),
                    };
                    spans.push(len);
                    path.push(i);
                    child.node.lay_out(rect, gap, path, g);
                    path.pop();
                    at += len;
                    if i + 1 < children.len() {
                        let rect = match axis {
                            Axis::Row => Rect::new(area.x + at, area.y, gap, area.h),
                            Axis::Column => Rect::new(area.x, area.y + at, area.w, gap),
                        };
                        g.dividers.push(Divider { path: path.clone(), index: i, axis: *axis, rect, span: 0.0 });
                        at += gap;
                    }
                }
                // Each divider resizes its two neighbors.
                let first = g.dividers.len() - (children.len() - 1);
                for (k, d) in g.dividers[first..].iter_mut().enumerate() {
                    d.span = spans[k] + spans[k + 1];
                }
            }
        }
    }

    /// The pane next to `from` towards `edge`, in a layout of `area`: the
    /// nearest pane whose span overlaps `from`'s across the direction.
    pub fn neighbor(&self, from: PaneId, edge: Edge, area: Rect) -> Option<PaneId> {
        let g = self.geometry(area, 0.0);
        let src = g.rect(from)?;
        let (cx, cy) = src.center();
        let eps = 0.5;
        g.panes
            .iter()
            .filter(|(p, _)| *p != from)
            .filter(|(_, r)| match edge {
                Edge::Left => r.x + r.w <= src.x + eps && overlaps(r.y, r.h, src.y, src.h),
                Edge::Right => r.x >= src.x + src.w - eps && overlaps(r.y, r.h, src.y, src.h),
                Edge::Top => r.y + r.h <= src.y + eps && overlaps(r.x, r.w, src.x, src.w),
                Edge::Bottom => r.y >= src.y + src.h - eps && overlaps(r.x, r.w, src.x, src.w),
            })
            .min_by(|(_, a), (_, b)| {
                let da = distance(src, *a, edge, (cx, cy));
                let db = distance(src, *b, edge, (cx, cy));
                da.total_cmp(&db)
            })
            .map(|(p, _)| *p)
    }
}

fn overlaps(a: f32, alen: f32, b: f32, blen: f32) -> bool {
    a < b + blen && b < a + alen
}

/// How far `to` is from `src` going towards `edge`; ties broken by how far
/// its center is from `src`'s across the direction.
fn distance(src: Rect, to: Rect, edge: Edge, (cx, cy): (f32, f32)) -> f32 {
    let (tx, ty) = to.center();
    let (along, across) = match edge {
        Edge::Left => (src.x - (to.x + to.w), (ty - cy).abs()),
        Edge::Right => (to.x - (src.x + src.w), (ty - cy).abs()),
        Edge::Top => (src.y - (to.y + to.h), (tx - cx).abs()),
        Edge::Bottom => (to.y - (src.y + src.h), (tx - cx).abs()),
    };
    along * 1000.0 + across
}

/// After child `i` changed: a split child along its parent's axis merges
/// into the parent, sharing out the child's weight.
fn normalize_child(axis: Axis, children: &mut Vec<Child>, i: usize) {
    let Node::Split { axis: child_axis, .. } = &children[i].node else { return };
    if *child_axis != axis {
        return;
    }
    let child = children.remove(i);
    let Node::Split { children: inner, .. } = child.node else { unreachable!() };
    let total: f32 = inner.iter().map(|c| c.weight).sum();
    for (k, c) in inner.into_iter().enumerate() {
        children.insert(i + k, Child { weight: child.weight * c.weight / total, node: c.node });
    }
}

/// Which side of a pane a point falls on for a drop: the outer quarter picks
/// the nearest edge, the middle is `None` (swap / replace).
pub fn drop_zone(rect: Rect, x: f32, y: f32) -> Option<Option<Edge>> {
    if !rect.contains(x, y) || rect.w <= 0.0 || rect.h <= 0.0 {
        return None;
    }
    let fx = (x - rect.x) / rect.w;
    let fy = (y - rect.y) / rect.h;
    let (d, edge) = [(fx, Edge::Left), (1.0 - fx, Edge::Right), (fy, Edge::Top), (1.0 - fy, Edge::Bottom)]
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap();
    Some((d < 0.25).then_some(edge))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0.0, 0.0, 1000.0, 800.0)
    }

    fn check(n: &Node) {
        if let Node::Split { axis, children } = n {
            assert!(children.len() >= 2, "split with {} children", children.len());
            for c in children {
                assert!(c.weight > 0.0);
                if let Node::Split { axis: a, .. } = &c.node {
                    assert_ne!(a, axis, "nested split along the same axis");
                }
                check(&c.node);
            }
        }
    }

    #[test]
    fn map_panes_renames_and_drops() {
        // 1 | (2 / (3 | 4)): dropping 2 leaves 1 | 3 | 4, flat.
        let mut n = Node::pane(1);
        assert!(n.split(1, Edge::Right, 2));
        assert!(n.split(2, Edge::Bottom, 3));
        assert!(n.split(3, Edge::Right, 4));
        let m = n.map_panes(&mut |p| (p != 2).then_some(p * 10)).unwrap();
        check(&m);
        assert_eq!(m.panes(), vec![10, 30, 40]);
        assert!(n.map_panes(&mut |_| None).is_none());
    }

    #[test]
    fn split_right_then_right_stays_flat() {
        let mut n = Node::pane(1);
        assert!(n.split(1, Edge::Right, 2));
        assert!(n.split(2, Edge::Right, 3));
        check(&n);
        assert_eq!(n.panes(), vec![1, 2, 3]);
        let Node::Split { axis, children } = &n else { panic!() };
        assert_eq!(*axis, Axis::Row);
        assert_eq!(children.len(), 3);
        // 2 gave half its space to 3.
        assert_eq!(children[0].weight, 1.0);
        assert_eq!(children[1].weight, 0.5);
        assert_eq!(children[2].weight, 0.5);
    }

    #[test]
    fn split_left_and_top_go_before() {
        let mut n = Node::pane(1);
        n.split(1, Edge::Left, 2);
        assert_eq!(n.panes(), vec![2, 1]);
        n.split(1, Edge::Top, 3);
        check(&n);
        assert_eq!(n.panes(), vec![2, 3, 1]);
    }

    #[test]
    fn remove_collapses_and_gives_space_to_neighbor() {
        let mut n = Node::pane(1);
        n.split(1, Edge::Right, 2);
        n.split(2, Edge::Bottom, 3);
        assert!(n.remove(3));
        check(&n);
        assert_eq!(n.panes(), vec![1, 2]);
        assert!(n.remove(1));
        assert_eq!(n, Node::pane(2));
        assert!(!n.remove(2), "the last pane stays");
    }

    #[test]
    fn remove_merges_same_axis_grandchildren() {
        let mut n = Node::pane(1);
        n.split(1, Edge::Right, 2);
        n.split(2, Edge::Bottom, 3);
        n.split(3, Edge::Right, 4); // row[1, col[2, row[3,4]]]
        check(&n);
        n.remove(2); // col[row[3,4]] -> row[3,4] merges into the outer row
        check(&n);
        let Node::Split { axis, children } = &n else { panic!() };
        assert_eq!(*axis, Axis::Row);
        assert_eq!(children.len(), 3);
        assert_eq!(n.panes(), vec![1, 3, 4]);
    }

    #[test]
    fn move_and_swap() {
        let mut n = Node::pane(1);
        n.split(1, Edge::Right, 2);
        n.split(2, Edge::Right, 3);
        assert!(n.move_to(3, 1, Edge::Top));
        check(&n);
        assert_eq!(n.panes(), vec![3, 1, 2]);
        assert!(n.swap(3, 2));
        assert_eq!(n.panes(), vec![2, 1, 3]);
        assert!(!n.move_to(1, 1, Edge::Left));
    }

    #[test]
    fn geometry_and_dividers() {
        let mut n = Node::pane(1);
        n.split(1, Edge::Right, 2);
        let g = n.geometry(area(), 10.0);
        assert_eq!(g.rect(1), Some(Rect::new(0.0, 0.0, 495.0, 800.0)));
        assert_eq!(g.rect(2), Some(Rect::new(505.0, 0.0, 495.0, 800.0)));
        assert_eq!(g.dividers.len(), 1);
        let d = &g.dividers[0];
        assert_eq!(d.rect, Rect::new(495.0, 0.0, 10.0, 800.0));
        assert_eq!(d.span, 990.0);
        assert!(d.path.is_empty());
    }

    #[test]
    fn resize_moves_weight_and_clamps() {
        let mut n = Node::pane(1);
        n.split(1, Edge::Right, 2);
        let d = n.geometry(area(), 0.0).dividers[0].clone();
        assert!(n.resize(&d.path, d.index, 250.0, d.span));
        let g = n.geometry(area(), 0.0);
        assert!((g.rect(1).unwrap().w - 750.0).abs() < 0.01);
        n.resize(&d.path, d.index, 5000.0, d.span);
        let g = n.geometry(area(), 0.0);
        assert!((g.rect(2).unwrap().w - 1000.0 * MIN_SHARE).abs() < 0.01);
    }

    #[test]
    fn neighbors() {
        // row[1, col[2, 3]]
        let mut n = Node::pane(1);
        n.split(1, Edge::Right, 2);
        n.split(2, Edge::Bottom, 3);
        assert_eq!(n.neighbor(1, Edge::Right, area()), Some(2));
        assert_eq!(n.neighbor(2, Edge::Bottom, area()), Some(3));
        assert_eq!(n.neighbor(3, Edge::Left, area()), Some(1));
        assert_eq!(n.neighbor(1, Edge::Left, area()), None);
    }

    #[test]
    fn drop_zones() {
        let r = Rect::new(0.0, 0.0, 100.0, 100.0);
        assert_eq!(drop_zone(r, 5.0, 50.0), Some(Some(Edge::Left)));
        assert_eq!(drop_zone(r, 50.0, 95.0), Some(Some(Edge::Bottom)));
        assert_eq!(drop_zone(r, 50.0, 50.0), Some(None));
        assert_eq!(drop_zone(r, 150.0, 50.0), None);
    }

    #[test]
    fn serde_round_trip() {
        let mut n = Node::pane(1);
        n.split(1, Edge::Right, 2);
        let s = serde_json::to_string(&n).unwrap();
        let back: Node = serde_json::from_str(&s).unwrap();
        assert_eq!(n, back);
    }
}
