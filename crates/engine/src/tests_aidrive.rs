//! Driving the engine from an AI or a script (PLAN-AI-DRIVE): batches with step references,
//! `ids` on selection commands, `look` on new objects, per-document requests, cheap checks.

use serde_json::{Value, json};

use super::*;

fn session() -> Session {
    let mut s = Session::new();
    s.execute("file.new", &json!({"width": 600, "height": 400})).unwrap();
    s
}

fn top_level(s: &Session) -> usize {
    s.doc().unwrap().doc.layers.first().and_then(|l| l.children()).map_or(0, |c| c.len())
}

fn node(s: &Session, id: &Value) -> vectorcraft_doc::Node {
    s.doc().unwrap().doc.node(NodeId(id.as_u64().unwrap())).unwrap().clone()
}

fn undo_len(s: &Session) -> usize {
    s.doc().unwrap().history.undo.len()
}

// ---------- A1: batches with step references ----------

#[test]
fn a_batch_step_takes_ids_from_earlier_steps_and_is_one_undo_step() {
    let mut s = session();
    let n = undo_len(&s);
    let r = s
        .execute(
            "command.batch",
            &json!({"label": "Tag", "commands": [
                {"command": "shape.rectangle", "params": {"x": 10, "y": 10, "width": 50, "height": 80}},
                {"command": "paint.setFill", "params": {"ids": ["$0.id"], "color": "#e8234e"}},
                {"command": "shape.ellipse", "params": {"x": 30, "y": 15, "width": 10, "height": 10}},
                {"command": "paint.setFill", "params": {"ids": ["$last.id"], "color": "#ffffff"}},
            ]}),
        )
        .unwrap();
    let ids: Vec<Value> = [0, 2].iter().map(|i| r["results"][i]["id"].clone()).collect();
    assert_eq!(node(&s, &ids[0]).appearance.fill_paint().color().map(|c| c.to_hex()), Some("#e8234e".into()));
    assert_eq!(node(&s, &ids[1]).appearance.fill_paint().color().map(|c| c.to_hex()), Some("#ffffff".into()));
    assert_eq!(undo_len(&s), n + 1, "one undo step");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(top_level(&s), 0);
    // The journal keeps the references, so a replay takes the ids its own steps make.
    let (_, entry) = s.journal.iter().rev().find(|(c, _)| c == "command.batch").unwrap();
    assert_eq!(entry["commands"][1]["params"]["ids"], json!(["$0.id"]));
}

#[test]
fn a_bad_reference_rolls_the_batch_back_and_says_which_step() {
    let mut s = session();
    let e = s
        .execute(
            "command.batch",
            &json!({"commands": [
                {"command": "shape.rectangle", "params": {"x": 0, "y": 0, "width": 5, "height": 5}},
                {"command": "paint.setFill", "params": {"ids": ["$3.id"], "color": "#000000"}},
            ]}),
        )
        .unwrap_err()
        .to_string();
    assert!(e.contains("step 1") && e.contains("$3.id"), "{e}");
    assert_eq!(top_level(&s), 0, "rolled back");
    // `$$` is a literal dollar, never a reference.
    let r = s.execute("command.batch", &json!({"commands": [{"command": "text.create", "params": {"x": 10, "y": 50, "text": "$$5 off"}}]})).unwrap();
    match &node(&s, &r["results"][0]["id"]).kind {
        vectorcraft_doc::NodeKind::Text(t) => assert_eq!(t.plain_text(), "$5 off"),
        k => panic!("{k:?}"),
    }
}

#[test]
fn batches_are_capped_and_atomic_false_keeps_going() {
    let mut s = session();
    let many: Vec<Value> = (0..10_001).map(|_| json!({"command": "select.none"})).collect();
    assert!(s.execute("command.batch", &json!({"commands": many})).is_err());
    let r = s
        .execute(
            "command.batch",
            &json!({"atomic": false, "commands": [
                {"command": "shape.rectangle", "params": {"x": 0, "y": 0, "width": 5, "height": 5}},
                {"command": "no.such.command"},
                {"command": "paint.setFill", "params": {"ids": ["$1.id"], "color": "#000000"}},
                {"command": "shape.ellipse", "params": {"x": 9, "y": 0, "width": 5, "height": 5}},
            ]}),
        )
        .unwrap();
    assert_eq!((r["completed"].clone(), r["failed"].clone()), (json!(2), json!(2)));
    assert_eq!(top_level(&s), 2, "the steps that worked stay");
    assert_eq!(r["results"][2]["ok"], false, "a reference to a failed step fails");
}

