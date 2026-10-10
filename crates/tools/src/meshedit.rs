//! Selecting and dragging mesh points and their handles on the canvas, shared by the Mesh tool and
//! Direct Selection: gradient meshes and mesh envelopes alike.
//!
//! Mesh points are a real selection ([`vectorcraft_doc::Selection::mesh_points`], set with
//! `select.meshPoints`), so they survive tool switches and undo. A press on a point selects it
//! (Shift adds or removes it) even on a mesh that wasn't selected, and a drag moves every selected
//! point (`path.moveAnchors`), snapping as dragged anchors do. The selected points' handles show
//! (the canvas draws them) and drag (`object.mesh.movePoint` with `handle`).

use std::borrow::Cow;
use std::collections::BTreeSet;

use serde_json::json;
use vectorcraft_doc::live::{GradientMesh, envelope_grid};
use vectorcraft_doc::{EnvelopeKind, Node, NodeId, NodeKind};
use vectorcraft_geom::Point;

use crate::bbox::move_delta;
use crate::guides::{PointSnap, Targets};
use crate::{Action, Overlay, ToolContext};

/// The editable grid of `n`: a gradient mesh, or a mesh envelope's grid (not while its contents
/// are being edited).
pub fn grid_of(n: &Node) -> Option<Cow<'_, GradientMesh>> {
    match &n.kind {
        NodeKind::Mesh(m) => Some(Cow::Borrowed(m)),
        NodeKind::Envelope { kind: EnvelopeKind::Mesh { rows, cols, points, handles }, editing: false, .. } => {
            envelope_grid(*rows, *cols, points, handles).map(Cow::Owned)
        }
        _ => None,
    }
}

/// The editable meshes (gradient meshes and mesh envelopes) whose points a press can pick: the
/// selected ones first, then the others, topmost first.
pub fn editable_meshes(cx: &ToolContext) -> Vec<NodeId> {
    let mut v: Vec<NodeId> = cx.selection.objects.iter().copied().filter(|id| cx.doc.node(*id).and_then(grid_of).is_some()).collect();
    let mut rest = vec![];
    cx.doc.walk(|n| {
        if grid_of(n).is_some() {
            rest.push(n.id);
        }
    });
    for id in rest.into_iter().rev() {
        if !v.contains(&id) {
            v.push(id);
        }
    }
    v.retain(|id| cx.doc.is_editable(*id));
    v
}

/// The selected points of mesh `id`: its direct-selected ones, all of them when it is selected as
/// a whole, none when it isn't selected.
pub fn selected_points(cx: &ToolContext, id: NodeId, count: usize) -> BTreeSet<usize> {
    match cx.selection.mesh_partial(id) {
        Some(set) => set.clone(),
        None if cx.selection.contains(id) => (0..count).collect(),
        None => BTreeSet::new(),
    }
}

/// The handles that show and drag: those of the direct-selected mesh points (none while more than
/// one is selected with Show handles when multiple anchors are selected off) → (mesh, point,
/// handle slot, where its end is, where its point is).
pub fn shown_handles(cx: &ToolContext) -> Vec<(NodeId, usize, usize, Point, Point)> {
    let mut out = vec![];
    let mut points = 0;
    for (id, set) in &cx.selection.mesh_points {
        let Some(grid) = cx.doc.node(*id).and_then(grid_of) else { continue };
        for &i in set {
            let Some(q) = grid.points.get(i) else { continue };
            points += 1;
            for (h, v) in q.handles.iter().enumerate().filter(|(_, v)| v.hypot() > 1e-6) {
                out.push((*id, i, h, q.p + *v, q.p));
            }
        }
    }
    if !cx.handles_multiple && points > 1 {
        out.clear();
    }
    out
}

/// What a press on a mesh grabs: the mesh, the point and (on a selected point) a handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeshGrab {
    pub id: NodeId,
    pub index: usize,
    pub handle: Option<usize>,
}

