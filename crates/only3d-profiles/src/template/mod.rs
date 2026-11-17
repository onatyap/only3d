//! Bambu Studio / PrusaSlicer G-code template ("placeholder parser") language.
//!
//! A faithful re-implementation of `src/libslic3r/PlaceholderParser.cpp` as shipped in Bambu
//! Studio (which is based on the PrusaSlicer 2.4-era grammar), with a hand-written
//! recursive-descent parser.
//!
//! # Syntax
//!
//! * Plain text is copied verbatim (including newlines and indentation).
//! * `[name]` — legacy expansion. Scalars print serialized (`bool` -> `1`/`0`, floats via
//!   `%g`); vectors print the element of the *current filament* (see
//!   [`Context::set_current_filament_id`]) or element 0 when out of range. `[name_3]` falls back to
//!   element 3 of vector `name` when `name_3` itself is not defined. `[name[idx]]` indexes vector
//!   `name` with *integer variable* `idx`.
//! * `{expr}` — prints an arithmetic expression (the grammar only allows an additive expression
//!   here: comparisons and `?:` must be parenthesised, e.g. `{(a > b)}`).
//! * `{if cond}...{elsif cond}...{else}...{endif}` — nested freely; conditions may span lines.
//! * Expressions: `+ - * / %`, unary `- + ! not`, comparisons `== != <> < > <= >=`, `&& and`,
//!   `|| or`, `cond ? a : b`, `=~ /regex/`, `!~ /regex/`, parentheses, int/float/string/bool
//!   literals, `var`, `var[expr]`, and functions `min max int round floor ceil digits zdigits
//!   random filament_change`.
//! * Whitespace (including newlines) is ignored inside `{}`/`[]`. Leading whitespace of the whole
//!   template is dropped (a Boost.Spirit pre-skip quirk reproduced on purpose).
//!
//! # Semantics reproduced from Bambu Studio
//!
//! * Types: bool, int, float, string. `int op int` stays int (`7/2 == 3`), anything with a float
//!   is float. `+` with a string concatenates. Numbers compare with a `1e-8` tolerance for `==`
//!   when a float is involved; a string compared with anything compares string forms.
//!   `&&`/`||`/`!`/`if`/`?:` require booleans (`{if 1}` is an error).
//! * Output formatting: floats `%g` (6 significant digits, see [`crate::format`]), ints decimal,
//!   bools `true`/`false` inside `{}` but `1`/`0` in legacy `[]`.
//! * Vector indexing: the index must be an int; negative or out-of-range indices silently read
//!   element 0. Referencing a float vector without an index yields element 0 if it has one
//!   element, else element `filament_map[current_filament]` (a Bambu quirk: the 1-based extruder
//!   number is used as the index). Referencing other vectors without an index is an error.
//! * **Every branch is evaluated.** Bambu evaluates macros while parsing, so variables used in
//!   `{if}` branches that are not taken, and in both arms of `?:`, must still exist and be
//!   well-typed; `&&`/`||` do not short-circuit. If a template renders here, Bambu Studio can
//!   render it too.
//! * Errors carry the 1-based line/column and the offending line; unknown variables are reported
//!   by name ([`TemplateErrorKind::UnknownVariable`]).

mod eval;
mod parser;
mod regex;

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use parser::{ExprKind, Node};

