//! Mesh points as a selection (`Selection::mesh_points`): selecting them, and the commands that
//! act on the selected points (move, nudge, Delete, fill colour, eyedropper, the mesh commands).

use serde_json::{Value, json};
use vectorcraft_doc::{GradientMesh, NodeKind};

use super::*;

fn session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({"width": 800, "height": 600})).unwrap();
    s
}

/// A 100 pt square at the origin as a 2×2 gradient mesh, white: points 0..9 row-major, 50 pt
/// apart (4 the centre), the mesh selected as a whole.
fn mesh_session() -> (Session, NodeId) {
    let mut s = session();
    let r = s.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 100, "height": 100})).unwrap();
    let a = NodeId(r["id"].as_u64().unwrap());
    s.execute("paint.setFill", &json!({"ids": [a.0], "color": "#ffffff"})).unwrap();
    s.execute("select.set", &json!({"ids": [a.0]})).unwrap();
    s.execute("object.mesh.create", &json!({"rows": 2, "cols": 2})).unwrap();
    (s, a)
}

fn undo_len(s: &Session) -> usize {
    s.doc().unwrap().history.undo.len()
}

fn mesh_of(s: &Session, id: NodeId) -> GradientMesh {
    match &s.doc().unwrap().doc.node(id).unwrap().kind {
        NodeKind::Mesh(m) => m.clone(),
        k => panic!("{k:?}"),
    }
}

fn picked(s: &Session, id: NodeId) -> Option<Vec<usize>> {
    s.doc().unwrap().selection.mesh_partial(id).map(|set| set.iter().copied().collect())
}

fn select(s: &mut Session, id: NodeId, indexes: &[usize], mode: &str) -> Value {
    s.execute("select.meshPoints", &json!({"id": id.0, "indexes": indexes, "mode": mode})).unwrap()
}

fn colors(m: &GradientMesh) -> Vec<String> {
    m.points.iter().map(|q| q.color.to_hex()).collect()
}

#[test]
fn mesh_points_are_selected_toggled_and_subtracted() {
    let (mut s, a) = mesh_session();
    select(&mut s, a, &[4], "set");
    assert_eq!(picked(&s, a), Some(vec![4]));
    assert!(s.doc().unwrap().selection.contains(a));
    select(&mut s, a, &[0, 4], "toggle");
    assert_eq!(picked(&s, a), Some(vec![0]));
    select(&mut s, a, &[8], "add");
    assert_eq!(picked(&s, a), Some(vec![0, 8]));
    select(&mut s, a, &[0, 8], "subtract");
    assert!(!s.doc().unwrap().selection.contains(a), "no point left: the mesh leaves the selection");
    // All of them: the mesh is selected as a whole.
    select(&mut s, a, &(0..9).collect::<Vec<_>>(), "set");
    assert_eq!(picked(&s, a), None);
    assert!(s.doc().unwrap().selection.contains(a));
    // Set with none: the mesh shows its points, none selected.
    select(&mut s, a, &[], "set");
    assert_eq!(picked(&s, a), Some(vec![]));
    // Out-of-range indexes are left out; a path isn't a mesh.
    select(&mut s, a, &[2, 99], "set");
    assert_eq!(picked(&s, a), Some(vec![2]));
    let r = s.execute("shape.rectangle", &json!({"x": 300, "y": 0, "width": 10, "height": 10})).unwrap();
    assert!(s.execute("select.meshPoints", &json!({"id": r["id"], "indexes": [0]})).is_err());
    // Selecting objects clears the points.
    s.execute("select.set", &json!({"ids": [a.0]})).unwrap();
    assert_eq!(picked(&s, a), None);
}

/// The marquee and Lasso's command carries mesh points beside path anchors.
#[test]
fn anchors_many_takes_mesh_points() {
    let (mut s, a) = mesh_session();
    let r = s.execute("shape.rectangle", &json!({"x": 300, "y": 0, "width": 10, "height": 10})).unwrap();
    let items = json!([{"id": a.0, "meshPoints": [1, 2]}, {"id": r["id"], "anchors": [[0, 0]]}]);
    s.execute("select.anchorsMany", &json!({"items": items})).unwrap();
    assert_eq!(picked(&s, a), Some(vec![1, 2]));
    assert_eq!(s.doc().unwrap().selection.objects.len(), 2);
    s.execute("select.anchorsMany", &json!({"items": [{"id": a.0, "meshPoints": [2, 5]}], "mode": "toggle"})).unwrap();
    assert_eq!(picked(&s, a), Some(vec![1, 5]));
}

