//! `document.summary`: a short answer to "what is in this document now?" for agents checking their
//! work, instead of the whole layer tree (`document.inspect`) or a screenshot.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use vectorcraft_doc::{Node, NodeKind};
use vectorcraft_geom::Rect;

use super::*;

/// The most ids a warning lists.
const MAX_LISTED: usize = 20;

pub fn specs() -> Vec<CommandSpec> {
    vec![cmd!(
        query "document.summary",
        "Document Summary",
        [],
        None,
        "{ids?: [id…] (only these objects and what they hold)} → {revision, artboards: [{name, x, y, width, height}], objects: total, kinds: {kind: count}, bounds: {x, y, width, height}|null (all the art), selection: [ids], warnings: [{kind: offArtboard|emptyPath|hidden|locked|zeroSize, count, ids (at most 20)}]} a short check of the art without the whole tree",
        has_doc,
        summary
    )]
}

fn rect_json(r: Rect) -> Value {
    json!({"x": r.x0, "y": r.y0, "width": r.width(), "height": r.height()})
}

fn summary(s: &mut Session, p: &Value) -> Result<Value> {
    let st = s.doc()?;
    let d = &st.doc;
    let roots: Vec<&Node> = match ids_param(p, "ids") {
        Some(ids) => ids.iter().map(|id| d.node(*id).ok_or(EngineError::NoNode(*id))).collect::<Result<_>>()?,
        None => d.layers.iter().map(|l| &**l).collect(),
    };
    let boards: Vec<Rect> = d.artboards.iter().map(|a| a.rect).collect();
    let (mut kinds, mut total, mut bounds) = (BTreeMap::<&str, usize>::new(), 0usize, None::<Rect>);
    let mut warn: BTreeMap<&str, Vec<u64>> = BTreeMap::new();
    let mut note = |w: &'static str, id: u64| warn.entry(w).or_default().push(id);
    for root in &roots {
        root.walk(&mut |n: &Node| {
            if n.is_layer() {
                return;
            }
            total += 1;
            *kinds.entry(n.kind_label()).or_default() += 1;
            if !n.visible {
                note("hidden", n.id.0);
            }
            if n.locked {
                note("locked", n.id.0);
            }
            if matches!(&n.kind, NodeKind::Path { path, .. } if path.anchor_count() == 0) {
                note("emptyPath", n.id.0);
            }
            // Leaves only: a group's bounds are its children's.
            if n.children().is_some() {
                return;
            }
            match n.geometric_bounds() {
                Some(b) if b.width() <= 1e-9 && b.height() <= 1e-9 => note("zeroSize", n.id.0),
                Some(b) => {
                    bounds = Some(bounds.map_or(b, |u| u.union(b)));
                    if !boards.is_empty() && !boards.iter().any(|a| a.intersect(b).area() > 0.0 || a.contains(b.center())) {
                        note("offArtboard", n.id.0);
                    }
                }
                None => {}
            }
        });
    }
    let warnings: Vec<Value> = warn
        .into_iter()
        .map(|(kind, ids)| json!({"kind": kind, "count": ids.len(), "ids": ids.iter().take(MAX_LISTED).collect::<Vec<_>>()}))
        .collect();
    Ok(json!({
        "revision": st.revision,
        "artboards": d.artboards.iter().map(|a| { let mut v = rect_json(a.rect); v["name"] = json!(a.name); v }).collect::<Vec<_>>(),
        "objects": total,
        "kinds": kinds,
        "bounds": bounds.map(rect_json),
        "selection": st.selection.objects.iter().map(|i| i.0).collect::<Vec<_>>(),
        "warnings": warnings,
    }))
}
