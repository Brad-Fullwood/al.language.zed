//! `JsonObject`, `JsonArray`, `JsonToken` and `JsonValue`.
//!
//! AL's JSON types are references: `Obj2 := Obj1` shares one object, and a
//! token from `Obj.Get('child', Token)` changes the child inside `Obj`. A
//! JSON value is therefore a [`JsonRef`] into the [`JsonArena`] the dispatch
//! context owns. A declared variable gets its node id when declared, so
//! copies made before its first use still share one node; the node itself,
//! an empty value of the declared type, is made on first use.
//!
//! Text is written compactly (`{"a":1}`), as BC's `WriteTo` does. Numbers
//! keep their exact decimal value. Dates and times are written in the XML
//! format (`2026-09-05`).

use std::collections::{HashMap, HashSet};

use rust_decimal::prelude::*;

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_error;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::Value;

/// The declared JSON type of a variable or value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum JsonKind {
    Object,
    Array,
    Token,
    Value,
}

impl JsonKind {
    fn name(self) -> &'static str {
        match self {
            JsonKind::Object => "JsonObject",
            JsonKind::Array => "JsonArray",
            JsonKind::Token => "JsonToken",
            JsonKind::Value => "JsonValue",
        }
    }
}

/// A JSON variable's value: its type and the node it refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsonRef {
    pub kind: JsonKind,
    pub node: Option<usize>,
}

/// A scalar JSON value.
#[derive(Debug, Clone, PartialEq)]
enum Scalar {
    Null,
    Bool(bool),
    Number(Decimal),
    Text(String),
}

#[derive(Debug, Clone)]
enum Node {
    Object(Vec<(String, usize)>),
    Array(Vec<usize>),
    Scalar(Scalar),
}

/// Every JSON node the running test has made.
#[derive(Debug, Default, Clone)]
pub struct JsonArena {
    nodes: HashMap<usize, Node>,
    /// Whether each node already sits inside an object or array: adding it
    /// elsewhere then adds a copy, as BC does.
    attached: HashSet<usize>,
}

impl JsonArena {
    fn push(&mut self, node: Node) -> usize {
        let id = fresh_id();
        self.nodes.insert(id, node);
        id
    }

    /// Make sure node `id` exists: a variable's node is created on first
    /// use, as an empty value of its declared kind.
    fn materialize(&mut self, id: usize, kind: JsonKind) {
        self.nodes.entry(id).or_insert_with(|| empty_node(kind));
    }

    fn set(&mut self, id: usize, node: Node) {
        self.nodes.insert(id, node);
    }

    fn deep_copy(&mut self, id: usize) -> usize {
        let node = match self.nodes[&id].clone() {
            Node::Object(entries) => Node::Object(
                entries
                    .into_iter()
                    .map(|(key, child)| {
                        let copy = self.deep_copy(child);
                        self.attached.insert(copy);
                        (key, copy)
                    })
                    .collect(),
            ),
            Node::Array(items) => Node::Array(
                items
                    .into_iter()
                    .map(|child| {
                        let copy = self.deep_copy(child);
                        self.attached.insert(copy);
                        copy
                    })
                    .collect(),
            ),
            scalar => scalar,
        };
        self.push(node)
    }

    /// The node to place inside a container for `value`: the referenced node
    /// itself, a copy of it when it already has a parent, or a new scalar.
    fn child_for(&mut self, value: &Value) -> Result<usize, String> {
        let id = match value {
            Value::Json(JsonRef {
                node: Some(id),
                kind,
            }) => {
                self.materialize(*id, *kind);
                if self.attached.contains(id) {
                    self.deep_copy(*id)
                } else {
                    *id
                }
            }
            Value::Json(JsonRef { kind, node: None }) => self.push(empty_node(*kind)),
            other => self.push(Node::Scalar(scalar_of(other)?)),
        };
        self.attached.insert(id);
        Ok(id)
    }

