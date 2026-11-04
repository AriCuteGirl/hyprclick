//! Sway backend, through swaymsg. Sway can't send clicks to a background
//! window or report where the cursor is, so Cursor mode and Gamepad anti-AFK
//! are what work here. Setup (binds, window rule, pad autostart) is in setup.rs.

use crate::desktop::Client;
use serde_json::Value;
use std::process::Command;

pub fn msg(args: &[&str]) -> Result<String, String> {
    let out = Command::new("swaymsg").args(args).output().map_err(|e| format!("swaymsg: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(text)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(if err.trim().is_empty() { text.trim().to_string() } else { err.trim().to_string() })
    }
}

fn tree() -> Option<Value> {
    serde_json::from_str(&msg(&["-r", "-t", "get_tree"]).ok()?).ok()
}

/// Every window in a get_tree reply, depth first, tagged with its workspace.
fn windows(tree: &Value) -> Vec<Client> {
    let mut out = Vec::new();
    let mut stack = vec![(tree, String::new())];
    while let Some((node, workspace)) = stack.pop() {
        let workspace = if node["type"] == "workspace" {
            node["name"].as_str().unwrap_or_default().to_string()
        } else {
            workspace
        };
        for key in ["nodes", "floating_nodes"] {
            for child in node[key].as_array().into_iter().flatten() {
                stack.push((child, workspace.clone()));
            }
        }
        if node["pid"].is_null() {
            continue;
        }
        let class = node["app_id"].as_str().or_else(|| node["window_properties"]["class"].as_str());
        let Some(class) = class else { continue };
        let rect = &node["rect"];
        let num = |v: &Value| v.as_f64().unwrap_or(0.0);
        out.push(Client {
            address: node["id"].to_string(),
            class: class.to_string(),
            title: node["name"].as_str().unwrap_or_default().to_string(),
            at: [num(&rect["x"]), num(&rect["y"])],
            size: [num(&rect["width"]), num(&rect["height"])],
            xwayland: node["shell"].as_str().map(|s| s == "xwayland"),
            workspace: workspace.clone(),
            visible: node["visible"].as_bool().unwrap_or(false),
            recency: if node["focused"].as_bool() == Some(true) { 0 } else { 1 },
        });
    }
    out
}

pub fn clients() -> Vec<Client> {
    tree().map(|t| windows(&t)).unwrap_or_default()
}

pub fn active_address() -> Option<String> {
    clients().into_iter().find(|c| c.recency == 0).map(|c| c.address)
}

/// Sway doesn't tell clients where the cursor is.
pub fn cursor_pos() -> Option<(f64, f64)> {
    None
}

pub fn notify(text: &str) {
    let _ = Command::new("notify-send").args(["-a", "hyprclick", "hyprclick", text]).spawn();
}

pub fn focus_class(class: &str) {
    let _ = msg(&[&format!("[app_id=\"^{class}$\"] focus")]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_windows_from_the_tree() {
        let tree: Value = serde_json::from_str(
            r#"{"type":"root","nodes":[{"type":"output","nodes":[{"type":"workspace","name":"2","nodes":[
                {"id":7,"pid":10,"app_id":"foot","name":"shell","shell":"xdg_shell","visible":true,"focused":true,
                 "rect":{"x":0,"y":20,"width":800,"height":600},"nodes":[],"floating_nodes":[]}],
              "floating_nodes":[
                {"id":9,"pid":11,"app_id":null,"window_properties":{"class":"Steam"},"name":"Steam","shell":"xwayland",
                 "visible":false,"focused":false,"rect":{"x":5,"y":6,"width":7,"height":8},"nodes":[],"floating_nodes":[]}]}]}]}"#,
        )
        .expect("json");
        let mut found = windows(&tree);
        found.sort_by_key(|c| c.address.clone());
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].class.as_str(), found[0].workspace.as_str(), found[0].recency), ("foot", "2", 0));
        assert_eq!(found[0].size, [800.0, 600.0]);
        assert_eq!((found[1].class.as_str(), found[1].xwayland, found[1].visible), ("Steam", Some(true), false));
    }
}