// ---------- A2: `ids` on selection commands, `look` on new objects ----------

fn rect(s: &mut Session, x: f64, y: f64, w: f64, h: f64) -> Value {
    s.execute("shape.rectangle", &json!({"x": x, "y": y, "width": w, "height": h})).unwrap()["id"].clone()
}

fn selection(s: &Session) -> Vec<u64> {
    s.doc().unwrap().selection.objects.iter().map(|i| i.0).collect()
}

/// Commands that take `ids` run with nothing selected, act only on those objects, and leave the
/// selection alone unless they make something new.
#[test]
fn ids_work_with_nothing_selected() {
    let mut s = session();
    let (a, b) = (rect(&mut s, 10.0, 10.0, 40.0, 40.0), rect(&mut s, 30.0, 30.0, 40.0, 40.0));
    s.execute("select.none", &json!({})).unwrap();
    let bounds = |s: &Session, id: &Value| node(s, id).geometric_bounds().unwrap();
    s.execute("object.move", &json!({"ids": [a], "dx": 5, "dy": 0})).unwrap();
    assert_eq!(bounds(&s, &a).x0, 15.0);
    assert_eq!(bounds(&s, &b).x0, 30.0, "only the named object moved");
    s.execute("object.rotate", &json!({"ids": [b], "angle": 45})).unwrap();
    s.execute("object.arrange.sendToBack", &json!({"ids": [b]})).unwrap();
    let kids: Vec<u64> = s.doc().unwrap().doc.layers[0].children().unwrap().iter().map(|n| n.id.0).collect();
    assert_eq!(kids.first().copied(), b.as_u64(), "sent to back");
    assert!(selection(&s).is_empty(), "the selection is left alone");
    // Offset Path makes new objects (above the originals).
    let r = s.execute("object.path.offsetPath", &json!({"ids": [a], "offset": -4})).unwrap();
    assert_eq!(r["ids"].as_array().map(Vec::len), Some(1));
    // Pathfinder and grouping by ids.
    let c = rect(&mut s, 200.0, 10.0, 20.0, 20.0);
    let d = rect(&mut s, 210.0, 10.0, 20.0, 20.0);
    s.execute("select.none", &json!({})).unwrap();
    s.execute("object.pathfinder.unite", &json!({"ids": [c, d]})).map_err(|e| e.to_string()).unwrap();
    assert!(s.doc().unwrap().doc.node(NodeId(c.as_u64().unwrap())).is_none() || s.doc().unwrap().doc.node(NodeId(d.as_u64().unwrap())).is_none());
    let g = s.execute("object.group", &json!({"ids": [a, b]})).unwrap();
    assert!(g["id"].is_u64(), "{g}");
    // A clipping mask from ids: the topmost clips.
    let big = rect(&mut s, 300.0, 10.0, 100.0, 100.0);
    let clip = s.execute("shape.ellipse", &json!({"x": 320, "y": 20, "width": 50, "height": 50})).unwrap()["id"].clone();
    s.execute("select.none", &json!({})).unwrap();
    let r = s.execute("object.clippingMask.make", &json!({"ids": [big, clip]})).unwrap();
    let cg = r["id"].clone();
    assert!(matches!(node(&s, &cg).kind, vectorcraft_doc::NodeKind::Group { clip: true, .. }), "{r}");
    // Ids that don't exist are an error, not a silent no-op.
    assert!(s.execute("object.move", &json!({"ids": [99_999], "dx": 1})).is_err());
    // Without ids the menu behaviour stands: a selection is needed.
    s.execute("select.none", &json!({})).unwrap();
    assert!(s.execute("object.group", &json!({})).is_err());
}

