//! Tools the assistant makes for itself: named, parameterized sequences of the
//! existing tools, saved as JSON and offered as `custom_<name>` from then on.
//!
//! A tool is data, not code: `${expression}` templates and `$repeat` loops
//! expand into ordinary tool calls, so a made tool can do nothing the assistant
//! couldn't already do one call at a time.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use crossbeam_channel::Sender;
use serde_json::{Map, Value, json};

use super::gate::CallCtx;
use super::harness::Who;
use super::llm::{Block, ToolDef};
use super::tools::{self, ToolJob, ToolOutput};

pub const PREFIX: &str = "custom_";
/// Expanded array items per template, so a typo can't hang the app.
const MAX_ITEMS: usize = 20_000;
const BUILTIN: &str = include_str!("../../assets/agent_tools/retail.json");

pub type Vars = HashMap<String, Value>;

// ---------------------------------------------------------------------------
// Arithmetic

struct Expr<'a> {
    s: &'a [u8],
    i: usize,
    vars: &'a Vars,
}

impl Expr<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        let hit = self.s.get(self.i) == Some(&c);
        if hit {
            self.i += 1;
        }
        hit
    }

    fn sum(&mut self) -> Result<f64> {
        let mut v = self.product()?;
        loop {
            if self.eat(b'+') {
                v += self.product()?;
            } else if self.eat(b'-') {
                v -= self.product()?;
            } else {
                return Ok(v);
            }
        }
    }

    fn product(&mut self) -> Result<f64> {
        let mut v = self.unary()?;
        loop {
            if self.eat(b'*') {
                v *= self.unary()?;
            } else if self.eat(b'/') {
                v /= self.unary()?;
            } else if self.eat(b'%') {
                v = v.rem_euclid(self.unary()?);
            } else {
                return Ok(v);
            }
        }
    }

    fn unary(&mut self) -> Result<f64> {
        if self.eat(b'-') {
            return Ok(-self.unary()?);
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<f64> {
        if self.eat(b'(') {
            let v = self.sum()?;
            if !self.eat(b')') {
                bail!("missing )");
            }
            return Ok(v);
        }
        self.ws();
        let start = self.i;
        let c = *self.s.get(self.i).ok_or_else(|| anyhow!("expression ends early"))?;
        if c.is_ascii_digit() || c == b'.' {
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
                self.i += 1;
            }
            let t = std::str::from_utf8(&self.s[start..self.i])?;
            return t.parse().map_err(|_| anyhow!("bad number '{t}'"));
        }
        if !(c.is_ascii_alphabetic() || c == b'_') {
            bail!("unexpected '{}'", c as char);
        }
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.i])?;
        if self.eat(b'(') {
            let mut args = vec![self.sum()?];
            while self.eat(b',') {
                args.push(self.sum()?);
            }
            if !self.eat(b')') {
                bail!("missing ) after {name}(");
            }
            let a = args[0];
            return Ok(match (name, args.len()) {
                ("min", 2) => a.min(args[1]),
                ("max", 2) => a.max(args[1]),
                ("floor", 1) => a.floor(),
                ("ceil", 1) => a.ceil(),
                ("round", 1) => a.round(),
                ("abs", 1) => a.abs(),
                ("sqrt", 1) => a.sqrt(),
                ("sin", 1) => a.sin(),
                ("cos", 1) => a.cos(),
                _ => bail!("unknown function {name} with {} argument(s)", args.len()),
            });
        }
        let v = self.vars.get(name).ok_or_else(|| anyhow!("unknown variable '{name}'"))?;
        v.as_f64().ok_or_else(|| anyhow!("variable '{name}' is not a number"))
    }
}