/// A variable value supplied to templates.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bools(Vec<bool>),
    Ints(Vec<i64>),
    /// Float vector. `NaN` entries are Bambu `nil` values (legacy `[]` prints them as `nil`).
    Floats(Vec<f64>),
    Strs(Vec<String>),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "string",
            Value::Bools(_) => "bool vector",
            Value::Ints(_) => "int vector",
            Value::Floats(_) => "float vector",
            Value::Strs(_) => "string vector",
        }
    }

    pub fn is_vector(&self) -> bool {
        matches!(self, Value::Bools(_) | Value::Ints(_) | Value::Floats(_) | Value::Strs(_))
    }

    /// Number of elements (1 for scalars).
    pub fn len(&self) -> usize {
        match self {
            Value::Bools(v) => v.len(),
            Value::Ints(v) => v.len(),
            Value::Floats(v) => v.len(),
            Value::Strs(v) => v.len(),
            _ => 1,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

macro_rules! value_from {
    ($($t:ty => |$v:ident| $e:expr),* $(,)?) => {
        $(impl From<$t> for Value { fn from($v: $t) -> Self { $e } })*
    };
}
value_from! {
    bool => |v| Value::Bool(v),
    i32 => |v| Value::Int(v as i64),
    i64 => |v| Value::Int(v),
    u32 => |v| Value::Int(v as i64),
    usize => |v| Value::Int(v as i64),
    f32 => |v| Value::Float(v as f64),
    f64 => |v| Value::Float(v),
    &str => |v| Value::Str(v.to_string()),
    String => |v| Value::Str(v),
    Vec<bool> => |v| Value::Bools(v),
    Vec<i32> => |v| Value::Ints(v.into_iter().map(i64::from).collect()),
    Vec<i64> => |v| Value::Ints(v),
    Vec<f64> => |v| Value::Floats(v),
    Vec<String> => |v| Value::Strs(v),
    Vec<&str> => |v| Value::Strs(v.into_iter().map(str::to_string).collect()),
    &[f64] => |v| Value::Floats(v.to_vec()),
    &[i64] => |v| Value::Ints(v.to_vec()),
}
impl<const N: usize> From<[f64; N]> for Value {
    fn from(v: [f64; N]) -> Self {
        Value::Floats(v.to_vec())
    }
}
impl<const N: usize> From<[i64; N]> for Value {
    fn from(v: [i64; N]) -> Self {
        Value::Ints(v.to_vec())
    }
}

/// Variables available to a template.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    vars: HashMap<String, Value>,
    current_filament_id: Option<usize>,
}

impl Context {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets (or replaces) a variable.
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<Value>) -> &mut Self {
        self.vars.insert(name.into(), value.into());
        self
    }

    /// Builder-style [`Context::set`].
    pub fn with(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.set(name, value);
        self
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.vars.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.vars.contains_key(name)
    }

    pub fn remove(&mut self, name: &str) -> Option<Value> {
        self.vars.remove(name)
    }

    /// Copies every variable of `other` into `self` (overriding).
    pub fn extend(&mut self, other: &Context) {
        for (k, v) in &other.vars {
            self.vars.insert(k.clone(), v.clone());
        }
        if other.current_filament_id.is_some() {
            self.current_filament_id = other.current_filament_id;
        }
    }

    /// Variables in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        let mut v: Vec<_> = self.vars.iter().map(|(k, v)| (k.as_str(), v)).collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        v.into_iter()
    }

    pub fn len(&self) -> usize {
        self.vars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// The filament id Bambu passes as `current_extruder_id` to the parser: it selects the
    /// element printed by legacy `[vector]` expansions. Bambu passes the initial filament for
    /// the start G-code, the *new* filament for `change_filament_gcode` and the active filament
    /// for layer-change / timelapse / end G-code. Defaults to 0.
    pub fn set_current_filament_id(&mut self, id: usize) -> &mut Self {
        self.current_filament_id = Some(id);
        self
    }

    pub fn current_filament_id(&self) -> Option<usize> {
        self.current_filament_id
    }
}

/// What went wrong while parsing or rendering a template.
#[derive(Clone, Debug, PartialEq)]
pub enum TemplateErrorKind {
    /// Malformed template text.
    Syntax(String),
    /// A referenced variable is not in the context.
    UnknownVariable(String),
    /// Type errors, division by zero, bad indices, ...
    Eval(String),
}

impl fmt::Display for TemplateErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TemplateErrorKind::Syntax(m) => write!(f, "syntax error: {m}"),
            TemplateErrorKind::UnknownVariable(n) => write!(f, "unknown variable `{n}`"),
            TemplateErrorKind::Eval(m) => f.write_str(m),
        }
    }
}

/// A template parse/render error with its location.
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateError {
    pub kind: TemplateErrorKind,
    /// 1-based line in the template.
    pub line: usize,
    /// 1-based column (in characters).
    pub column: usize,
    /// The text of the offending line.
    pub line_text: String,
    /// Template name (e.g. `machine_start_gcode`) when known.
    pub template: Option<String>,
}

