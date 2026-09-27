//! `JsonObject`, `JsonArray`, `JsonToken` and `JsonValue`.
//!
//! AL's JSON types are references: `Obj2 := Obj1` shares one object, and a
//! token from `Obj.Get('child', Token)` changes the child inside `Obj`. A
//! JSON value is therefore a [`JsonRef`], a handle that the [`JsonArena`] the
//! dispatch context owns maps to a node. `Obj2 := Obj1` copies the handle,
//! and `Get` makes a new handle to the child's node. `ReadFrom` points the
//! handle at a new node and leaves the old one in the tree that holds it,
//! as BC disconnects the variable from its tree. A declared variable gets
//! its handle when declared, so copies made before its first use still
//! share one; the node, an empty value of the declared type, is made on
//! first use.
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

/// A JSON variable's value: its type and the handle of the node it refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsonRef {
    pub kind: JsonKind,
    pub handle: Option<usize>,
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
    /// The node each [`JsonRef`] handle refers to.
    targets: HashMap<usize, usize>,
}

impl JsonArena {
    fn push(&mut self, node: Node) -> usize {
        let id = fresh_id();
        self.nodes.insert(id, node);
        id
    }

    /// The node `handle` refers to. A variable's node is created on first
    /// use, as an empty value of its declared kind.
    fn target(&mut self, handle: usize, kind: JsonKind) -> usize {
        if let Some(node) = self.targets.get(&handle) {
            return *node;
        }
        let node = self.push(empty_node(kind));
        self.targets.insert(handle, node);
        node
    }