/// The mesh point within reach of `p` among the meshes `ids` (in order): (mesh, point, where).
pub fn point_at(cx: &ToolContext, ids: &[NodeId], p: Point) -> Option<(NodeId, usize, Point)> {
    let tol = cx.point_tol();
    ids.iter().filter(|id| cx.doc.is_editable(**id)).find_map(|id| {
        let grid = grid_of(cx.doc.node(*id)?)?;
        let index = grid.point_near(p, tol)?;
        Some((*id, index, grid.points.get(index)?.p))
    })
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    /// The selected points, from `start`; `grab` is where the pressed point was (it snaps).
    Points { start: Point, grab: Point, began: bool },
    /// A handle of a selected point.
    Handle(MeshGrab),
}

/// Point and handle drags on meshes.
#[derive(Default)]
pub struct MeshEdit {
    drag: Option<Drag>,
    /// What the dragged point snaps to, gathered when the drag begins.
    snap: Option<PointSnap>,
    /// Smart guides of the drag.
    guides: Vec<Overlay>,
}

impl MeshEdit {
    /// The end of a shown handle ([`shown_handles`]) nearest to `p` within reach, and nearer to it
    /// than its point (a short handle leaves its point to be picked); else the mesh point under
    /// `p` among the meshes `ids` (editable ones, in order).
    pub fn hit(&self, cx: &ToolContext, ids: &[NodeId], p: Point) -> Option<MeshGrab> {
        let tol = cx.point_tol();
        let handle = shown_handles(cx)
            .into_iter()
            .filter(|(.., end, at)| end.distance(p) <= tol && end.distance(p) < at.distance(p))
            .min_by(|a, b| a.3.distance(p).total_cmp(&b.3.distance(p)));
        if let Some((id, index, h, ..)) = handle {
            return Some(MeshGrab { id, index, handle: Some(h) });
        }
        point_at(cx, ids, p).map(|(id, index, _)| MeshGrab { id, index, handle: None })
    }

    /// Press on `grab` at `p`: a handle starts its drag; a point is selected (Shift adds or
    /// removes it) and the selected points get ready to move.
    pub fn press(&mut self, cx: &ToolContext, grab: MeshGrab, p: Point, shift: bool) -> Vec<Action> {
        if grab.handle.is_some() {
            self.drag = Some(Drag::Handle(grab));
            return vec![Action::Begin("Move Mesh Point".into())];
        }
        let at = cx.doc.node(grab.id).and_then(grid_of).and_then(|g| g.points.get(grab.index).map(|q| q.p)).unwrap_or(p);
        self.press_points(cx, grab.id, &[grab.index], at, p, shift)
    }

    /// Press on the points `indexes` of mesh `id` (a point, or a patch's corners) at `p`, `grab`
    /// being the place that snaps when they move: they get selected (Shift toggles them; already
    /// selected, the selection stays so they all move), ready to drag.
    pub fn press_points(&mut self, cx: &ToolContext, id: NodeId, indexes: &[usize], grab: Point, p: Point, shift: bool) -> Vec<Action> {
        self.drag = Some(Drag::Points { start: p, grab, began: false });
        let count = cx.doc.node(id).and_then(grid_of).map_or(0, |g| g.points.len());
        let selected = selected_points(cx, id, count);
        let select = |mode: &str| vec![Action::Exec("select.meshPoints".into(), json!({"id": id.0, "indexes": indexes, "mode": mode}))];
        if shift {
            select("toggle")
        } else if indexes.iter().all(|i| selected.contains(i)) {
            vec![]
        } else {
            select("set")
        }
    }