    fn write(&self, id: usize, out: &mut String) {
        match &self.nodes[&id] {
            Node::Object(entries) => {
                out.push('{');
                for (index, (key, child)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_string(key, out);
                    out.push(':');
                    self.write(*child, out);
                }
                out.push('}');
            }
            Node::Array(items) => {
                out.push('[');
                for (index, child) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    self.write(*child, out);
                }
                out.push(']');
            }
            Node::Scalar(Scalar::Null) => out.push_str("null"),
            Node::Scalar(Scalar::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
            Node::Scalar(Scalar::Number(n)) => out.push_str(&n.normalize().to_string()),
            Node::Scalar(Scalar::Text(t)) => write_string(t, out),
        }
    }

    fn text_of(&self, id: usize) -> String {
        let mut out = String::new();
        self.write(id, &mut out);
        out
    }

    fn import(&mut self, parsed: &serde_json::Value) -> Result<usize, String> {
        let node = match parsed {
            serde_json::Value::Object(map) => {
                let mut entries = Vec::with_capacity(map.len());
                for (key, value) in map {
                    let child = self.import(value)?;
                    self.attached.insert(child);
                    entries.push((key.clone(), child));
                }
                Node::Object(entries)
            }
            serde_json::Value::Array(items) => {
                let mut children = Vec::with_capacity(items.len());
                for value in items {
                    let child = self.import(value)?;
                    self.attached.insert(child);
                    children.push(child);
                }
                Node::Array(children)
            }
            serde_json::Value::Null => Node::Scalar(Scalar::Null),
            serde_json::Value::Bool(b) => Node::Scalar(Scalar::Bool(*b)),
            serde_json::Value::Number(n) => {
                let text = n.to_string();
                let number = Decimal::from_str(&text)
                    .or_else(|_| Decimal::from_scientific(&text))
                    .map_err(|_| format!("the JSON number {text} is outside the Decimal range"))?;
                Node::Scalar(Scalar::Number(number))
            }
            serde_json::Value::String(s) => Node::Scalar(Scalar::Text(s.clone())),
        };
        Ok(self.push(node))
    }

    /// Follow a `SelectToken` path: `$.a.b[0]`, `a.b`, `['a b'].c`.
    fn select(&self, from: usize, path: &str) -> Result<Option<usize>, String> {
        let mut current = from;
        let mut rest = path.trim().strip_prefix('$').unwrap_or(path.trim());
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('.') {
                rest = after;
                continue;
            }
            if let Some(after) = rest.strip_prefix('[') {
                let close = after
                    .find(']')
                    .ok_or_else(|| format!("unclosed '[' in path '{path}'"))?;
                let inside = after[..close].trim();
                rest = &after[close + 1..];
                let next = if let Some(key) = inside
                    .strip_prefix('\'')
                    .and_then(|key| key.strip_suffix('\''))
                {
                    self.member(current, key)
                } else {
                    let index: usize = inside.parse().map_err(|_| {
                        format!("'{inside}' is not an array index in path '{path}'")
                    })?;
                    match &self.nodes[&current] {
                        Node::Array(items) => items.get(index).copied(),
                        _ => None,
                    }
                };
                match next {
                    Some(next) => current = next,
                    None => return Ok(None),
                }
                continue;
            }
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            let key = &rest[..end];
            rest = &rest[end..];
            match self.member(current, key) {
                Some(next) => current = next,
                None => return Ok(None),
            }
        }
        Ok(Some(current))
    }

    fn member(&self, object: usize, key: &str) -> Option<usize> {
        match &self.nodes[&object] {
            Node::Object(entries) => entries
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, child)| *child),
            _ => None,
        }
    }
}

fn write_string(text: &str, out: &mut String) {
    out.push_str(&serde_json::Value::String(text.to_string()).to_string());
}

