//! `command.batch`: several commands as one undo step (in each document they edit). A step's
//! params may take values from earlier steps' results (`"$1.id"`, `"$last.ids.0"`: see
//! [`crate::steps`]), so a whole drawing goes in one request.

use serde_json::{Value, json};

use super::super::*;
use crate::{DocState, EngineError};

/// Most steps one batch runs: a request is untrusted input.
pub(crate) const MAX_STEPS: usize = 10_000;

pub(super) fn batch(s: &mut Session, p: &Value) -> Result<Value> {
    let mut cmds = p.get("commands").and_then(Value::as_array).ok_or_else(|| bad("command.batch", "missing commands"))?.clone();
    if cmds.len() > MAX_STEPS {
        return Err(bad("command.batch", format!("at most {MAX_STEPS} commands in one batch (got {})", cmds.len())));
    }
    if !bool_or(p, "atomic", true) {
        let r = run_each(s, &mut cmds)?;
        s.journal.push(("command.batch".into(), p.clone()));
        return Ok(r);
    }
    let label = str_param(p, "label").unwrap_or("Batch").to_string();
    // What an error rolls back besides the documents: session-level state a step may change
    // (paint defaults, drawing mode, clipboard, the Untitled-N count).
    let saved = (s.paint.clone(), s.fill_active, s.draw_mode, s.draw_inside, s.clipboard.clone(), s.untitled_counter);
    let (start, active): (Vec<u64>, _) = (s.docs.iter().map(|d| d.uid).collect(), s.active);
    s.batch_stash = Some(vec![]);
    let r = run_steps(s, &label, &mut cmds);
    let stash = s.batch_stash.take().unwrap_or_default();
    let results = match r {
        Ok(results) => results,
        Err(e) => {
            roll_back(s, &start, stash);
            s.active = active;
            (s.paint, s.fill_active, s.draw_mode, s.draw_inside, s.clipboard, s.untitled_counter) = saved;
            s.pending_paint = None;
            return Err(e);
        }
    };
    // Each document the steps edited keeps them as one undo step.
    s.remember_pending_paint();
    for st in &mut s.docs {
        if let Some(it) = st.interaction.take() {
            st.keep_interaction(it);
        }
    }
    // The batch journals itself, whichever document it ends in.
    let mut entry = p.clone();
    if let Some(o) = entry.as_object_mut() {
        o.insert("commands".into(), Value::Array(cmds));
    }
    s.journal.push(("command.batch".into(), entry));
    Ok(json!({ "results": results }))
}

/// Run the steps → their results. Each runs inside an interaction of the document active when it
/// starts (one for all the steps in that document), so a step may open, switch or close documents.
fn run_steps(s: &mut Session, label: &str, cmds: &mut [Value]) -> Result<Vec<Value>> {
    let mut results = Vec::with_capacity(cmds.len());
    for c in cmds {
        let id = c.get("command").and_then(Value::as_str).unwrap_or("").to_string();
        if id == "command.batch" {
            return Err(bad("command.batch", "batches cannot nest"));
        }
        if c.get("params").is_some_and(|p| p.get("doc").is_some()) {
            return Err(bad("command.batch", format!("step {}: give `doc` on the batch, not on a step", results.len())));
        }
        if s.active().is_some() {
            s.begin_interaction(label)?;
        }
        let given = c.get("params").cloned().unwrap_or(json!({}));
        let params =
            crate::steps::resolve(&given, &results).map_err(|e| EngineError::Other(format!("batch step {} (`{id}`) failed: {e}", results.len())))?;
        let (v, noted) =
            s.execute_step(&id, &params).map_err(|e| EngineError::Other(format!("batch step {} (`{id}`) failed: {e}", results.len())))?;
        // The step's journal entry records what it took from the preferences or the clock (a
        // save's date, a new document's), as a command of its own would; references stay as
        // written, so a replay takes the ids its own steps make.
        if noted != params
            && let Some(c) = c.as_object_mut()
        {
            c.insert("params".into(), if given == params { noted } else { with_noted(&given, &params, &noted) });
        }
        results.push(v);
    }
    Ok(results)
}

/// `given` (with its step references) plus what the step noted beyond the `resolved` params.
fn with_noted(given: &Value, resolved: &Value, noted: &Value) -> Value {
    let mut out = given.clone();
    if let (Some(o), Some(r), Some(n)) = (out.as_object_mut(), resolved.as_object(), noted.as_object()) {
        for (k, v) in n {
            if !r.contains_key(k) {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

/// `atomic: false`: each step is a command of its own (its own undo step, journaled alone) and
/// the steps after a failure still run; references to a failed step's result fail in turn. →
/// `{results: [{ok, result}|{ok: false, error}], completed, failed}`.
fn run_each(s: &mut Session, cmds: &mut [Value]) -> Result<Value> {
    let (mut values, mut results, mut failed) = (Vec::with_capacity(cmds.len()), Vec::with_capacity(cmds.len()), 0usize);
    for c in cmds.iter() {
        let id = c.get("command").and_then(Value::as_str).unwrap_or("").to_string();
        let r = if id == "command.batch" {
            Err(bad("command.batch", "batches cannot nest").to_string())
        } else {
            let given = c.get("params").cloned().unwrap_or(json!({}));
            crate::steps::resolve(&given, &values).and_then(|params| s.execute(&id, &params).map_err(|e| e.to_string()))
        };
        match r {
            Ok(v) => {
                results.push(json!({"ok": true, "result": v}));
                values.push(v);
            }
            Err(e) => {
                failed += 1;
                results.push(json!({"ok": false, "error": format!("step {} (`{id}`): {e}", values.len())}));
                values.push(Value::Null);
            }
        }
    }
    Ok(json!({"results": results, "completed": cmds.len() - failed, "failed": failed}))
}

/// The documents as they were before the batch (`start`: their uids in order): the ones it opened
/// close, the ones it closed or replaced (`stash`) come back, and each one's interaction is undone.
fn roll_back(s: &mut Session, start: &[u64], mut stash: Vec<DocState>) {
    let mut open = std::mem::take(&mut s.docs);
    for &uid in start {
        // The newest revision the document reached, so the restored one redraws.
        let newest = open.iter().chain(&stash).filter(|d| d.uid == uid).map(|d| d.revision).max().unwrap_or_default();
        // Its state before the batch: the first one a step closed or replaced, else the open one.
        let st = match stash.iter().position(|d| d.uid == uid) {
            Some(i) => Some(stash.remove(i)),
            None => open.iter().position(|d| d.uid == uid).map(|i| open.remove(i)),
        };
        if let Some(mut st) = st {
            st.undo_interaction();
            st.revision = newest.saturating_add(1);
            s.docs.push(st);
        }
    }
}