    /// A new reference of `kind` to `node`.
    fn reference(&mut self, kind: JsonKind, node: usize) -> Value {
        let handle = fresh_id();
        self.targets.insert(handle, node);
        Value::Json(JsonRef {
            kind,
            handle: Some(handle),
        })
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
                handle: Some(handle),
                kind,
            }) => {
                let node = self.target(*handle, *kind);
                if self.attached.contains(&node) {
                    self.deep_copy(node)
                } else {
                    node
                }
            }
            Value::Json(JsonRef { kind, handle: None }) => self.push(empty_node(*kind)),
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

    /// Every node `path` selects from `from`, in document order.
    fn select(&self, from: usize, path: &str) -> Result<Vec<usize>, String> {
        let steps = parse_path(path).map_err(PathError::into_message)?;
        Ok(self.follow(from, &[from], &steps))
    }

    /// The nodes `steps` reach from `start`. `root` is what `$` names in a
    /// filter.
    fn follow(&self, root: usize, start: &[usize], steps: &[PathStep]) -> Vec<usize> {
        let mut current = start.to_vec();
        let mut descend = false;
        for step in steps {
            if matches!(step, PathStep::Descend) {
                descend = true;
                continue;
            }
            let scope = if std::mem::take(&mut descend) {
                let mut all = Vec::new();
                for node in current {
                    self.self_and_descendants(node, &mut all);
                }
                all
            } else {
                current
            };
            current = scope
                .into_iter()
                .flat_map(|node| self.step(root, node, step))
                .collect();
        }
        current
    }

    fn step(&self, root: usize, node: usize, step: &PathStep) -> Vec<usize> {
        match (step, &self.nodes[&node]) {
            (PathStep::Member(key), _) => self.member(node, key).into_iter().collect(),
            (PathStep::Index(at), Node::Array(items)) => {
                items.get(*at).copied().into_iter().collect()
            }
            (PathStep::Wildcard, Node::Array(items)) => items.clone(),
            (PathStep::Wildcard, Node::Object(entries)) => {
                entries.iter().map(|(_, child)| *child).collect()
            }
            (PathStep::Filter(filter), Node::Array(items)) => items
                .iter()
                .copied()
                .filter(|item| self.accepts(root, *item, filter))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn self_and_descendants(&self, node: usize, out: &mut Vec<usize>) {
        out.push(node);
        match &self.nodes[&node] {
            Node::Object(entries) => {
                for (_, child) in entries {
                    self.self_and_descendants(*child, out);
                }
            }
            Node::Array(items) => {
                for child in items {
                    self.self_and_descendants(*child, out);
                }
            }
            Node::Scalar(_) => {}
        }
    }

    /// Whether array element `item` passes `filter`. A path operand that
    /// selects several nodes passes when any of them does.
    fn accepts(&self, root: usize, item: usize, filter: &PathFilter) -> bool {
        match filter {
            PathFilter::Any(parts) => parts.iter().any(|part| self.accepts(root, item, part)),
            PathFilter::All(parts) => parts.iter().all(|part| self.accepts(root, item, part)),
            PathFilter::Exists(operand) => !self.operand(root, item, operand).is_empty(),
            PathFilter::Compare(left, op, right) => {
                let left = self.operand(root, item, left);
                let right = self.operand(root, item, right);
                left.iter()
                    .any(|left| right.iter().any(|right| compare(left, *op, right)))
            }
        }
    }

    fn operand(&self, root: usize, item: usize, operand: &FilterOperand) -> Vec<Option<Scalar>> {
        match operand {
            FilterOperand::Literal(scalar) => vec![Some(scalar.clone())],
            FilterOperand::Path { from_root, steps } => {
                let start = if *from_root { root } else { item };
                self.follow(root, &[start], steps)
                    .into_iter()
                    .map(|node| match &self.nodes[&node] {
                        Node::Scalar(scalar) => Some(scalar.clone()),
                        _ => None,
                    })
                    .collect()
            }
        }
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

/// One step of a `SelectToken` path.
#[derive(Debug, Clone)]
enum PathStep {
    /// `.name` or `['name']`.
    Member(String),
    /// `[n]`.
    Index(usize),
    /// `.*` or `[*]`: every child.
    Wildcard,
    /// `..`: the next step applies to every descendant as well.
    Descend,
    /// `[?(...)]`: the array elements the filter accepts.
    Filter(Box<PathFilter>),
}

#[derive(Debug, Clone)]
enum PathFilter {
    /// `a || b`.
    Any(Vec<PathFilter>),
    /// `a && b`.
    All(Vec<PathFilter>),
    /// `@.name`: the path selects something.
    Exists(FilterOperand),
    Compare(FilterOperand, CompareOp, FilterOperand),
}

#[derive(Debug, Clone)]
enum FilterOperand {
    /// `@.a.b` from the element, `$.a.b` from the token queried.
    Path {
        from_root: bool,
        steps: Vec<PathStep>,
    },
    Literal(Scalar),
}

#[derive(Debug, Clone, Copy)]
enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Why a path does not parse.
enum PathError {
    /// Malformed, as Business Central would also report.
    Invalid(String),
    /// Valid JSONPath the local runtime does not follow: a slice, a union,
    /// a regular expression, a grouped filter.
    Unsupported(String),
}

impl PathError {
    fn into_message(self) -> String {
        match self {
            PathError::Invalid(message) | PathError::Unsupported(message) => message,
        }
    }
}

/// The step of `path` the local runtime does not follow, as an error
/// message, or `None` when it follows all of them (or the path is
/// malformed, which Business Central rejects too). The test router asks
/// this of a literal `SelectToken` path.
pub fn unsupported_path_step(path: &str) -> Option<String> {
    match parse_path(path) {
        Err(PathError::Unsupported(message)) => Some(message),
        _ => None,
    }
}

fn unsupported(what: &str, text: &str, path: &str) -> PathError {
    PathError::Unsupported(format!(
        "the {what} '{text}' in path '{path}' is not supported by the local runtime"
    ))
}

/// Parse a `SelectToken` path: `$.a.b[0]`, `a.b`, `['a b'].c`, `$..c`,
/// `$.items[*].id`, `$.items[?(@.qty > 1 && @.id == 'A')].price`.
fn parse_path(path: &str) -> Result<Vec<PathStep>, PathError> {
    let text = path.trim();
    let rest = text.strip_prefix('$').unwrap_or(text);
    // A relative path starts with a name: `a.b`.
    let relative = !text.starts_with('$') && !rest.is_empty() && !rest.starts_with(['.', '[']);
    let (steps, rest) = parse_steps(rest, path, relative, false)?;
    if !rest.is_empty() {
        return Err(PathError::Invalid(format!(
            "unexpected '{rest}' in path '{path}'"
        )));
    }
    Ok(steps)
}

/// Read steps from the start of `rest` until one does not start there.
/// `leading_name` reads a bare name first. In a filter a name also ends at
/// white space and at an operator.
fn parse_steps<'t>(
    mut rest: &'t str,
    path: &str,
    leading_name: bool,
    in_filter: bool,
) -> Result<(Vec<PathStep>, &'t str), PathError> {
    let name_end = |text: &str| {
        text.find(|c: char| {
            c == '.' || c == '[' || (in_filter && (c.is_whitespace() || "=!<>&|)".contains(c)))
        })
        .unwrap_or(text.len())
    };
    let mut steps = Vec::new();
    if leading_name {
        let end = name_end(rest);
        steps.push(PathStep::Member(rest[..end].to_string()));
        rest = &rest[end..];
    }
    loop {
        if let Some(after) = rest.strip_prefix("..") {
            steps.push(PathStep::Descend);
            if after.starts_with('[') {
                rest = after;
                continue;
            }
            let (step, after) = dotted_step(after, path, name_end)?;
            steps.push(step);
            rest = after;
        } else if let Some(after) = rest.strip_prefix('.') {
            let (step, after) = dotted_step(after, path, name_end)?;
            steps.push(step);
            rest = after;
        } else if rest.starts_with('[') {
            let (step, after) = bracket_step(rest, path)?;
            steps.push(step);
            rest = after;
        } else {
            return Ok((steps, rest));
        }
    }
}

/// The step after a `.`: `*` or a name.
fn dotted_step<'t>(
    text: &'t str,
    path: &str,
    name_end: impl Fn(&str) -> usize,
) -> Result<(PathStep, &'t str), PathError> {
    if let Some(after) = text.strip_prefix('*') {
        return Ok((PathStep::Wildcard, after));
    }
    let end = name_end(text);
    if end == 0 {
        return Err(PathError::Invalid(format!(
            "a name is missing after '.' in path '{path}'"
        )));
    }
    Ok((PathStep::Member(text[..end].to_string()), &text[end..]))
}

/// The step `[...]` at the start of `text`.
fn bracket_step<'t>(text: &'t str, path: &str) -> Result<(PathStep, &'t str), PathError> {
    let inner = &text[1..];
    if let Some(after) = inner.strip_prefix("?(") {
        let close = filter_end(after)
            .ok_or_else(|| PathError::Invalid(format!("unclosed filter in path '{path}'")))?;
        let expression = &after[..close];
        let filter = parse_filter(expression, path)?;
        return Ok((PathStep::Filter(Box::new(filter)), &after[close + 2..]));
    }
    if let Some(after) = inner.strip_prefix('\'') {
        let close = after
            .find('\'')
            .ok_or_else(|| PathError::Invalid(format!("unclosed quote in path '{path}'")))?;
        let key = &after[..close];
        let after = after[close + 1..].trim_start();
        return match after.strip_prefix(']') {
            Some(rest) => Ok((PathStep::Member(key.to_string()), rest)),
            None => {
                let end = text.find(']').map_or(text.len(), |at| at + 1);
                Err(unsupported("step", &text[..end], path))
            }
        };
    }
    let close = inner
        .find(']')
        .ok_or_else(|| PathError::Invalid(format!("unclosed '[' in path '{path}'")))?;
    let step = &text[..close + 2];
    let inside = inner[..close].trim();
    let rest = &inner[close + 1..];
    if inside == "*" {
        return Ok((PathStep::Wildcard, rest));
    }
    if !inside.is_empty() && inside.chars().all(|c| c.is_ascii_digit()) {
        return inside
            .parse()
            .map(|index| (PathStep::Index(index), rest))
            .map_err(|_| {
                PathError::Invalid(format!("index {inside} is too large in path '{path}'"))
            });
    }
    Err(unsupported("step", step, path))
}