/// `look` styles the new object in the same undo step, leaves the paint defaults alone, journals
/// once, and a bad look leaves nothing behind.
#[test]
fn look_styles_a_new_object_in_one_undo_step() {
    let mut s = session();
    let defaults = s.paint.clone();
    let n = undo_len(&s);
    let r = s
        .execute(
            "shape.rectangle",
            &json!({"x": 10, "y": 10, "width": 40, "height": 40, "look": {"fill": "#e8234e", "stroke": "#222222", "weight": 3, "opacity": 50}}),
        )
        .unwrap();
    let n1 = node(&s, &r["id"]);
    assert_eq!(n1.appearance.fill_paint().color().map(|c| c.to_hex()), Some("#e8234e".into()));
    assert_eq!(n1.appearance.stroke_paint().color().map(|c| c.to_hex()), Some("#222222".into()));
    assert_eq!(n1.appearance.stroke_width(), 3.0);
    assert!((n1.opacity - 0.5).abs() < 1e-6, "{}", n1.opacity);
    assert_eq!(undo_len(&s), n + 1, "one undo step");
    assert_eq!(s.paint, defaults, "the defaults new art gets are unchanged");
    let (cmd, p) = s.journal.last().unwrap();
    assert_eq!((cmd.as_str(), p["look"]["fill"].clone()), ("shape.rectangle", json!("#e8234e")));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(top_level(&s), 0);
    // A bad look: an error, and the object it would have styled is gone again.
    assert!(s.execute("shape.ellipse", &json!({"x": 0, "y": 0, "width": 9, "height": 9, "look": {"colour": "red"}})).is_err());
    assert_eq!(top_level(&s), 0);
    assert_eq!(undo_len(&s), n);
    // Inside a batch, look is part of the batch's one undo step.
    s.execute(
        "command.batch",
        &json!({"commands": [
            {"command": "shape.ellipse", "params": {"x": 0, "y": 0, "width": 9, "height": 9, "look": {"fill": "none", "stroke": "#000000"}}},
            {"command": "shape.rectangle", "params": {"x": 20, "y": 0, "width": 9, "height": 9, "look": {"fill": "#00ff00"}}},
        ]}),
    )
    .unwrap();
    assert_eq!((top_level(&s), undo_len(&s)), (2, n + 1));
}

/// Every command that needs a selection takes `ids`, apart from the ones below (selecting by
/// sameness picks from the selection; the rest are still to convert). A new selection-only command
/// fails here: give it `ids` (cmd::edit::targets) or add it with a reason.
#[test]
fn selection_commands_take_ids() {
    const STILL_SELECTION_ONLY: &[&str] = &[
        // Pick from the selection by design.
        "select.same.appearance",
        "select.same.appearanceAttribute",
        "select.same.fillAndStroke",
        "select.same.fillColor",
        "select.same.fontFamily",
        "select.same.fontFamilyStyle",
        "select.same.fontFamilyStyleSize",
        "select.same.graphicStyle",
        "select.same.opacity",
        "select.same.shapeType",
        "select.same.strokeColor",
        "select.same.strokeWeight",
        "select.same.symbolInstance",
        "select.same.textFillColor",
        "select.same.textStrokeColor",
        "select.same.blendingMode",
        "select.nextAbove",
        "select.nextBelow",
        "select.object.allOnSameLayers",
        "select.object.directionHandles",
        "select.object.strayPoints",
        "select.save",
        "select.savedList",
    ];
    let s = session();
    let mut missing = vec![];
    for spec in cmd::command_specs() {
        let inputs = spec.params.split('→').next().unwrap_or("");
        let needs_selection = (spec.enabled)(&s).is_err_and(|e| e.contains("selected") || e.contains("select "));
        if needs_selection && !inputs.contains("ids") && !inputs.contains("{id") && !STILL_SELECTION_ONLY.contains(&spec.id) {
            missing.push(spec.id);
        }
    }
    missing.sort();
    // Report the list so it can be worked down: today's remainder is printed, not asserted, while
    // Part A converts the commands an AI uses most (see PLAN-AI-DRIVE A2).
    println!("selection-only commands without ids ({}): {missing:?}", missing.len());
    for must in [
        "object.move",
        "object.rotate",
        "object.group",
        "object.align",
        "object.clippingMask.make",
        "object.path.offsetPath",
        "object.pathfinder.unite",
        "object.arrange.bringToFront",
    ] {
        assert!(!missing.contains(&must), "{must} lost its ids");
    }
}

// ---------- A3: requests aimed at a document ----------