/// Dragging (path.moveAnchors) and the arrow keys (object.nudge) move only the selected points,
/// handles and all; one undo step each, and undo brings back the selection too.
#[test]
fn selected_mesh_points_move_and_nudge_alone() {
    let (mut s, a) = mesh_session();
    let before = mesh_of(&s, a);
    select(&mut s, a, &[4, 5], "set");
    s.execute("path.moveAnchors", &json!({"dx": 10, "dy": -5})).unwrap();
    let m = mesh_of(&s, a);
    assert_eq!(m.points[4].p, before.points[4].p + vectorcraft_geom::Vec2::new(10.0, -5.0));
    assert_eq!(m.points[5].p, before.points[5].p + vectorcraft_geom::Vec2::new(10.0, -5.0));
    assert_eq!(m.points[4].handles, before.points[4].handles, "the handles follow their point");
    assert_eq!(m.points[0].p, before.points[0].p);
    // The arrow keys: 1 pt by default, ×10 with Shift.
    s.execute("object.nudge", &json!({"dx": 1, "dy": 0, "big": true})).unwrap();
    assert_eq!(mesh_of(&s, a).points[4].p.x, before.points[4].p.x + 20.0);
    assert_eq!(mesh_of(&s, a).points[1].p, before.points[1].p);
    // Switching to another point keeps nothing stale; undo restores the points and the selection.
    select(&mut s, a, &[0], "set");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(mesh_of(&s, a).points[4].p.x, before.points[4].p.x + 10.0);
    assert_eq!(picked(&s, a), Some(vec![4, 5]));
    // A mesh selected as a whole moves whole.
    s.execute("select.set", &json!({"ids": [a.0]})).unwrap();
    s.execute("object.nudge", &json!({"dx": 0, "dy": 1})).unwrap();
    assert_eq!(mesh_of(&s, a).points[0].p, before.points[0].p + vectorcraft_geom::Vec2::new(0.0, 1.0));
}

/// A swatch or the Colour panel (paint.setFill) colours the selected points only, and every point
/// of a mesh selected as a whole; the Fill proxy shows the selected point's colour.
#[test]
fn a_fill_colour_goes_to_the_selected_mesh_points() {
    let (mut s, a) = mesh_session();
    select(&mut s, a, &[4, 8], "set");
    s.execute("paint.setFill", &json!({"color": "#ff0000"})).unwrap();
    let c = colors(&mesh_of(&s, a));
    assert_eq!((c[4].as_str(), c[8].as_str(), c[0].as_str()), ("#ff0000", "#ff0000", "#ffffff"));
    assert!(matches!(s.proxy_paints().0, Paint::Solid { color, .. } if color.to_hex() == "#ff0000"));
    select(&mut s, a, &[0], "set");
    assert!(matches!(s.proxy_paints().0, Paint::Solid { color, .. } if color.to_hex() == "#ffffff"));
    // The whole mesh: all its points.
    s.execute("select.set", &json!({"ids": [a.0]})).unwrap();
    s.execute("paint.setFill", &json!({"color": "#0000ff"})).unwrap();
    assert!(colors(&mesh_of(&s, a)).iter().all(|c| c == "#0000ff"));
    // One undo step.
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(colors(&mesh_of(&s, a))[4], "#ff0000");
}

/// The Eyedropper on a filled object gives the selected points its fill colour.
#[test]
fn the_eyedropper_colours_the_selected_mesh_points() {
    let (mut s, a) = mesh_session();
    let r = s.execute("shape.rectangle", &json!({"x": 300, "y": 0, "width": 10, "height": 10})).unwrap();
    s.execute("paint.setFill", &json!({"ids": [r["id"]], "color": "#00ff00"})).unwrap();
    select(&mut s, a, &[4], "set");
    s.execute("appearance.copyFrom", &json!({"source": r["id"]})).unwrap();
    let c = colors(&mesh_of(&s, a));
    assert_eq!((c[4].as_str(), c[3].as_str()), ("#00ff00", "#ffffff"));
}