impl TemplateError {
    pub(crate) fn at(src: &str, pos: usize, kind: TemplateErrorKind) -> Self {
        let pos = pos.min(src.len());
        let before = &src[..pos];
        let line = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |i| i + 1);
        let line_end = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
        TemplateError {
            kind,
            line,
            column: src[line_start..pos].chars().count() + 1,
            line_text: src[line_start..line_end].trim_end_matches('\r').to_string(),
            template: None,
        }
    }

    /// Attaches a template name for nicer messages.
    pub fn with_template(mut self, name: impl Into<String>) -> Self {
        self.template = Some(name.into());
        self
    }

    /// The unknown variable's name, if that is the error.
    pub fn unknown_variable(&self) -> Option<&str> {
        match &self.kind {
            TemplateErrorKind::UnknownVariable(n) => Some(n),
            _ => None,
        }
    }
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(t) = &self.template {
            write!(f, "{t}: ")?;
        }
        write!(f, "line {}, column {}: {}\n    {}", self.line, self.column, self.kind, self.line_text)
    }
}

impl std::error::Error for TemplateError {}

/// How a variable is referenced (see [`Template::variable_uses`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum VarForm {
    /// `{name}` / `{if name}` — scalar use.
    Scalar,
    /// `{name[expr]}` — vector element.
    Indexed,
    /// `[name]` — legacy expansion (scalar, or vector element of the current filament).
    Legacy,
    /// `[name[idx]]` — the vector part.
    LegacyIndexed,
    /// `[vec[name]]` — the (integer) index variable.
    LegacyIndexVar,
}

/// One reference to a variable in a template.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VarUse {
    pub name: String,
    pub form: VarForm,
    /// 1-based line of the reference.
    pub line: usize,
}

/// A parsed template, reusable for many renders.
#[derive(Clone, Debug)]
pub struct Template {
    source: String,
    name: Option<String>,
    nodes: Vec<Node>,
}

impl Template {
    pub fn parse(source: impl Into<String>) -> Result<Self, TemplateError> {
        let source = source.into();
        let nodes = parser::Parser::parse_template(&source)?;
        Ok(Template { source, name: None, nodes })
    }

