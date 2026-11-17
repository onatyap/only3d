//! Command palette model: fuzzy matching over every UI action and every studio tool, and JSON
//! argument templates for tools (the palette's "run tool" form).

use serde_json::{json, Map, Value};

use crate::keymap::Action;

#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Action(Action),
    /// A studio tool by name.
    Tool(String),
}

#[derive(Clone, Debug)]
pub struct Item {
    pub entry: Entry,
    /// What is matched and shown: "Edit: Select all", "Tool: slice".
    pub label: String,
    pub detail: String,
    pub chord: Option<String>,
}

/// A studio tool, as listed in the registry.
#[derive(Clone, Debug)]
pub struct ToolInfo {
    pub name: String,
    pub title: String,
    pub description: String,
    pub schema: Value,
    pub mutates: bool,
    pub side_effects: bool,
}

impl ToolInfo {
    /// True when the tool takes no arguments at all (runs straight from the palette).
    pub fn no_args(&self) -> bool {
        self.schema.get("properties").and_then(|p| p.as_object()).is_none_or(|p| p.is_empty())
    }
}

/// Case-insensitive subsequence match with bonuses for word starts, consecutive runs and
/// prefixes. `None` when `query` is not a subsequence of `text`.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().collect();
    let tl: Vec<char> = t.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let mut score = 0i32;
    let mut qi = 0;
    let mut prev: Option<usize> = None;
    for (i, c) in tl.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            let word_start = i == 0 || matches!(t[i - 1], ' ' | '_' | '-' | '.' | ':' | '/' | '(') || (t[i].is_uppercase() && t[i - 1].is_lowercase());
            score += 10;
            if word_start {
                score += 12;
            }
            if let Some(p) = prev {
                if p + 1 == i {
                    score += 15;
                } else {
                    score -= ((i - p - 1) as i32).min(8);
                }
            } else {
                score -= (i as i32).min(12);
            }
            prev = Some(i);
            qi += 1;
        }
    }
    if qi < q.len() {
        return None;
    }
    // shorter labels win ties
    Some(score * 4 - t.len() as i32 / 8)
}

/// Indices of `items` matching `query`, best first (stable for equal scores).
pub fn filter(items: &[Item], query: &str) -> Vec<usize> {
    let mut scored: Vec<(i32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, it)| {
            let a = fuzzy_score(query, &it.label);
            // also match the tool name / action id in the detail, at a discount
            let b = fuzzy_score(query, &it.detail).map(|s| s / 3 - 20);
            a.max(b).map(|s| (s, i))
        })
        .collect();
    scored.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// A JSON object with every required property filled with a placeholder of the right type.
pub fn args_template(schema: &Value) -> String {
    let props = schema.get("properties").and_then(|p| p.as_object()).cloned().unwrap_or_default();
    let required: Vec<String> = schema.get("required").and_then(|r| r.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
    let mut out = Map::new();
    for k in &required {
        let p = props.get(k).cloned().unwrap_or(Value::Null);
        out.insert(k.clone(), placeholder(&p, schema));
    }
    serde_json::to_string_pretty(&Value::Object(out)).unwrap_or_else(|_| "{}".into())
}

/// Optional properties, for the hint under the form: "name (string) — description".
pub fn optional_args(schema: &Value) -> Vec<String> {
    let props = schema.get("properties").and_then(|p| p.as_object()).cloned().unwrap_or_default();
    let required: Vec<&str> = schema.get("required").and_then(|r| r.as_array()).map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
    props
        .iter()
        .filter(|(k, _)| !required.contains(&k.as_str()))
        .map(|(k, v)| {
            let ty = type_name(v);
            let desc = v.get("description").and_then(|d| d.as_str()).unwrap_or("");
            if desc.is_empty() {
                format!("{k} ({ty})")
            } else {
                format!("{k} ({ty}) — {desc}")
            }
        })
        .collect()
}

fn type_name(v: &Value) -> String {
    match v.get("type") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str()).filter(|s| *s != "null").collect::<Vec<_>>().join("|"),
        _ => {
            if v.get("$ref").is_some() || v.get("anyOf").is_some() || v.get("oneOf").is_some() {
                "object".into()
            } else {
                "any".into()
            }
        }
    }
}