pub fn eval(expr: &str, vars: &Vars) -> Result<f64> {
    let mut e = Expr { s: expr.as_bytes(), i: 0, vars };
    let v = e.sum()?;
    e.ws();
    if e.i != e.s.len() {
        bail!("unexpected text at the end of '{expr}'");
    }
    if !v.is_finite() {
        bail!("'{expr}' is not a finite number");
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Templates

fn number(v: f64) -> Value {
    if v.fract() == 0.0 && v.abs() < 9e15 { json!(v as i64) } else { json!(v) }
}

fn expand_str(s: &str, vars: &Vars) -> Result<Value> {
    // A whole-string template keeps its value's type.
    if let Some(inner) = s.strip_prefix("${").and_then(|r| r.strip_suffix('}'))
        && !inner.contains("${")
    {
        if let Some(v) = vars.get(inner.trim()) {
            return Ok(v.clone());
        }
        return Ok(number(eval(inner, vars).map_err(|e| anyhow!("in '{s}': {e}"))?));
    }
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find("${") {
        out.push_str(&rest[..i]);
        let end = rest[i..].find('}').ok_or_else(|| anyhow!("unclosed ${{ in '{s}'"))? + i;
        let inner = rest[i + 2..end].trim();
        match vars.get(inner) {
            Some(Value::String(t)) => out.push_str(t),
            Some(v) => out.push_str(&v.to_string()),
            None => out.push_str(&number(eval(inner, vars).map_err(|e| anyhow!("in '{s}': {e}"))?).to_string()),
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

fn expand_into(out: &mut Vec<Value>, v: &Value, vars: &Vars, budget: &mut usize) -> Result<()> {
    if let Some(rep) = v.get("$repeat") {
        let count = match rep {
            Value::String(s) => expand_str(s, vars)?.as_f64().or_else(|| eval(s, vars).ok()).ok_or_else(|| anyhow!("$repeat '{s}' is not a number"))?,
            other => other.as_f64().ok_or_else(|| anyhow!("$repeat must be a number"))?,
        };
        let var = v["as"].as_str().unwrap_or("i");
        let item = v.get("item").ok_or_else(|| anyhow!("$repeat needs item"))?;
        let mut inner = vars.clone();
        for i in 0..count.max(0.0).round() as usize {
            inner.insert(var.to_string(), json!(i));
            expand_into(out, item, &inner, budget)?;
        }
        return Ok(());
    }
    if *budget == 0 {
        bail!("the template expands to more than {MAX_ITEMS} items");
    }
    *budget -= 1;
    out.push(expand(v, vars, budget)?);
    Ok(())
}

fn expand(v: &Value, vars: &Vars, budget: &mut usize) -> Result<Value> {
    Ok(match v {
        Value::String(s) if s.contains("${") => expand_str(s, vars)?,
        Value::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for x in a {
                expand_into(&mut out, x, vars, budget)?;
            }
            Value::Array(out)
        }
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, x) in m {
                out.insert(k.clone(), expand(x, vars, budget)?);
            }
            Value::Object(out)
        }
        other => other.clone(),
    })
}

pub fn expand_root(v: &Value, vars: &Vars) -> Result<Value> {
    let mut budget = MAX_ITEMS;
    expand(v, vars, &mut budget)
}

// ---------------------------------------------------------------------------
// Saved tools

fn dir() -> PathBuf {
    crate::app_paths::data_base().join("SenseSim").join("agent_tools")
}

fn builtin() -> Vec<Value> {
    serde_json::from_str(BUILTIN).expect("assets/agent_tools/retail.json is valid")
}

/// Every available tool spec; a saved tool shadows a built-in of the same name.
fn specs() -> Vec<Value> {
    let mut by_name: std::collections::BTreeMap<String, Value> = Default::default();
    for s in builtin() {
        by_name.insert(s["name"].as_str().unwrap_or_default().to_string(), s);
    }
    for e in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
        if let Some(s) = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok())
            && let Some(n) = s["name"].as_str()
        {
            by_name.insert(n.to_string(), s);
        }
    }
    by_name.into_values().collect()
}

fn find(name: &str) -> Option<Value> {
    specs().into_iter().find(|s| s["name"] == name)
}

/// Tool definitions of the made tools, for the model.
pub fn defs() -> Vec<ToolDef> {
    specs()
        .iter()
        .filter_map(|s| {
            let name = s["name"].as_str()?;
            let params = s["parameters"].as_object().cloned().unwrap_or_default();
            let required: Vec<&String> = params.iter().filter(|(_, p)| p.get("default").is_none()).map(|(k, _)| k).collect();
            Some(ToolDef {
                name: format!("{PREFIX}{name}"),
                description: s["description"].as_str().unwrap_or_default().to_string(),
                schema: json!({ "type": "object", "properties": params, "required": required }),
            })
        })
        .collect()
}

fn check_vars(params: &Map<String, Value>, given: &Value) -> Result<Vars> {
    let mut vars = Vars::new();
    for (k, p) in params {
        match given.get(k).filter(|v| !v.is_null()).or(p.get("default")) {
            Some(v) => {
                vars.insert(k.clone(), v.clone());
            }
            None => bail!("missing parameter '{k}'"),
        }
    }
    Ok(vars)
}

/// A value of the declared type, to test-expand a tool when it is defined.
fn sample(p: &Value) -> Value {
    if let Some(d) = p.get("default") {
        return d.clone();
    }
    match p["type"].as_str() {
        Some("string") => json!("sample"),
        Some("boolean") => json!(true),
        Some("array") => json!([]),
        Some("object") => json!({}),
        _ => json!(1),
    }
}

/// Tools a macro may never run. MCP clients can define and call macros, and a step isn't marked
/// as coming from them, so without this a macro would reach what the MCP server refuses.
fn barred_from_macros(tool: &str, input: &Value) -> bool {
    tool.starts_with("twin_") || super::gate::agent_only(tool) || tool == "rtls_configure_outputs" || (tool == "rtls_openems" && input["action"] == "start")
}