/// Delete removes the interior mesh lines through the selected points (border lines stay; a
/// selection of corners only changes nothing and records no undo step).
#[test]
fn delete_removes_the_lines_through_the_selected_mesh_points() {
    let (mut s, a) = mesh_session();
    select(&mut s, a, &[0, 8], "set");
    let n = undo_len(&s);
    s.execute("edit.clear", &json!({})).unwrap();
    assert_eq!(undo_len(&s), n, "corners: nothing to delete");
    assert!(s.doc().unwrap().doc.node(a).is_some(), "the mesh stays");
    select(&mut s, a, &[4], "set");
    s.execute("edit.clear", &json!({})).unwrap();
    let m = mesh_of(&s, a);
    assert_eq!((m.rows, m.cols), (1, 1));
    assert_eq!(picked(&s, a), Some(vec![]), "the mesh stays selected, no point");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(mesh_of(&s, a).rows, 2);
    assert_eq!(picked(&s, a), Some(vec![4]));
}

/// The mesh commands take several points (`indexes`) and default to the selected ones.
#[test]
fn mesh_commands_take_indexes_or_the_selection() {
    let (mut s, a) = mesh_session();
    let r = s.execute("object.mesh.setPointColor", &json!({"id": a.0, "indexes": [0, 1], "color": "#123456"})).unwrap();
    assert_eq!(r["changed"], 2);
    select(&mut s, a, &[2], "set");
    s.execute("object.mesh.setPointColor", &json!({"opacity": 0.25})).unwrap();
    let m = mesh_of(&s, a);
    assert_eq!((colors(&m)[1].as_str(), colors(&m)[2].as_str()), ("#123456", "#ffffff"));
    assert!((m.points[2].opacity - 0.25).abs() < 1e-6 && m.points[1].opacity == 1.0);
    // deletePoint: the selected points with no index.
    select(&mut s, a, &[3], "set");
    s.execute("object.mesh.deletePoint", &json!({"id": a.0})).unwrap();
    assert_eq!((mesh_of(&s, a).rows, mesh_of(&s, a).cols), (1, 2));
    s.execute("select.none", &json!({})).unwrap();
    assert!(s.execute("object.mesh.deletePoint", &json!({})).is_err(), "nothing selected");
}

/// The Mesh tool's clicks: the new point is selected (a new mesh's too).
#[test]
fn a_new_mesh_point_is_selected() {
    let (mut s, a) = mesh_session();
    let r = s.execute("object.mesh.addLine", &json!({"id": a.0, "x": 20, "y": 30})).unwrap();
    let i = r["index"].as_u64().unwrap() as usize;
    assert_eq!(picked(&s, a), Some(vec![i]));
    let e = s.execute("shape.ellipse", &json!({"x": 300, "y": 0, "width": 100, "height": 80})).unwrap();
    s.execute("select.set", &json!({"ids": [e["id"]]})).unwrap();
    let r = s.execute("object.mesh.create", &json!({"at": [350, 40]})).unwrap();
    let e = NodeId(e["id"].as_u64().unwrap());
    assert_eq!(picked(&s, e), Some(vec![r["index"].as_u64().unwrap() as usize]));
    assert_eq!(s.doc().unwrap().selection.objects, vec![e]);
}

/// A structural change elsewhere (undo past the line that made the point) drops indexes the mesh
/// no longer has.
#[test]
fn stale_mesh_point_indexes_are_pruned() {
    let (mut s, a) = mesh_session();
    s.execute("object.mesh.addLine", &json!({"id": a.0, "x": 20, "y": 30})).unwrap();
    s.execute("select.meshPoints", &json!({"id": a.0, "indexes": [15], "mode": "set"})).unwrap();
    assert_eq!(picked(&s, a), Some(vec![15]));
    s.execute("object.mesh.deletePoint", &json!({"id": a.0, "index": 5})).unwrap();
    assert!(picked(&s, a).is_some_and(|v| v.iter().all(|i| *i < mesh_of(&s, a).points.len())));
}