/// A variable's initial node: an empty object or array, or a null value
/// (also for a token no one has assigned, which `ReadFrom` can fill).
fn empty_node(kind: JsonKind) -> Node {
    match kind {
        JsonKind::Object => Node::Object(Vec::new()),
        JsonKind::Array => Node::Array(Vec::new()),
        JsonKind::Value | JsonKind::Token => Node::Scalar(Scalar::Null),
    }
}

/// Node ids are unique across every arena, so a variable can be given its
/// id when declared, before any arena holds the node: copies of the variable
/// then share it, as AL's reference semantics require.
fn fresh_id() -> usize {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn scalar_of(value: &Value) -> Result<Scalar, String> {
    Ok(match value {
        Value::Null | Value::Empty => Scalar::Null,
        Value::Boolean(b) => Scalar::Bool(*b),
        Value::Integer(n) | Value::BigInteger(n) => Scalar::Number(Decimal::from(*n)),
        Value::Decimal(d) => Scalar::Number(*d),
        Value::Text(t) | Value::Code(t) | Value::TextBuilder(t) => Scalar::Text(t.clone()),
        Value::Char(c) => Scalar::Text(c.to_string()),
        Value::Guid(g) => Scalar::Text(g.clone()),
        Value::Option { member, .. } => Scalar::Text(member.clone()),
        Value::Date(_) | Value::Time(_) | Value::DateTime(_) => {
            Scalar::Text(crate::interpreter::dispatch::render_value_xml(value))
        }
        other => {
            return Err(format!(
                "a {} cannot be stored in JSON by the local runtime",
                other.type_name()
            ))
        }
    })
}

/// Methods each JSON type supports locally.
pub fn supports_json_method(kind: JsonKind, method: &str) -> bool {
    let method = method.to_ascii_lowercase();
    let common = matches!(
        method.as_str(),
        "writeto" | "readfrom" | "selecttoken" | "clone" | "astoken"
    );
    common
        || match kind {
            JsonKind::Object => matches!(
                method.as_str(),
                "add"
                    | "get"
                    | "contains"
                    | "remove"
                    | "replace"
                    | "keys"
                    | "values"
                    | "gettext"
                    | "getinteger"
                    | "getbiginteger"
                    | "getdecimal"
                    | "getboolean"
                    | "getcode"
            ),
            JsonKind::Array => matches!(
                method.as_str(),
                "add"
                    | "get"
                    | "count"
                    | "insert"
                    | "removeat"
                    | "set"
                    | "indexof"
                    | "gettext"
                    | "getinteger"
                    | "getbiginteger"
                    | "getdecimal"
                    | "getboolean"
                    | "getcode"
            ),
            JsonKind::Token => matches!(
                method.as_str(),
                "isobject" | "isarray" | "isvalue" | "asobject" | "asarray" | "asvalue"
            ),
            JsonKind::Value => matches!(
                method.as_str(),
                "astext"
                    | "ascode"
                    | "asinteger"
                    | "asbiginteger"
                    | "asdecimal"
                    | "asboolean"
                    | "isnull"
                    | "isundefined"
                    | "setvalue"
                    | "setvaluetonull"
            ),
        }
}

/// The node behind the JSON variable `recv`, allocating an empty one on
/// first use and storing it back on the variable.
fn node_of(
    recv: &str,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<(JsonKind, usize), String> {
    let json = match stack.lookup(recv) {
        Some(Value::Json(json)) => *json,
        _ => return Err(format!("'{recv}' is not a JSON variable")),
    };
    let node = match json.node {
        Some(node) => node,
        None => {
            let node = fresh_id();
            if let Some(Value::Json(slot)) = stack.lookup_mut(recv) {
                slot.node = Some(node);
            }
            node
        }
    };
    ctx.json.materialize(node, json.kind);
    Ok((json.kind, node))
}

fn reference(kind: JsonKind, node: usize) -> Value {
    Value::Json(JsonRef {
        kind,
        node: Some(node),
    })
}

fn text_arg(value: Option<&Value>, what: &str) -> Result<String, String> {
    match value {
        Some(Value::Text(t) | Value::Code(t)) => Ok(t.clone()),
        Some(other) => Err(format!("{what} must be Text, got {}", other.type_name())),
        None => Err(format!("{what} is missing")),
    }
}

fn index_arg(value: Option<&Value>, len: usize, inclusive: bool) -> Result<usize, String> {
    let index = match value {
        Some(Value::Integer(n)) => *n,
        _ => return Err("a JSON array index must be an Integer".to_string()),
    };
    let limit = if inclusive {
        len
    } else {
        len.saturating_sub(1)
    };
    usize::try_from(index)
        .ok()
        .filter(|index| *index <= limit && (inclusive || len > 0))
        .ok_or_else(|| format!("index {index} is outside the JSON array of {len} elements"))
}

/// `value.AsInteger()` and the typed getters: the scalar converted to `as`.
fn scalar_as(scalar: &Scalar, as_type: &str) -> Result<Value, String> {
    let number = |scalar: &Scalar| match scalar {
        Scalar::Number(n) => Ok(*n),
        other => Err(format!("the JSON value {other:?} is not a number")),
    };
    Ok(match as_type {
        "text" => Value::Text(match scalar {
            Scalar::Text(t) => t.clone(),
            Scalar::Number(n) => n.normalize().to_string(),
            Scalar::Bool(b) => b.to_string(),
            Scalar::Null => return Err("the JSON value is null".to_string()),
        }),
        "code" => match scalar_as(scalar, "text")? {
            Value::Text(text) => Value::Code(text.to_uppercase()),
            other => other,
        },
        "integer" | "biginteger" => {
            let n = number(scalar)?;
            let whole = n
                .trunc()
                .to_i64()
                .filter(|_| n.fract().is_zero())
                .ok_or_else(|| format!("the JSON value {n} is not an Integer"))?;
            if as_type == "integer" {
                Value::Integer(whole)
            } else {
                Value::BigInteger(whole)
            }
        }
        "decimal" => Value::Decimal(number(scalar)?),
        "boolean" => match scalar {
            Scalar::Bool(b) => Value::Boolean(*b),
            other => return Err(format!("the JSON value {other:?} is not a Boolean")),
        },
        other => return Err(format!("unsupported JSON conversion to {other}")),
    })
}

/// Run `recv.method(args)` on a JSON variable. `var` results (the token of
/// `Get`, the text of `WriteTo`) go to `ctx.var_writebacks`.
pub(crate) fn dispatch_json_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    match run(recv, method, &args, stack, ctx) {
        Ok(value) => Eval::Normal(value),
        Err(error) => eval_error(format!("{method}: {error}")),
    }
}

fn run(
    recv: &str,
    method: &str,
    args: &[Value],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<Value, String> {
    let (kind, node) = node_of(recv, stack, ctx)?;
    let lower = method.to_ascii_lowercase();
    ctx.var_writebacks.clear();
    let arena = &mut ctx.json;
    match lower.as_str() {
        "writeto" => {
            let text = arena.text_of(node);
            return Ok(match args {
                [] => Value::Text(text),
                _ => {
                    ctx.var_writebacks.push((0, Value::Text(text)));
                    Value::Boolean(true)
                }
            });
        }
        "readfrom" => {
            let text = text_arg(args.first(), "the JSON text")?;
            let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) else {
                return Ok(Value::Boolean(false));
            };
            let fits = match kind {
                JsonKind::Object => parsed.is_object(),
                JsonKind::Array => parsed.is_array(),
                JsonKind::Value => !parsed.is_object() && !parsed.is_array(),
                JsonKind::Token => true,
            };
            if !fits {
                return Ok(Value::Boolean(false));
            }
            let imported = arena.import(&parsed)?;
            let imported = arena.nodes[&imported].clone();
            arena.set(node, imported);
            return Ok(Value::Boolean(true));
        }
        "selecttoken" => {
            let path = text_arg(args.first(), "the path")?;
            return Ok(match arena.select(node, &path)? {
                Some(found) => {
                    ctx.var_writebacks
                        .push((1, reference(JsonKind::Token, found)));
                    Value::Boolean(true)
                }
                None => Value::Boolean(false),
            });
        }
        "clone" => {
            let copy = arena.deep_copy(node);
            return Ok(reference(kind, copy));
        }
        "astoken" => return Ok(reference(JsonKind::Token, node)),
        _ => {}
    }
    let current = arena.nodes[&node].clone();
    match (kind, lower.as_str(), current) {
        (JsonKind::Token, "isobject", current) => {
            Ok(Value::Boolean(matches!(current, Node::Object(_))))
        }
        (JsonKind::Token, "isarray", current) => {
            Ok(Value::Boolean(matches!(current, Node::Array(_))))
        }
        (JsonKind::Token, "isvalue", current) => {
            Ok(Value::Boolean(matches!(current, Node::Scalar(_))))
        }
        (JsonKind::Token, "asobject", Node::Object(_)) => Ok(reference(JsonKind::Object, node)),
        (JsonKind::Token, "asarray", Node::Array(_)) => Ok(reference(JsonKind::Array, node)),
        (JsonKind::Token, "asvalue", Node::Scalar(_)) => Ok(reference(JsonKind::Value, node)),
        (JsonKind::Token, "asobject" | "asarray" | "asvalue", _) => {
            Err(format!("the token is not a JSON {}", &lower[2..]))
        }
        (JsonKind::Object, "add", Node::Object(mut entries)) => {
            let key = text_arg(args.first(), "the key")?;
            if entries.iter().any(|(name, _)| *name == key) {
                return Err(format!("the key '{key}' already exists"));
            }
            let child = arena.child_for(args.get(1).ok_or("the value is missing")?)?;
            entries.push((key, child));
            arena.set(node, Node::Object(entries));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Object, "replace", Node::Object(mut entries)) => {
            let key = text_arg(args.first(), "the key")?;
            let Some(at) = entries.iter().position(|(name, _)| *name == key) else {
                return Ok(Value::Boolean(false));
            };
            entries[at].1 = arena.child_for(args.get(1).ok_or("the value is missing")?)?;
            arena.set(node, Node::Object(entries));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Object, "remove", Node::Object(mut entries)) => {
            let key = text_arg(args.first(), "the key")?;
            let before = entries.len();
            entries.retain(|(name, _)| *name != key);
            let removed = entries.len() != before;
            arena.set(node, Node::Object(entries));
            Ok(Value::Boolean(removed))
        }
        (JsonKind::Object, "contains", Node::Object(entries)) => {
            let key = text_arg(args.first(), "the key")?;
            Ok(Value::Boolean(entries.iter().any(|(name, _)| *name == key)))
        }
        (JsonKind::Object, "get", Node::Object(entries)) => {
            let key = text_arg(args.first(), "the key")?;
            Ok(match entries.iter().find(|(name, _)| *name == key) {
                Some((_, child)) => {
                    ctx.var_writebacks
                        .push((1, reference(JsonKind::Token, *child)));
                    Value::Boolean(true)
                }
                None => Value::Boolean(false),
            })
        }
        (JsonKind::Object, "keys", Node::Object(entries)) => Ok(Value::List(
            entries
                .into_iter()
                .map(|(key, _)| Value::Text(key))
                .collect(),
        )),
        (JsonKind::Object, "values", Node::Object(entries)) => Ok(Value::List(
            entries
                .into_iter()
                .map(|(_, child)| reference(JsonKind::Token, child))
                .collect(),
        )),
        (JsonKind::Object, getter, Node::Object(entries)) if getter.starts_with("get") => {
            let key = text_arg(args.first(), "the key")?;
            let child = entries
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, child)| *child)
                .ok_or_else(|| format!("the key '{key}' does not exist"))?;
            match &arena.nodes[&child] {
                Node::Scalar(scalar) => scalar_as(scalar, &getter[3..]),
                _ => Err(format!("the value of '{key}' is not a JSON value")),
            }
        }
        (JsonKind::Array, "add", Node::Array(mut items)) => {
            let child = arena.child_for(args.first().ok_or("the value is missing")?)?;
            items.push(child);
            arena.set(node, Node::Array(items));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Array, "insert", Node::Array(mut items)) => {
            let at = index_arg(args.first(), items.len(), true)?;
            let child = arena.child_for(args.get(1).ok_or("the value is missing")?)?;
            items.insert(at, child);
            arena.set(node, Node::Array(items));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Array, "set", Node::Array(mut items)) => {
            let at = index_arg(args.first(), items.len(), false)?;
            items[at] = arena.child_for(args.get(1).ok_or("the value is missing")?)?;
            arena.set(node, Node::Array(items));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Array, "removeat", Node::Array(mut items)) => {
            let at = index_arg(args.first(), items.len(), false)?;
            items.remove(at);
            arena.set(node, Node::Array(items));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Array, "count", Node::Array(items)) => Ok(Value::Integer(items.len() as i64)),
        (JsonKind::Array, "get", Node::Array(items)) => {
            let at = index_arg(args.first(), items.len(), false)?;
            ctx.var_writebacks
                .push((1, reference(JsonKind::Token, items[at])));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Array, "indexof", Node::Array(items)) => {
            let wanted = match args.first() {
                Some(Value::Json(JsonRef { node: Some(id), .. })) => arena.text_of(*id),
                Some(other) => {
                    let probe = arena.child_for(other)?;
                    arena.text_of(probe)
                }
                None => return Err("the value is missing".to_string()),
            };
            Ok(Value::Integer(
                items
                    .iter()
                    .position(|item| arena.text_of(*item) == wanted)
                    .map_or(-1, |at| at as i64),
            ))
        }
        (JsonKind::Array, getter, Node::Array(items)) if getter.starts_with("get") => {
            let at = index_arg(args.first(), items.len(), false)?;
            match &arena.nodes[&items[at]] {
                Node::Scalar(scalar) => scalar_as(scalar, &getter[3..]),
                _ => Err(format!("element {at} is not a JSON value")),
            }
        }
        (JsonKind::Value, "isnull", Node::Scalar(scalar)) => {
            Ok(Value::Boolean(scalar == Scalar::Null))
        }
        (JsonKind::Value, "isundefined", _) => Ok(Value::Boolean(false)),
        (JsonKind::Value, "setvaluetonull", _) => {
            arena.set(node, Node::Scalar(Scalar::Null));
            Ok(Value::Empty)
        }
        (JsonKind::Value, "setvalue", _) => {
            let scalar = scalar_of(args.first().ok_or("the value is missing")?)?;
            arena.set(node, Node::Scalar(scalar));
            Ok(Value::Empty)
        }
        (JsonKind::Value, conversion, Node::Scalar(scalar)) if conversion.starts_with("as") => {
            scalar_as(&scalar, &conversion[2..])
        }
        (kind, _, _) => Err(format!(
            "{}.{method} is not supported by the local runtime here",
            kind.name()
        )),
    }
}

/// The zero value of a declared JSON type (`JsonObject`, ...).
pub(crate) fn default_for(type_name: &str) -> Option<Value> {
    let kind = match type_name.to_ascii_lowercase().as_str() {
        "jsonobject" => JsonKind::Object,
        "jsonarray" => JsonKind::Array,
        "jsontoken" => JsonKind::Token,
        "jsonvalue" => JsonKind::Value,
        _ => return None,
    };
    Some(Value::Json(JsonRef {
        kind,
        node: Some(fresh_id()),
    }))
}