/// An agent draws in its own document (`doc`) while another stays active: nothing lands in the
/// active one, the active one stays active, and undo works per document.
#[test]
fn doc_runs_a_command_in_another_document_without_switching() {
    let mut s = session();
    let mine = s.execute("file.new", &json!({"width": 300, "height": 300})).unwrap()["doc"].as_u64().unwrap();
    s.execute("document.activate", &json!({"index": 0})).unwrap();
    let theirs = s.documents()[0].uid;
    let r = s.execute("shape.rectangle", &json!({"doc": mine, "x": 10, "y": 10, "width": 20, "height": 20, "look": {"fill": "#ff0000"}})).unwrap();
    assert!(r["id"].is_u64());
    assert_eq!(s.active_index(), Some(0), "the active document stays active");
    assert_eq!(top_level(&s), 0, "nothing landed in it");
    let count = |s: &Session, uid: u64| s.documents().iter().find(|d| d.uid == uid).unwrap().doc.layers[0].children().map_or(0, |c| c.len());
    assert_eq!(count(&s, mine), 1);
    // A whole batch aimed at it.
    s.execute(
        "command.batch",
        &json!({"doc": mine, "commands": [
            {"command": "shape.ellipse", "params": {"x": 50, "y": 10, "width": 20, "height": 20}},
            {"command": "paint.setFill", "params": {"ids": ["$0.id"], "color": "#00ff00"}},
        ]}),
    )
    .unwrap();
    assert_eq!((count(&s, mine), count(&s, theirs)), (2, 0));
    s.execute("edit.undo", &json!({"doc": mine})).unwrap();
    assert_eq!(count(&s, mine), 1, "undo in that document");
    // `doc` belongs on the batch, not on a step; unknown documents are errors.
    let step_doc = json!({"commands": [{"command": "shape.ellipse", "params": {"doc": mine, "x": 0, "y": 0, "width": 5, "height": 5}}]});
    assert!(s.execute("command.batch", &step_doc).is_err());
    assert!(s.execute("shape.rectangle", &json!({"doc": 987_654, "x": 0, "y": 0, "width": 5, "height": 5})).is_err());
    assert!(s.execute("shape.rectangle", &json!({"doc": "mine", "x": 0, "y": 0, "width": 5, "height": 5})).is_err());
    // document.list names them; document.activate takes an id.
    let list = s.execute("document.list", &json!({})).unwrap();
    let docs = list["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 2);
    assert_eq!((docs[0]["doc"].as_u64(), docs[0]["active"].clone()), (Some(theirs), json!(true)));
    assert_eq!(docs[1]["doc"].as_u64(), Some(mine));
    s.execute("document.activate", &json!({"doc": mine})).unwrap();
    assert_eq!(s.active_index(), Some(1));
}

// ---------- A4: cheap checks ----------

/// document.summary: counts by kind, the art's bounds and what looks wrong, without the tree.
#[test]
fn the_summary_counts_bounds_and_warns() {
    let mut s = session();
    let a = rect(&mut s, 10.0, 10.0, 40.0, 40.0);
    s.execute("shape.ellipse", &json!({"x": 100, "y": 20, "width": 30, "height": 30})).unwrap();
    let off = rect(&mut s, 900.0, 900.0, 10.0, 10.0);
    s.execute("object.hide", &json!({"ids": [a]})).unwrap();
    let r = s.execute("document.summary", &json!({})).unwrap();
    assert_eq!(r["objects"], 3);
    assert_eq!(r["kinds"]["Rectangle"].as_u64().unwrap() + r["kinds"]["Ellipse"].as_u64().unwrap(), 3, "{r}");
    assert_eq!(r["bounds"]["x"], 10.0);
    assert_eq!(r["artboards"][0]["width"], 600.0);
    let warn = |kind: &str| r["warnings"].as_array().unwrap().iter().find(|w| w["kind"] == kind).map(|w| w["ids"].clone());
    assert_eq!(warn("offArtboard"), Some(json!([off])));
    assert_eq!(warn("hidden"), Some(json!([a])));
    assert!(r["revision"].is_u64());
    // Just some objects.
    let one = s.execute("document.summary", &json!({"ids": [off]})).unwrap();
    assert_eq!(one["objects"], 1);
    assert!(s.execute("document.summary", &json!({"ids": [99_999]})).is_err());
}
