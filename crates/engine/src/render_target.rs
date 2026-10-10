//! What a render request covers (`ui.render` on the desktop control port, the headless MCP
//! backend's render, MCP `render_preview`): which document, which area, at what scale. Shared so
//! agents get the same answer wherever they render; the rendering itself is the caller's.

use std::sync::Arc;

use serde_json::Value;
use vectorcraft_doc::{Document, NodeId};
use vectorcraft_geom::Rect;

use crate::Session;

/// Most pixels a side may have when `maxSize` sets the scale.
pub const MAX_SIZE: f64 = 4096.0;

/// `{doc?: id (document.list; default the active one), artboard?: index (0), ids?: [id…] (their
/// bounds and a small margin), rect?: [x, y, width, height], maxSize?: px (8..4096: fit the area
/// into it), scale?: (1)}` → the document, the area and the scale (0.01..16).
pub fn render_target(s: &Session, p: &Value) -> Result<(Arc<Document>, Rect, f64), String> {
    let st = match p.get("doc") {
        Some(v) => {
            let uid = v.as_u64().ok_or("`doc` must be a document id (see document.list)")?;
            s.documents().iter().find(|d| d.uid == uid).ok_or_else(|| format!("no open document with id {uid} (see document.list)"))?
        }
        None => s.active().ok_or("no document")?,
    };
    let doc = st.doc.clone();
    let area = match (p.get("ids"), p.get("rect")) {
        (Some(ids), _) => {
            let ids = ids.as_array().ok_or("`ids` must be an array of object ids")?;
            let mut b: Option<Rect> = None;
            for v in ids {
                let n = v.as_u64().and_then(|i| doc.node(NodeId(i))).ok_or_else(|| format!("no object {v}"))?;
                if let Some(nb) = n.geometric_bounds() {
                    b = Some(b.map_or(nb, |u| u.union(nb)));
                }
            }
            let b = b.ok_or("those objects have no bounds")?;
            b.inflate((b.width() * 0.05).max(2.0), (b.height() * 0.05).max(2.0))
        }
        (None, Some(r)) => match r.as_array().and_then(|a| a.iter().map(Value::as_f64).collect::<Option<Vec<f64>>>()).as_deref() {
            Some(&[x, y, w, h]) if w > 0.0 && h > 0.0 && [x, y, w, h].iter().all(|v| v.is_finite()) => Rect::new(x, y, x + w, y + h),
            _ => return Err("`rect` must be [x, y, width, height] with a positive size".into()),
        },
        (None, None) => {
            let i = p.get("artboard").and_then(Value::as_u64).unwrap_or(0);
            usize::try_from(i).ok().and_then(|i| doc.artboards.get(i)).map(|a| a.rect).ok_or("no such artboard")?
        }
    };
    let scale = match p.get("maxSize") {
        Some(v) => {
            let m = v.as_f64().filter(|m| m.is_finite() && (8.0..=MAX_SIZE).contains(m)).ok_or("`maxSize` must be 8..4096 pixels")?;
            m / area.width().max(area.height()).max(1e-9)
        }
        None => p.get("scale").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(1.0),
    };
    Ok((doc, area, scale.clamp(0.01, 16.0)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_render_covers_a_document_an_area_or_objects() {
        let mut s = Session::new();
        let first = s.execute("file.new", &json!({"width": 400, "height": 200})).unwrap()["doc"].as_u64().unwrap();
        let id = s.execute("shape.rectangle", &json!({"x": 100, "y": 50, "width": 40, "height": 20})).unwrap()["id"].clone();
        s.execute("file.new", &json!({"width": 100, "height": 100})).unwrap();
        // The first artboard of the active document, at scale 1.
        let (_, r, scale) = render_target(&s, &json!({})).unwrap();
        assert_eq!((r.width(), scale), (100.0, 1.0));
        // Another document's object, with a margin, fitted into 256 px.
        let (doc, r, scale) = render_target(&s, &json!({"doc": first, "ids": [id], "maxSize": 256})).unwrap();
        assert!(doc.node(NodeId(id.as_u64().unwrap())).is_some());
        assert!(r.x0 < 100.0 && r.x1 > 140.0 && r.width() < 50.0, "{r:?}");
        assert!((r.width().max(r.height()) * scale - 256.0).abs() < 1e-6);
        let (_, r, _) = render_target(&s, &json!({"rect": [10, 10, 30, 20]})).unwrap();
        assert_eq!((r.x0, r.y1), (10.0, 30.0));
        for bad in [
            json!({"doc": 999}),
            json!({"ids": [99999]}),
            json!({"rect": [0, 0, -1, 5]}),
            json!({"maxSize": 5}),
            json!({"maxSize": "big"}),
            json!({"artboard": 9}),
        ] {
            assert!(render_target(&s, &bad).is_err(), "{bad}");
        }
    }
}