    /// The preview of a drag to `p` (`None` when nothing is being dragged); Shift keeps a point
    /// move at 45° steps (unsnapped).
    pub fn drag_to(&mut self, cx: &ToolContext, p: Point, shift: bool) -> Option<Vec<Action>> {
        match self.drag? {
            Drag::Handle(g) => {
                let params = json!({"id": g.id.0, "index": g.index, "x": p.x, "y": p.y, "handle": g.handle});
                Some(vec![Action::Preview("object.mesh.movePoint".into(), params)])
            }
            Drag::Points { start, grab, began } => {
                let mut out = vec![];
                if !began {
                    if p.distance(start) < cx.tol(3.0) {
                        return Some(out);
                    }
                    out.push(Action::Begin("Move".into()));
                    self.drag = Some(Drag::Points { start, grab, began: true });
                    self.snap = Some(PointSnap::new(cx, || Targets::for_anchor_drag(cx.doc, cx.selection)));
                }
                let mut d = move_delta(start, p, shift);
                self.guides.clear();
                if !shift && let Some(snap) = &self.snap {
                    let (q, guides) = snap.snap(cx, grab + d);
                    (d, self.guides) = (q - grab, guides);
                }
                out.push(Action::Preview("path.moveAnchors".into(), json!({"dx": d.x, "dy": d.y})));
                Some(out)
            }
        }
    }

