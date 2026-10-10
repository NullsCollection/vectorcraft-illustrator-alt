//! `look`: styling for the objects a command makes, given with the command itself
//! (`shape.rectangle {…, look: {fill: "#e8234e", stroke: "none"}}`), applied in the same undo step.
//! Any command whose result names the objects it made (`id` or `ids`) takes it; the paint it sets
//! leaves the defaults new art gets alone.

use serde_json::{Map, Value, json};

/// The keys a `look` takes.
pub(crate) const KEYS: &[&str] = &["fill", "stroke", "weight", "dash", "cap", "join", "miterLimit", "align", "opacity", "blend"];

/// The ids a command's result names: `id`, else `ids`.
pub(crate) fn made(result: &Value) -> Vec<Value> {
    match (result.get("id"), result.get("ids").and_then(Value::as_array)) {
        (Some(id), _) if id.is_u64() => vec![id.clone()],
        (_, Some(ids)) => ids.iter().filter(|v| v.is_u64()).cloned().collect(),
        _ => vec![],
    }
}

/// The commands that give the objects `ids` the look `look`, in order: fill, stroke, stroke
/// options, transparency.
pub(crate) fn steps(look: &Value, ids: &[Value]) -> Result<Vec<(&'static str, Value)>, String> {
    let o = look.as_object().ok_or("`look` must be an object")?;
    if let Some(k) = o.keys().find(|k| !KEYS.contains(&k.as_str())) {
        return Err(format!("unknown `look` key `{k}` (expected {})", KEYS.join(", ")));
    }
    let ids = Value::Array(ids.to_vec());
    let mut out = vec![];
    for (key, cmd) in [("fill", "paint.setFill"), ("stroke", "paint.setStroke")] {
        let Some(v) = o.get(key) else { continue };
        let mut p = paint_params(key, v)?;
        p.insert("ids".into(), ids.clone());
        p.insert("focus".into(), json!(false));
        p.insert("default".into(), json!(false));
        out.push((cmd, Value::Object(p)));
    }
    let mut stroke = Map::new();
    for key in ["weight", "dash", "cap", "join", "miterLimit", "align"] {
        if let Some(v) = o.get(key) {
            stroke.insert(key.into(), v.clone());
        }
    }
    if !stroke.is_empty() {
        stroke.insert("ids".into(), ids.clone());
        stroke.insert("default".into(), json!(false));
        out.push(("stroke.set", Value::Object(stroke)));
    }
    let mut tr = Map::new();
    for key in ["opacity", "blend"] {
        if let Some(v) = o.get(key) {
            tr.insert(key.into(), v.clone());
        }
    }
    if !tr.is_empty() {
        tr.insert("ids".into(), ids);
        out.push(("transparency.set", Value::Object(tr)));
    }
    Ok(out)
}

/// A `fill` or `stroke` value as paint params: "none", a colour (string, `[r,g,b]` or a colour
/// object), or paint params of their own (`{gradient}`, `{swatch, tint?}`…).
fn paint_params(key: &str, v: &Value) -> Result<Map<String, Value>, String> {
    const PAINT_KEYS: &[&str] = &["gradient", "swatch", "none", "color", "tint", "keepModel", "item"];
    Ok(match v {
        Value::String(s) if s == "none" => Map::from_iter([("none".into(), json!(true))]),
        Value::String(_) | Value::Array(_) => Map::from_iter([("color".into(), v.clone())]),
        Value::Object(m) if m.keys().any(|k| PAINT_KEYS.contains(&k.as_str())) => m.clone(),
        Value::Object(_) => Map::from_iter([("color".into(), v.clone())]),
        _ => return Err(format!("`look.{key}` must be a colour, \"none\" or paint params")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_look_becomes_paint_stroke_and_transparency_steps() {
        let ids = [json!(7)];
        let look = json!({"fill": "#ff0000", "stroke": "none", "weight": 2, "dash": [4, 2], "opacity": 50});
        let s = steps(&look, &ids).unwrap();
        let cmds: Vec<&str> = s.iter().map(|(c, _)| *c).collect();
        assert_eq!(cmds, ["paint.setFill", "paint.setStroke", "stroke.set", "transparency.set"]);
        assert_eq!(s[0].1, json!({"color": "#ff0000", "ids": [7], "focus": false, "default": false}));
        assert_eq!(s[1].1["none"], true);
        assert_eq!(s[2].1, json!({"weight": 2, "dash": [4, 2], "ids": [7], "default": false}));
        assert_eq!(s[3].1, json!({"opacity": 50, "ids": [7]}));
        // Paint params and colour objects pass through.
        let g = steps(&json!({"fill": {"gradient": {"kind": "radial"}}, "stroke": {"c": 0, "m": 1, "y": 1, "k": 0}}), &ids).unwrap();
        assert!(g[0].1.get("gradient").is_some() && g[1].1["color"]["m"] == 1);
        assert!(steps(&json!({"colour": "red"}), &ids).is_err());
        assert!(steps(&json!("red"), &ids).is_err());
        assert!(steps(&json!({"fill": 3}), &ids).is_err());
        assert_eq!(made(&json!({"id": 4})), vec![json!(4)]);
        assert_eq!(made(&json!({"ids": [4, 5], "index": 0})), vec![json!(4), json!(5)]);
        assert!(made(&json!(null)).is_empty());
    }
}