fn placeholder(p: &Value, root: &Value) -> Value {
    // follow a local $ref to an enum, if any
    if let Some(r) = p.get("$ref").and_then(|r| r.as_str()) {
        let name = r.rsplit('/').next().unwrap_or("");
        if let Some(def) = root.get("$defs").or_else(|| root.get("definitions")).and_then(|d| d.get(name)) {
            if let Some(e) = def.get("enum").and_then(|e| e.as_array()).and_then(|a| a.first()) {
                return e.clone();
            }
            if let Some(c) = def.get("oneOf").and_then(|o| o.as_array()).and_then(|a| a.first()).and_then(|f| f.get("const").or_else(|| f.get("enum").and_then(|e| e.get(0)))) {
                return c.clone();
            }
            return placeholder(def, root);
        }
    }
    if let Some(e) = p.get("enum").and_then(|e| e.as_array()).and_then(|a| a.first()) {
        return e.clone();
    }
    match type_name(p).split('|').next().unwrap_or("") {
        "string" => json!(""),
        "integer" | "number" => json!(0),
        "boolean" => json!(false),
        "array" => json!([]),
        "object" => json!({}),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, detail: &str) -> Item {
        Item { entry: Entry::Tool(label.into()), label: label.into(), detail: detail.into(), chord: None }
    }

    #[test]
    fn fuzzy_basics() {
        assert!(fuzzy_score("sel", "Edit: Select all").is_some());
        assert!(fuzzy_score("xyz", "Edit: Select all").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        // word starts beat scattered letters
        let a = fuzzy_score("sa", "Edit: Select all").unwrap();
        let b = fuzzy_score("sa", "Tool: spool_add_fast").unwrap();
        assert!(a > 0 && b > 0);
        let exact = fuzzy_score("slice", "Slice plate").unwrap();
        let scattered = fuzzy_score("slice", "Spool list, ice").unwrap();
        assert!(exact > scattered, "{exact} vs {scattered}");
        // case-insensitive
        assert!(fuzzy_score("SLICE", "slice plate").is_some());
    }

    #[test]
    fn filter_orders_best_first() {
        let items = vec![item("View: Toggle Prepare / Preview", "view.toggle_preview"), item("Slice & print: Slice plate", "slice.slice"), item("Tool: spool_list — Spools", "spool_list")];
        let r = filter(&items, "slice");
        assert_eq!(r.first(), Some(&1));
        let r = filter(&items, "spool");
        assert_eq!(r, vec![2]);
        let r = filter(&items, "toggle_prev");
        assert_eq!(r, vec![0]);
        assert_eq!(filter(&items, "").len(), 3);
    }

    #[test]
    fn templates_fill_required_args() {
        let schema = json!({
            "type": "object",
            "required": ["path", "kind", "count", "mode"],
            "properties": {
                "path": {"type": "string"},
                "kind": {"type": "string", "enum": ["process", "filament"]},
                "count": {"type": "integer"},
                "mode": {"$ref": "#/$defs/Mode"},
                "flatten": {"type": ["boolean", "null"], "description": "Resolve inherits."}
            },
            "$defs": {"Mode": {"oneOf": [{"const": "fast"}, {"const": "slow"}]}}
        });
        let t: Value = serde_json::from_str(&args_template(&schema)).unwrap();
        assert_eq!(t, json!({"path": "", "kind": "process", "count": 0, "mode": "fast"}));
        let opt = optional_args(&schema);
        assert_eq!(opt, vec!["flatten (boolean) — Resolve inherits.".to_string()]);
        let none = ToolInfo { name: "undo".into(), title: "Undo".into(), description: String::new(), schema: json!({"type": "object", "properties": {}}), mutates: false, side_effects: false };
        assert!(none.no_args());
    }
}