    /// End a drag (`None` when nothing was being dragged).
    pub fn release(&mut self) -> Option<Vec<Action>> {
        let drag = self.drag.take()?;
        self.snap = None;
        self.guides.clear();
        Some(match drag {
            Drag::Points { began: false, .. } => vec![],
            _ => vec![Action::Commit],
        })
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The smart guides of a point drag (the canvas draws the points and handles themselves).
    pub fn overlays(&self) -> Vec<Overlay> {
        self.guides.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::testutil::*;
    use crate::{PointerEvent, PointerKind, Tool};
    use vectorcraft_doc::live::{EnvelopeOptions, grid_points};
    use vectorcraft_doc::{Document, Selection};
    use vectorcraft_geom::{Affine, Rect};

    /// The rectangle of [`doc_with_rect`] (100..200 square) in a 1×1 mesh envelope.
    fn doc_with_envelope() -> (Document, NodeId) {
        let (mut d, a) = doc_with_rect();
        let rect = d.node(a).cloned().unwrap();
        d.remove(a).unwrap();
        let id = d.alloc_id();
        let kind = EnvelopeKind::Mesh { rows: 1, cols: 1, points: grid_points(Rect::new(100.0, 100.0, 200.0, 200.0), 1, 1), handles: vec![] };
        let env = NodeKind::Envelope {
            content: vec![Arc::new(rect)],
            kind,
            fidelity: 50.0,
            editing: false,
            options: EnvelopeOptions::NEW,
            frame: Affine::IDENTITY,
        };
        let l = d.layers[0].id;
        d.insert(Some(l), 0, Node::new(id, env)).unwrap();
        (d, id)
    }

    /// The rectangle of [`doc_with_rect`] as a 2×2 gradient mesh: points 0..9, row-major from
    /// (100, 100) in 50 pt steps (point 4 the centre).
    pub(crate) fn doc_with_mesh() -> (Document, NodeId) {
        let (mut d, a) = doc_with_rect();
        let path = d.node(a).and_then(|n| n.path_data().cloned()).unwrap();
        let m = GradientMesh::for_path(&path, 2, 2, vectorcraft_color::Color::WHITE, Default::default(), 0.0).unwrap();
        *d.node_mut(a).unwrap() = Node::new(a, NodeKind::Mesh(m));
        (d, a)
    }

    /// Selection with points `indexes` of mesh `id` direct-selected.
    pub(crate) fn points_of(id: NodeId, indexes: &[usize]) -> Selection {
        let mut s = Selection::default();
        s.set([id]);
        s.mesh_points.insert(id, indexes.iter().copied().collect());
        s
    }

    fn press(t: &mut dyn Tool, c: &ToolContext, x: f64, y: f64, mods: crate::Mods) -> Vec<Action> {
        t.pointer(c, &PointerEvent::new(PointerKind::Down, x, y).with_mods(mods))
    }

    #[test]
    fn the_mesh_point_grid_is_row_major() {
        let (d, m) = doc_with_mesh();
        let grid = grid_of(d.node(m).unwrap()).unwrap();
        let at = |i: usize| grid.points[i].p;
        assert_eq!(grid.points.len(), 9);
        let corners = [at(0), at(2), at(6), at(8)];
        for c in [Point::new(100.0, 100.0), Point::new(200.0, 100.0), Point::new(100.0, 200.0), Point::new(200.0, 200.0)] {
            assert!(corners.iter().any(|q| q.distance(c) < 1e-6), "{c:?} in {corners:?}");
        }
        assert!(at(4).distance(Point::new(150.0, 150.0)) < 1e-6);
    }

    #[test]
    fn the_mesh_tool_selects_and_drags_envelope_points_and_adds_lines() {
        let (d, e) = doc_with_envelope();
        let (s, p) = (Selection::default(), paint());
        let c = cx(&d, &s, &p);
        let mut t = crate::meshblend::create("mesh").unwrap();
        // A press on a point selects it, on a mesh that wasn't selected...
        assert_eq!(
            press(t.as_mut(), &c, 200.0, 100.0, Default::default()),
            vec![Action::Exec("select.meshPoints".into(), json!({"id": e.0, "indexes": [1], "mode": "set"}))]
        );
        // ...and the drag moves the selected points.
        let s = points_of(e, &[1]);
        let c = cx(&d, &s, &p);
        let a = t.pointer(&c, &PointerEvent::new(PointerKind::Drag, 210.0, 90.0));
        assert_eq!(a, vec![Action::Begin("Move".into()), Action::Preview("path.moveAnchors".into(), json!({"dx": 10.0, "dy": -10.0}))]);
        assert_eq!(t.pointer(&c, &PointerEvent::new(PointerKind::Up, 210.0, 90.0)), vec![Action::Commit]);
        // The selected point's handles (the smooth mesh's) can be dragged.
        assert_eq!(press(t.as_mut(), &c, 200.0, 133.33333, Default::default()), vec![Action::Begin("Move Mesh Point".into())]);
        let drag = t.pointer(&c, &PointerEvent::new(PointerKind::Drag, 220.0, 140.0));
        assert!(matches!(&drag[..], [Action::Preview(id, p)] if id == "object.mesh.movePoint" && p["handle"] == json!(2)), "{drag:?}");
        t.pointer(&c, &PointerEvent::new(PointerKind::Up, 220.0, 140.0));
        // Inside the envelope: a new row and column.
        assert_eq!(
            press(t.as_mut(), &c, 150.0, 160.0, Default::default()),
            vec![Action::Exec("object.mesh.addLine".into(), json!({"id": e.0, "x": 150.0, "y": 160.0}))]
        );
    }

    /// Direct Selection picks a mesh point even on an unselected mesh, Shift toggles points, and
    /// pressing a selected point keeps the selection so all the selected points move together.
    #[test]
    fn direct_selection_selects_mesh_points_on_any_mesh() {
        let (d, m) = doc_with_mesh();
        let p = paint();
        let shift = crate::Mods { shift: true, ..Default::default() };
        let mut t = crate::direct::DirectSelectionTool::new(false);
        let none = Selection::default();
        let c = cx(&d, &none, &p);
        assert_eq!(
            press(&mut t, &c, 151.0, 149.0, Default::default()),
            vec![Action::Exec("select.meshPoints".into(), json!({"id": m.0, "indexes": [4], "mode": "set"}))]
        );
        t.pointer(&c, &PointerEvent::new(PointerKind::Up, 151.0, 149.0));
        let s = points_of(m, &[4, 0]);
        let c = cx(&d, &s, &p);
        assert_eq!(
            press(&mut t, &c, 100.0, 150.0, shift),
            vec![Action::Exec("select.meshPoints".into(), json!({"id": m.0, "indexes": [3], "mode": "toggle"}))]
        );
        t.pointer(&c, &PointerEvent::new(PointerKind::Up, 100.0, 150.0));
        assert_eq!(press(&mut t, &c, 150.0, 150.0, Default::default()), vec![], "already selected: the selection stays");
        let a = t.pointer(&c, &PointerEvent::new(PointerKind::Drag, 160.0, 150.0));
        assert_eq!(a.first(), Some(&Action::Begin("Move".into())));
        assert!(matches!(a.last(), Some(Action::Preview(c, _)) if c == "path.moveAnchors"), "{a:?}");
        assert_eq!(t.pointer(&c, &PointerEvent::new(PointerKind::Up, 160.0, 150.0)), vec![Action::Commit]);
        // A click without a drag commits nothing.
        press(&mut t, &c, 150.0, 150.0, Default::default());
        assert_eq!(t.pointer(&c, &PointerEvent::new(PointerKind::Up, 150.0, 150.0)), vec![]);
    }

    /// A click inside a patch selects its four corners; Alt-click selects the whole mesh.
    #[test]
    fn direct_selection_selects_a_patch() {
        let (d, m) = doc_with_mesh();
        let (s, p) = (Selection::default(), paint());
        let c = cx(&d, &s, &p);
        let mut t = crate::direct::DirectSelectionTool::new(false);
        let a = press(&mut t, &c, 125.0, 175.0, Default::default());
        let [Action::Exec(cmd, v)] = &a[..] else { panic!("{a:?}") };
        assert_eq!((cmd.as_str(), &v["id"], &v["mode"]), ("select.meshPoints", &json!(m.0), &json!("set")));
        let mut got: Vec<u64> = v["indexes"].as_array().unwrap().iter().map(|i| i.as_u64().unwrap()).collect();
        got.sort();
        assert_eq!(got, vec![3, 4, 6, 7]);
        t.pointer(&c, &PointerEvent::new(PointerKind::Up, 125.0, 175.0));
        let alt = crate::Mods { alt: true, ..Default::default() };
        assert_eq!(press(&mut t, &c, 125.0, 175.0, alt), vec![Action::Exec("select.set".into(), json!({"ids": [m.0]}))]);
    }

    /// A marquee takes the mesh points inside it (Shift toggles), with the anchors of paths.
    #[test]
    fn the_direct_selection_marquee_takes_mesh_points() {
        let (d, m) = doc_with_mesh();
        let (s, p) = (Selection::default(), paint());
        let c = cx(&d, &s, &p);
        let mut t = crate::direct::DirectSelectionTool::new(false);
        press(&mut t, &c, 140.0, 90.0, Default::default());
        t.pointer(&c, &PointerEvent::new(PointerKind::Drag, 210.0, 160.0));
        let a = t.pointer(&c, &PointerEvent::new(PointerKind::Up, 210.0, 160.0));
        let [Action::Exec(cmd, v)] = &a[..] else { panic!("{a:?}") };
        assert_eq!(cmd, "select.anchorsMany");
        assert_eq!(v["items"], json!([{"id": m.0, "meshPoints": [1, 2, 4, 5]}]));
    }

    /// The selected points' handles are the ones hit; none with Show handles when multiple anchors
    /// are selected off and several points selected.
    #[test]
    fn only_the_selected_points_handles_drag() {
        let (d, m) = doc_with_mesh();
        let p = paint();
        let grid = grid_of(d.node(m).unwrap()).unwrap().into_owned();
        let centre = &grid.points[4];
        let end = centre.p + centre.handles[0];
        let e = MeshEdit::default();
        let ids = [m];
        let one = points_of(m, &[4]);
        assert_eq!(e.hit(&cx(&d, &one, &p), &ids, end), Some(MeshGrab { id: m, index: 4, handle: Some(0) }));
        let other = points_of(m, &[0]);
        assert_eq!(e.hit(&cx(&d, &other, &p), &ids, end), None, "point 4 isn't selected");
        let two = points_of(m, &[4, 0]);
        assert!(e.hit(&cx(&d, &two, &p), &ids, end).is_some());
        let off = ToolContext { handles_multiple: false, ..cx(&d, &two, &p) };
        assert_eq!(e.hit(&off, &ids, end), None);
    }
}