/// Where the filter that starts at `text` (after `[?(`) ends: the `)` that
/// closes it, followed by `]`.
fn filter_end(text: &str) -> Option<usize> {
    let mut depth = 1usize;
    let mut quoted = false;
    for (at, c) in text.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted => {
                depth -= 1;
                if depth == 0 {
                    return text[at + 1..].starts_with(']').then_some(at);
                }
            }
            _ => {}
        }
    }
    None
}

/// Parse the expression inside `[?(...)]`: comparisons and paths joined by
/// `&&` and `||`.
fn parse_filter(expression: &str, path: &str) -> Result<PathFilter, PathError> {
    let refuse = || unsupported("filter", expression, path);
    let mut any = Vec::new();
    let mut all = Vec::new();
    let mut rest = expression;
    loop {
        rest = rest.trim_start();
        if rest.starts_with(['(', '!']) {
            return Err(refuse());
        }
        let (left, after) = parse_operand(rest, path).ok_or_else(refuse)?;
        rest = after.trim_start();
        let op = [
            ("==", CompareOp::Eq),
            ("!=", CompareOp::Ne),
            ("<=", CompareOp::Le),
            (">=", CompareOp::Ge),
            ("<", CompareOp::Lt),
            (">", CompareOp::Gt),
        ]
        .into_iter()
        .find(|(token, _)| rest.starts_with(token));
        let part = match op {
            Some((token, op)) => {
                let (right, after) =
                    parse_operand(rest[token.len()..].trim_start(), path).ok_or_else(refuse)?;
                rest = after.trim_start();
                PathFilter::Compare(left, op, right)
            }
            None if matches!(left, FilterOperand::Path { .. }) => PathFilter::Exists(left),
            None => return Err(refuse()),
        };
        all.push(part);
        if let Some(after) = rest.strip_prefix("&&") {
            rest = after;
        } else if let Some(after) = rest.strip_prefix("||") {
            any.push(joined(std::mem::take(&mut all), PathFilter::All));
            rest = after;
        } else if rest.is_empty() {
            any.push(joined(all, PathFilter::All));
            return Ok(joined(any, PathFilter::Any));
        } else {
            return Err(refuse());
        }
    }
}