/// Validates and saves a tool. Returns its callable name.
pub fn define(input: &Value, known_tools: &[ToolDef]) -> Result<String> {
    let name = super::assetgen::slug(input["name"].as_str().unwrap_or_default());
    if name.is_empty() {
        bail!("name is required");
    }
    if input["description"].as_str().unwrap_or_default().trim().is_empty() {
        bail!("description is required: it is how you will recognize the tool later");
    }
    let params = input["parameters"].as_object().cloned().unwrap_or_default();
    let steps = input["steps"].as_array().filter(|a| !a.is_empty()).ok_or_else(|| anyhow!("steps must list at least one {{tool, input}}"))?;
    let vars: Vars = params.iter().map(|(k, p)| (k.clone(), sample(p))).collect();
    for (n, st) in steps.iter().enumerate() {
        let tool = st["tool"].as_str().ok_or_else(|| anyhow!("step {n}: tool is required"))?;
        if tool.starts_with(PREFIX) || matches!(tool, "define_tool" | "delete_tool") {
            bail!("step {n}: a made tool can't call {tool}; copy its steps instead");
        }
        if !known_tools.iter().any(|d| d.name == tool) {
            bail!("step {n}: unknown tool '{tool}'");
        }
        let call = expand_root(&st["input"], &vars).map_err(|e| anyhow!("step {n}: {e}"))?;
        if barred_from_macros(tool, &call) {
            bail!("step {n}: a made tool can't call {tool} this way; call it directly");
        }
    }
    let spec = json!({ "name": name, "description": input["description"], "parameters": params, "steps": steps });
    std::fs::create_dir_all(dir())?;
    std::fs::write(dir().join(format!("{name}.json")), serde_json::to_string_pretty(&spec)?)?;
    Ok(format!("{PREFIX}{name}"))
}

pub fn delete(name: &str) -> Result<()> {
    let name = super::assetgen::slug(name.strip_prefix(PREFIX).unwrap_or(name));
    let path = dir().join(format!("{name}.json"));
    if !path.exists() {
        bail!("no saved tool '{name}'");
    }
    std::fs::remove_file(path)?;
    Ok(())
}