    /// Parses a template and names it in error messages (e.g. `"machine_start_gcode"`).
    pub fn named(name: impl Into<String>, source: impl Into<String>) -> Result<Self, TemplateError> {
        let name = name.into();
        let mut t = Self::parse(source).map_err(|e| e.with_template(name.clone()))?;
        t.name = Some(name);
        Ok(t)
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn render(&self, ctx: &Context) -> Result<String, TemplateError> {
        self.render_layered(&[ctx])
    }

    /// Renders with several contexts; earlier layers take precedence (e.g.
    /// `&[&per_toolchange_vars, &print_vars]`, like Bambu's `config_override` over the print
    /// config). The current filament id is taken from the first layer that sets it.
    pub fn render_layered(&self, layers: &[&Context]) -> Result<String, TemplateError> {
        let current = layers.iter().find_map(|c| c.current_filament_id).unwrap_or(0);
        let mut ev = eval::Evaluator::new(&self.source, layers, current);
        let mut out = String::with_capacity(self.source.len());
        ev.render_nodes(&self.nodes, &mut out).map_err(|e| match &self.name {
            Some(n) => e.with_template(n.clone()),
            None => e,
        })?;
        Ok(out)
    }

    /// Every variable reference, in source order.
    pub fn variable_uses(&self) -> Vec<VarUse> {
        let mut out = Vec::new();
        collect_nodes(&self.source, &self.nodes, &mut out);
        out
    }

    /// Sorted, de-duplicated names of all variables the template references (in any branch:
    /// Bambu evaluates all branches, so all of them must be supplied).
    ///
    /// Legacy `[name_N]` references are reported as written; they are also satisfied by a
    /// vector `name`.
    pub fn required_variables(&self) -> Vec<String> {
        let set: BTreeSet<String> = self.variable_uses().into_iter().map(|u| u.name).collect();
        set.into_iter().collect()
    }

    /// Required variables that none of `layers` defines.
    pub fn missing_variables(&self, layers: &[&Context]) -> Vec<String> {
        let defined = |n: &str| layers.iter().any(|c| c.contains(n));
        let mut out = BTreeSet::new();
        for u in self.variable_uses() {
            if defined(&u.name) {
                continue;
            }
            // `[name_N]` falls back to element N of vector `name`; `[name_[i]]` to `name`.
            let fallback = match u.form {
                VarForm::Legacy => u.name.rfind('_').map(|i| &u.name[..i]),
                VarForm::LegacyIndexed => u.name.strip_suffix('_'),
                _ => None,
            };
            if fallback.is_some_and(defined) {
                continue;
            }
            out.insert(u.name);
        }
        out.into_iter().collect()
    }
}

fn line_of(src: &str, pos: usize) -> usize {
    src[..pos.min(src.len())].matches('\n').count() + 1
}

fn collect_nodes(src: &str, nodes: &[Node], out: &mut Vec<VarUse>) {
    for node in nodes {
        match node {
            Node::Text(_) => {}
            Node::Expr(e) => collect_expr(src, e, out),
            Node::Legacy { name, index, pos } => match index {
                None => out.push(VarUse { name: name.clone(), form: VarForm::Legacy, line: line_of(src, *pos) }),
                Some((idx, ipos)) => {
                    out.push(VarUse {
                        name: name.clone(),
                        form: VarForm::LegacyIndexed,
                        line: line_of(src, *pos),
                    });
                    out.push(VarUse {
                        name: idx.clone(),
                        form: VarForm::LegacyIndexVar,
                        line: line_of(src, *ipos),
                    });
                }
            },
            Node::If { branches, otherwise } => {
                for (cond, body) in branches {
                    collect_expr(src, cond, out);
                    collect_nodes(src, body, out);
                }
                if let Some(body) = otherwise {
                    collect_nodes(src, body, out);
                }
            }
        }
    }
}

fn collect_expr(src: &str, e: &parser::Expr, out: &mut Vec<VarUse>) {
    match &e.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) => {}
        ExprKind::Var(n) => out.push(VarUse { name: n.clone(), form: VarForm::Scalar, line: line_of(src, e.pos) }),
        ExprKind::Index(n, idx) => {
            out.push(VarUse { name: n.clone(), form: VarForm::Indexed, line: line_of(src, e.pos) });
            collect_expr(src, idx, out);
        }
        ExprKind::Neg(x) | ExprKind::Plus(x) | ExprKind::Not(x) => collect_expr(src, x, out),
        ExprKind::Binary(_, a, b) => {
            collect_expr(src, a, out);
            collect_expr(src, b, out);
        }
        ExprKind::Ternary(c, a, b) => {
            collect_expr(src, c, out);
            collect_expr(src, a, out);
            collect_expr(src, b, out);
        }
        ExprKind::Regex { lhs, .. } => collect_expr(src, lhs, out),
        ExprKind::Call(_, args) => args.iter().for_each(|a| collect_expr(src, a, out)),
    }
}

/// Parses and renders `template` with `ctx`.
pub fn render(template: &str, ctx: &Context) -> Result<String, TemplateError> {
    Template::parse(template)?.render(ctx)
}

/// Parses and renders `template` with layered contexts (earlier layers win).
pub fn render_layered(template: &str, layers: &[&Context]) -> Result<String, TemplateError> {
    Template::parse(template)?.render_layered(layers)
}

/// Sorted names of every variable `template` references (see [`Template::required_variables`]).
pub fn required_variables(template: &str) -> Result<Vec<String>, TemplateError> {
    Ok(Template::parse(template)?.required_variables())
}

/// Evaluates a boolean expression such as a `compatible_printers_condition`
/// (Bambu's `PlaceholderParser::evaluate_boolean_expression`).
pub fn eval_condition(expr: &str, ctx: &Context) -> Result<bool, TemplateError> {
    let e = parser::Parser::parse_condition(expr)?;
    let layers = [ctx];
    let mut ev = eval::Evaluator::new(expr, &layers, ctx.current_filament_id.unwrap_or(0));
    ev.eval_bool(&e)
}