fn joined(mut parts: Vec<PathFilter>, join: fn(Vec<PathFilter>) -> PathFilter) -> PathFilter {
    if parts.len() == 1 {
        parts.remove(0)
    } else {
        join(parts)
    }
}

/// One side of a filter comparison: `@.path`, `$.path`, `'text'`, a
/// number, `true`, `false` or `null`. `None` for anything else.
fn parse_operand<'t>(text: &'t str, path: &str) -> Option<(FilterOperand, &'t str)> {
    let path_from = |rest: &'t str, from_root: bool| {
        let (steps, rest) = parse_steps(rest, path, false, true).ok()?;
        Some((FilterOperand::Path { from_root, steps }, rest))
    };
    if let Some(rest) = text.strip_prefix('@') {
        return path_from(rest, false);
    }
    if let Some(rest) = text.strip_prefix('$') {
        return path_from(rest, true);
    }
    if let Some(rest) = text.strip_prefix('\'') {
        let close = rest.find('\'')?;
        let literal = Scalar::Text(rest[..close].to_string());
        return Some((FilterOperand::Literal(literal), &rest[close + 1..]));
    }
    for (word, scalar) in [
        ("true", Scalar::Bool(true)),
        ("false", Scalar::Bool(false)),
        ("null", Scalar::Null),
    ] {
        if let Some(rest) = text.strip_prefix(word) {
            return Some((FilterOperand::Literal(scalar), rest));
        }
    }
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || "+-.eE".contains(c)))
        .unwrap_or(text.len());
    let number = &text[..end];
    let value = Decimal::from_str(number)
        .or_else(|_| Decimal::from_scientific(number))
        .ok()?;
    Some((FilterOperand::Literal(Scalar::Number(value)), &text[end..]))
}