/// Runs a made tool's steps one by one, on a worker thread: each goes through
/// the bridge like any other call. Stops at the first failing step.
pub fn run(name: &str, input: &Value, tx: Sender<ToolJob>, ctx: CallCtx) -> ToolOutput {
    let bare = name.strip_prefix(PREFIX).unwrap_or(name);
    let Some(spec) = find(bare) else { return ToolOutput::err(format!("unknown tool '{name}'")) };
    let params = spec["parameters"].as_object().cloned().unwrap_or_default();
    let vars = match check_vars(&params, input) {
        Ok(v) => v,
        Err(e) => return ToolOutput::err(format!("{name}: {e:#}")),
    };
    let steps = spec["steps"].as_array().cloned().unwrap_or_default();
    let mut content = Vec::new();
    for (n, st) in steps.iter().enumerate() {
        let tool = st["tool"].as_str().unwrap_or_default();
        let call = match expand_root(&st["input"], &vars) {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("{name}: step {n} ({tool}): {e:#}")),
        };
        // Saved files can be older than this rule, or written by hand. An action can also
        // come from a parameter, so this looks at the expanded call.
        if barred_from_macros(tool, &call) {
            return ToolOutput::err(format!("{name}: step {n}: a made tool can't call {tool}"));
        }
        if ctx.cancelled() {
            content.push(Block::Text(format!("{name} was stopped before step {n}; the earlier steps stay applied.")));
            return ToolOutput { content, is_error: true };
        }
        // A step inherits who asked, so a macro can't launder an MCP client's call.
        let out = tools::call_ctx(&tx, tool, call.clone(), ctx.clone());
        // The self-checks judge what actually changed, not the macro's name.
        if let Some(log) = &ctx.log {
            log.push(Who::Macro, tool, &call, &out);
        }
        content.push(Block::Text(format!("step {n} ({tool}):")));
        let failed = out.is_error;
        content.extend(out.content);
        if failed {
            content.push(Block::Text(format!("{name} stopped at step {n}; the earlier steps stay applied.")));
            return ToolOutput { content, is_error: true };
        }
    }
    ToolOutput { content, is_error: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(v: Value) -> Vars {
        v.as_object().unwrap().iter().map(|(k, x)| (k.clone(), x.clone())).collect()
    }

    #[test]
    fn macros_cant_reach_what_the_mcp_server_refuses() {
        let known: Vec<ToolDef> = ["rtls_configure_outputs", "rtls_openems"].iter().map(|n| ToolDef { name: (*n).into(), description: String::new(), schema: json!({}) }).collect();
        for step in [json!({ "tool": "rtls_configure_outputs", "input": { "webhook": "http://x" } }), json!({ "tool": "rtls_openems", "input": { "action": "start" } })] {
            let spec = json!({ "name": "aim_outputs", "description": "d", "steps": [step] });
            let e = define(&spec, &known).unwrap_err().to_string();
            assert!(e.contains("can't call"), "{e}");
        }
        assert!(!dir().join("aim_outputs.json").exists(), "a refused tool isn't saved");
        // The action can arrive as a parameter, so it's judged after expansion.
        let call = expand_root(&json!({ "action": "${a}" }), &vars(json!({ "a": "start" }))).unwrap();
        assert!(barred_from_macros("rtls_openems", &call));
        assert!(!barred_from_macros("rtls_openems", &json!({ "action": "status" })));
        assert!(barred_from_macros("twin_summary", &json!({})));
        assert!(!barred_from_macros("place_objects", &json!({})));
    }

    #[test]
    fn arithmetic() {
        let v = vars(json!({ "w": 2.0, "n": 5 }));
        assert_eq!(eval("1 + 2 * 3", &v).unwrap(), 7.0);
        assert_eq!(eval("(1 + 2) * 3", &v).unwrap(), 9.0);
        assert_eq!(eval("-w / 2 + min(n, 3)", &v).unwrap(), 2.0);
        assert_eq!(eval("7 % 3", &v).unwrap(), 1.0);
        assert!(eval("nope + 1", &v).is_err());
        assert!(eval("1 / 0", &v).is_err());
        assert!(eval("1 +", &v).is_err());
        assert!(eval("1 2", &v).is_err());
    }

    #[test]
    fn templates_keep_types_and_interpolate() {
        let v = vars(json!({ "w": 1.5, "name": "shelf", "mat": { "preset": "steel" } }));
        let out = expand_root(&json!({ "a": "${w*2}", "b": "${name}_a", "c": "${mat}", "d": "${w}", "e": 4 }), &v).unwrap();
        assert_eq!(out, json!({ "a": 3, "b": "shelf_a", "c": { "preset": "steel" }, "d": 1.5, "e": 4 }));
    }

    #[test]
    fn repeats_nest_and_flatten() {
        let v = vars(json!({ "n": 2 }));
        let t = json!({ "parts": [
            { "id": "first" },
            { "$repeat": "n", "as": "i", "item": { "$repeat": 2, "as": "j", "item": { "at": ["${i}", "${j * 0.5}"] } } }
        ] });
        let out = expand_root(&t, &v).unwrap();
        assert_eq!(out["parts"].as_array().unwrap().len(), 5);
        assert_eq!(out["parts"][4]["at"], json!([1, 0.5]));
    }

    #[test]
    fn runaway_templates_stop() {
        let t = json!([{ "$repeat": 1000000, "item": { "a": 1 } }]);
        assert!(expand_root(&t, &Vars::new()).is_err());
    }

    #[test]
    fn built_in_tools_expand_to_valid_assets() {
        let known = tools::tool_defs(false);
        for spec in builtin() {
            let name = spec["name"].as_str().unwrap();
            let params = spec["parameters"].as_object().cloned().unwrap_or_default();
            let v: Vars = params.iter().map(|(k, p)| (k.clone(), sample(p))).collect();
            for st in spec["steps"].as_array().unwrap() {
                let tool = st["tool"].as_str().unwrap();
                assert!(known.iter().any(|d| d.name == tool), "{name}: unknown {tool}");
                let call = expand_root(&st["input"], &v).unwrap_or_else(|e| panic!("{name}: {e:#}"));
                if tool == "create_asset" {
                    for (i, p) in call["parts"].as_array().unwrap().iter().enumerate() {
                        assert!(p["size"].as_array().is_some_and(|a| a.iter().all(|x| x.as_f64().unwrap() > 0.0)), "{name} part {i}: {p}");
                    }
                }
            }
        }
    }

    #[test]
    fn definitions_are_checked() {
        let known = tools::tool_defs(false);
        let ok = |steps: Value| json!({ "name": "t", "description": "d", "parameters": { "w": { "type": "number" } }, "steps": steps });
        assert!(define(&ok(json!([{ "tool": "nonexistent", "input": {} }])), &known).is_err());
        assert!(define(&ok(json!([{ "tool": "define_tool", "input": {} }])), &known).is_err());
        assert!(define(&ok(json!([{ "tool": "custom_x", "input": {} }])), &known).is_err());
        assert!(define(&ok(json!([{ "tool": "get_scene", "input": { "x": "${typo}" } }])), &known).is_err(), "a misspelled parameter fails at definition");
    }
}