/// A filter comparison of two values. Numbers compare by value and text
/// ordinally. Values of different types are unequal and unordered, and an
/// object or array (`None`) equals nothing.
fn compare(left: &Option<Scalar>, op: CompareOp, right: &Option<Scalar>) -> bool {
    use std::cmp::Ordering;
    let order = match (left, right) {
        (Some(Scalar::Number(a)), Some(Scalar::Number(b))) => Some(a.cmp(b)),
        (Some(Scalar::Text(a)), Some(Scalar::Text(b))) => Some(a.cmp(b)),
        (Some(Scalar::Bool(a)), Some(Scalar::Bool(b))) if a == b => Some(Ordering::Equal),
        (Some(Scalar::Null), Some(Scalar::Null)) => Some(Ordering::Equal),
        _ => None,
    };
    match op {
        CompareOp::Eq => order == Some(Ordering::Equal),
        CompareOp::Ne => order != Some(Ordering::Equal),
        CompareOp::Lt => order == Some(Ordering::Less),
        CompareOp::Le => matches!(order, Some(Ordering::Less | Ordering::Equal)),
        CompareOp::Gt => order == Some(Ordering::Greater),
        CompareOp::Ge => matches!(order, Some(Ordering::Greater | Ordering::Equal)),
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

/// Node ids and handles are unique across every arena, so a variable can be
/// given its handle when declared, before any arena maps it: copies of the
/// variable then share it, as AL's reference semantics require.
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

/// The handle and node behind the JSON variable `recv`, allocating both on
/// first use and storing the handle back on the variable.
fn node_of(
    recv: &str,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<(JsonKind, usize, usize), String> {
    let json = match stack.lookup(recv) {
        Some(Value::Json(json)) => *json,
        _ => return Err(format!("'{recv}' is not a JSON variable")),
    };
    let handle = match json.handle {
        Some(handle) => handle,
        None => {
            let handle = fresh_id();
            if let Some(Value::Json(slot)) = stack.lookup_mut(recv) {
                slot.handle = Some(handle);
            }
            handle
        }
    };
    let node = ctx.json.target(handle, json.kind);
    Ok((json.kind, handle, node))
}

fn text_arg(value: Option<&Value>, what: &str) -> Result<String, String> {
    match value {
        Some(Value::Text(t) | Value::Code(t)) => Ok(t.clone()),
        Some(other) => Err(format!("{what} must be Text, got {}", other.type_name())),
        None => Err(format!("{what} is missing")),
    }
}

fn index_arg(value: Option<&Value>, len: usize, inclusive: bool) -> Result<usize, JsonError> {
    let index = match value {
        Some(Value::Integer(n)) => *n,
        _ => return Err("a JSON array index must be an Integer".to_string().into()),
    };
    let limit = if inclusive {
        len
    } else {
        len.saturating_sub(1)
    };
    usize::try_from(index)
        .ok()
        .filter(|index| *index <= limit && (inclusive || len > 0))
        .ok_or_else(|| {
            JsonError::Failed(format!(
                "index {index} is outside the JSON array of {len} elements"
            ))
        })
}

/// Why a JSON method did not complete.
enum JsonError {
    /// The operation failed: `Get` of a missing key, `Add` of a key that
    /// exists, `ReadFrom` of text that is not JSON. BC raises it when the
    /// call's Boolean result is not used and returns false when it is.
    Failed(String),
    /// Always a runtime error: a wrong argument, a conversion that does not
    /// hold, a method the runtime does not model.
    Invalid(String),
}

impl From<String> for JsonError {
    fn from(message: String) -> Self {
        JsonError::Invalid(message)
    }
}

impl From<&str> for JsonError {
    fn from(message: &str) -> Self {
        JsonError::Invalid(message.to_string())
    }
}

/// What `JsonObject.Get<type>(Key, true)` returns for a missing key.
fn default_for_getter(as_type: &str) -> Option<Value> {
    Some(match as_type {
        "text" => Value::Text(String::new()),
        "code" => Value::Code(String::new()),
        "integer" => Value::Integer(0),
        "biginteger" => Value::BigInteger(0),
        "decimal" => Value::Decimal(Decimal::ZERO),
        "boolean" => Value::Boolean(false),
        _ => return None,
    })
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
/// `Get`, the text of `WriteTo`) go to `ctx.var_writebacks`. `statement`
/// says the call is a statement, where a failed operation is a runtime
/// error. Where its result is used, the result is false.
pub(crate) fn dispatch_json_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    statement: bool,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    match run(recv, method, &args, stack, ctx) {
        Ok(value) => Eval::Normal(value),
        Err(JsonError::Failed(_)) if !statement => Eval::Normal(Value::Boolean(false)),
        Err(JsonError::Failed(error) | JsonError::Invalid(error)) => {
            eval_error(format!("{method}: {error}"))
        }
    }
}

fn run(
    recv: &str,
    method: &str,
    args: &[Value],
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Result<Value, JsonError> {
    let (kind, handle, node) = node_of(recv, stack, ctx)?;
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
            let parsed = serde_json::from_str::<serde_json::Value>(&text).map_err(|error| {
                JsonError::Failed(format!("the text is not valid JSON: {error}"))
            })?;
            let fits = match kind {
                JsonKind::Object => parsed.is_object(),
                JsonKind::Array => parsed.is_array(),
                JsonKind::Value => !parsed.is_object() && !parsed.is_array(),
                JsonKind::Token => true,
            };
            if !fits {
                return Err(JsonError::Failed(format!(
                    "the text does not hold a {}",
                    kind.name()
                )));
            }
            let imported = arena.import(&parsed)?;
            arena.targets.insert(handle, imported);
            return Ok(Value::Boolean(true));
        }
        "selecttoken" => {
            let path = text_arg(args.first(), "the path")?;
            // SelectToken fails unless exactly one token matches.
            let found = match arena.select(node, &path)?.as_slice() {
                [found] => *found,
                [] => {
                    return Err(JsonError::Failed(format!(
                        "no token matches the path '{path}'"
                    )))
                }
                several => {
                    return Err(JsonError::Failed(format!(
                        "the path '{path}' matches {} tokens",
                        several.len()
                    )))
                }
            };
            let token = arena.reference(JsonKind::Token, found);
            ctx.var_writebacks.push((1, token));
            return Ok(Value::Boolean(true));
        }
        "clone" => {
            let copy = arena.deep_copy(node);
            return Ok(arena.reference(kind, copy));
        }
        "astoken" => return Ok(arena.reference(JsonKind::Token, node)),
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
        (JsonKind::Token, "asobject", Node::Object(_)) => {
            Ok(arena.reference(JsonKind::Object, node))
        }
        (JsonKind::Token, "asarray", Node::Array(_)) => Ok(arena.reference(JsonKind::Array, node)),
        (JsonKind::Token, "asvalue", Node::Scalar(_)) => Ok(arena.reference(JsonKind::Value, node)),
        (JsonKind::Token, "asobject" | "asarray" | "asvalue", _) => {
            Err(format!("the token is not a JSON {}", &lower[2..]).into())
        }
        (JsonKind::Object, "add", Node::Object(mut entries)) => {
            let key = text_arg(args.first(), "the key")?;
            if entries.iter().any(|(name, _)| *name == key) {
                return Err(JsonError::Failed(format!("the key '{key}' already exists")));
            }
            let child = arena.child_for(args.get(1).ok_or("the value is missing")?)?;
            entries.push((key, child));
            arena.set(node, Node::Object(entries));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Object, "replace", Node::Object(mut entries)) => {
            let key = text_arg(args.first(), "the key")?;
            let Some(at) = entries.iter().position(|(name, _)| *name == key) else {
                return Err(JsonError::Failed(format!("the key '{key}' does not exist")));
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
            let (_, child) = entries
                .iter()
                .find(|(name, _)| *name == key)
                .ok_or_else(|| JsonError::Failed(format!("the key '{key}' does not exist")))?;
            let token = arena.reference(JsonKind::Token, *child);
            ctx.var_writebacks.push((1, token));
            Ok(Value::Boolean(true))
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
                .map(|(_, child)| arena.reference(JsonKind::Token, child))
                .collect(),
        )),
        (JsonKind::Object, getter, Node::Object(entries)) if getter.starts_with("get") => {
            let key = text_arg(args.first(), "the key")?;
            let found = entries
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, child)| *child);
            let default_if_not_found = match args.get(1) {
                None => false,
                Some(Value::Boolean(flag)) => *flag,
                Some(other) => {
                    return Err(format!(
                        "DefaultIfNotFound must be a Boolean, got {}",
                        other.type_name()
                    )
                    .into())
                }
            };
            let Some(child) = found else {
                return match default_for_getter(&getter[3..]) {
                    Some(default) if default_if_not_found => Ok(default),
                    _ => Err(format!("the key '{key}' does not exist").into()),
                };
            };
            match &arena.nodes[&child] {
                Node::Scalar(scalar) => Ok(scalar_as(scalar, &getter[3..])?),
                _ => Err(format!("the value of '{key}' is not a JSON value").into()),
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
            let token = arena.reference(JsonKind::Token, items[at]);
            ctx.var_writebacks.push((1, token));
            Ok(Value::Boolean(true))
        }
        (JsonKind::Array, "indexof", Node::Array(items)) => {
            let wanted = match args.first() {
                Some(Value::Json(JsonRef {
                    handle: Some(handle),
                    kind,
                })) => {
                    let node = arena.target(*handle, *kind);
                    arena.text_of(node)
                }
                Some(other) => {
                    let probe = arena.child_for(other)?;
                    arena.text_of(probe)
                }
                None => return Err("the value is missing".into()),
            };
            Ok(Value::Integer(
                items
                    .iter()
                    .position(|item| arena.text_of(*item) == wanted)
                    .map_or(-1, |at| at as i64),
            ))
        }
        (JsonKind::Array, getter, Node::Array(items)) if getter.starts_with("get") => {
            // The typed getters return the value itself, so a bad index
            // is an error wherever the call is.
            let at = index_arg(args.first(), items.len(), false).map_err(|error| match error {
                JsonError::Failed(message) => JsonError::Invalid(message),
                invalid => invalid,
            })?;
            match &arena.nodes[&items[at]] {
                Node::Scalar(scalar) => Ok(scalar_as(scalar, &getter[3..])?),
                _ => Err(format!("element {at} is not a JSON value").into()),
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
            Ok(scalar_as(&scalar, &conversion[2..])?)
        }
        (kind, _, _) => Err(format!(
            "{}.{method} is not supported by the local runtime here",
            kind.name()
        )
        .into()),
    }
}

/// What `Clear` leaves in a JSON variable of `kind`: a reference to a new
/// empty node, so copies of the old reference keep the old node.
pub(crate) fn cleared(kind: JsonKind) -> Value {
    Value::Json(JsonRef {
        kind,
        handle: Some(fresh_id()),
    })
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
        handle: Some(fresh_id()),
    }))
}

#[cfg(test)]
mod tests {
    use super::unsupported_path_step;

    #[test]
    fn paths_the_runtime_follows_and_the_steps_it_refuses() {
        for followed in [
            "",
            "$",
            "a.b",
            "$.a['b c'][0]",
            "$..c",
            "$..[0]",
            "$.a.*",
            "$.a[*]",
            "$.a[?(@.id == 'x' && @.n >= -1.5 || @.flag)]",
            "$.a[?(@.id == $.boss)]",
            "$.a[?(@ != null)]",
        ] {
            assert_eq!(unsupported_path_step(followed), None, "{followed}");
        }
        for (refused, step) in [
            ("$.a[0:2]", "[0:2]"),
            ("$.a[-1]", "[-1]"),
            ("$.a[0,1]", "[0,1]"),
            ("$['a','b']", "['a','b']"),
            ("$.a[?(@.id =~ /x/)]", "@.id =~ /x/"),
            ("$.a[?((@.n > 1))]", "(@.n > 1)"),
            ("$.a[?(!@.flag)]", "!@.flag"),
        ] {
            let message = unsupported_path_step(refused).unwrap_or_default();
            assert!(
                message.contains(&format!("'{step}'")),
                "{refused}: {message}"
            );
        }
    }
}
