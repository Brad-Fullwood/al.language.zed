//! Method dispatch for `List of [T]`, `Text`, `TextBuilder` and `Dictionary`
//! values, called from the same `Rec.Method(args)` / `Value.Method(args)`
//! dispatch path as the record methods.

use crate::interpreter::dispatch::DispatchCtx;
use crate::interpreter::eval_error;
use crate::interpreter::scope::{Eval, ScopeStack};
use crate::interpreter::value::{check_collection_len, check_text_size, Collection, Value};

/// True if `method` is a `List of [T]` method implemented by the local runtime.
pub fn supports_list_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "add"
            | "addrange"
            | "get"
            | "getrange"
            | "count"
            | "contains"
            | "indexof"
            | "lastindexof"
            | "insert"
            | "remove"
            | "removeat"
            | "removerange"
            | "reverse"
            | "set"
    )
}

/// Execute a `List of [T]` method call on the list bound to `recv`. The
/// element or old value of the `var` forms of `Get` and `Set` goes to
/// `ctx.var_writebacks`. `statement` says the call is a statement, where an
/// index out of range raises instead of returning false.
pub(crate) fn dispatch_list_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    statement: bool,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    ctx.var_writebacks.clear();
    let lower = method.to_ascii_lowercase();
    let Some(slot) = stack.lookup_mut(recv) else {
        return eval_error(format!("list variable '{recv}' is not bound"));
    };
    let Value::List(list) = slot else {
        return eval_error(format!("'{recv}' is not a List"));
    };
    let list = list.clone();
    // `AddRange(Other)` for a `List of [T]` adds Other's elements. Read them
    // before the list is locked: `L.AddRange(L)` doubles L.
    let mut args = match (lower.as_str(), args.as_slice()) {
        ("addrange", [Value::List(other)]) if adds_elements(&list, other) => other.snapshot(),
        _ => args,
    };
    if let Err(error) = declared_elements(&lower, &mut args, list.member_type()) {
        return eval_error(error);
    }
    // Comparing with the list itself would lock it twice.
    if args
        .iter()
        .any(|arg| matches!(arg, Value::List(other) if other.same(&list)))
    {
        return eval_error(format!(
            "List.{method} with the list itself as an argument is not supported by the local runtime"
        ));
    }
    if let ("contains" | "indexof" | "lastindexof" | "remove", [needle]) =
        (lower.as_str(), args.as_slice())
    {
        return search_list(&list, &lower, needle);
    }
    let mut items = list.lock();
    let added = match lower.as_str() {
        "addrange" => args.len(),
        "add" | "insert" => 1,
        _ => 0,
    };
    if let Err(error) =
        check_collection_len(&format!("List.{method}"), items.len().saturating_add(added))
    {
        return eval_error(error);
    }
    match lower.as_str() {
        "addrange" if !args.is_empty() => {
            items.extend(args);
            Eval::Normal(Value::Empty)
        }
        "addrange" => eval_error("List.AddRange expects at least one value or a List"),
        "getrange" => match list_range("List.GetRange", &args, items.len()) {
            Ok(range) => Eval::Normal(Value::List(Collection::new(
                items[range].to_vec(),
                list.member_type(),
            ))),
            Err(error) => eval_error(error),
        },
        "removerange" => match list_range("List.RemoveRange", &args, items.len()) {
            Ok(range) => {
                items.drain(range);
                Eval::Normal(Value::Boolean(true))
            }
            Err(_) if !statement => Eval::Normal(Value::Boolean(false)),
            Err(error) => eval_error(error),
        },
        "reverse" if args.is_empty() => {
            items.reverse();
            Eval::Normal(Value::Empty)
        }
        "reverse" => eval_error("List.Reverse expects no arguments"),
        "lastindexof" => eval_error("List.LastIndexOf expects exactly one value"),
        "add" if args.len() == 1 => {
            items.push(args[0].clone());
            Eval::Normal(Value::Boolean(true))
        }
        "add" => eval_error("List.Add expects exactly one value"),
        "count" if args.is_empty() => match i64::try_from(items.len()) {
            Ok(count) => Eval::Normal(Value::Integer(count)),
            Err(_) => eval_error("List.Count exceeds the supported Integer range"),
        },
        "count" => eval_error("List.Count expects no arguments"),
        "get" if args.len() == 2 => match list_index("List.Get", &args[..1], items.len()) {
            Ok(index) => {
                ctx.var_writebacks.push((1, items[index].clone()));
                Eval::Normal(Value::Boolean(true))
            }
            Err(_) if !statement => Eval::Normal(Value::Boolean(false)),
            Err(error) => eval_error(error),
        },
        "get" => match list_index("List.Get", &args, items.len()) {
            Ok(index) => Eval::Normal(items[index].clone()),
            Err(error) => eval_error(error),
        },
        "contains" => eval_error("List.Contains expects exactly one value"),
        "indexof" => eval_error("List.IndexOf expects exactly one value"),
        "removeat" => match list_index("List.RemoveAt", &args, items.len()) {
            Ok(index) => {
                items.remove(index);
                Eval::Normal(Value::Boolean(true))
            }
            Err(error) => eval_error(error),
        },
        "remove" => eval_error("List.Remove expects exactly one value"),
        "set" if args.len() == 2 => match list_index("List.Set", &args[..1], items.len()) {
            Ok(index) => {
                items[index] = args[1].clone();
                Eval::Normal(Value::Boolean(true))
            }
            Err(error) => eval_error(error),
        },
        "set" if args.len() == 3 => match list_index("List.Set", &args[..1], items.len()) {
            Ok(index) => {
                let old = std::mem::replace(&mut items[index], args[1].clone());
                ctx.var_writebacks.push((2, old));
                Eval::Normal(Value::Boolean(true))
            }
            Err(_) if !statement => Eval::Normal(Value::Boolean(false)),
            Err(error) => eval_error(error),
        },
        "set" => eval_error(
            "List.Set expects an Integer index, a value and an optional var for the old value",
        ),
        // `Insert(index, value)`: 1-based, up to one past the end.
        "insert" => match args.as_slice() {
            [Value::Integer(index), value] => {
                let position = index
                    .checked_sub(1)
                    .and_then(|index| usize::try_from(index).ok())
                    .filter(|index| *index <= items.len());
                match position {
                    Some(position) => {
                        items.insert(position, value.clone());
                        Eval::Normal(Value::Boolean(true))
                    }
                    None => eval_error(format!(
                        "List.Insert: index {index} out of range 1..{}",
                        items.len() + 1
                    )),
                }
            }
            _ => eval_error("List.Insert expects an Integer index and one value"),
        },
        other => eval_error(format!("unsupported List method: {other}")),
    }
}

/// `Contains`, `IndexOf`, `LastIndexOf` and `Remove`: find `needle` among
/// the elements of `list`.
///
/// Comparing two lists or dictionaries locks each to read it, and through a
/// cycle (`A.Add(B); B.Add(A)`) that includes `list`. A `std::sync::Mutex`
/// locked twice on one thread waits for good, so when `needle` holds a list
/// or dictionary the elements are compared as a copy, with `list` unlocked.
fn search_list(list: &Collection<Vec<Value>>, method: &str, needle: &Value) -> Eval {
    let find = |items: &[Value]| {
        if method == "lastindexof" {
            items.iter().rposition(|item| item == needle)
        } else {
            items.iter().position(|item| item == needle)
        }
    };
    let position = if holds_collection(needle) {
        find(&list.snapshot())
    } else {
        find(&list.lock())
    };
    match (method, position) {
        ("contains", position) => Eval::Normal(Value::Boolean(position.is_some())),
        ("remove", Some(position)) => {
            list.lock().remove(position);
            Eval::Normal(Value::Boolean(true))
        }
        ("remove", None) => Eval::Normal(Value::Boolean(false)),
        (_, None) => Eval::Normal(Value::Integer(0)),
        (_, Some(position)) => match i64::try_from(position + 1) {
            Ok(position) => Eval::Normal(Value::Integer(position)),
            Err(_) => eval_error("List.IndexOf result exceeds the supported Integer range"),
        },
    }
}

/// Whether comparing `value` with another value can lock a List or
/// Dictionary: whether it is one or holds one.
fn holds_collection(value: &Value) -> bool {
    match value {
        Value::List(_) | Value::Dict(_) => true,
        Value::Variant(inner) => holds_collection(inner),
        Value::Array(items) => items.iter().any(holds_collection),
        Value::Range { start, end } => holds_collection(start) || holds_collection(end),
        _ => false,
    }
}

/// Whether `list.AddRange(other)` is the `AddRange(List of [T])` overload,
/// which adds the elements of `other`, or `AddRange(T)` for a list whose
/// elements are lists, which adds `other` as one element. A list with no
/// declared element type takes the first, and an argument with none is judged
/// by its elements.
fn adds_elements(list: &Collection<Vec<Value>>, other: &Collection<Vec<Value>>) -> bool {
    let is_list_type = |type_text: &str| {
        type_text
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("list of")
    };
    let normalised = |type_text: &str| {
        type_text
            .split_whitespace()
            .collect::<String>()
            .to_ascii_lowercase()
    };
    match (list.member_type(), other.member_type()) {
        (Some(element), _) if !is_list_type(element) => true,
        (Some(element), Some(other_element)) => normalised(element) == normalised(other_element),
        (Some(_), None) => other
            .lock()
            .iter()
            .any(|item| matches!(item, Value::List(_))),
        (None, _) => true,
    }
}

/// The 0-based range `(index, count)` names in a list of `len` elements, for
/// `GetRange` and `RemoveRange`. `GetRange`'s form with a `var` result runs
/// live only.
fn list_range(method: &str, args: &[Value], len: usize) -> Result<std::ops::Range<usize>, String> {
    let (index, count) = match args {
        [Value::Integer(index), Value::Integer(count)] => (*index, *count),
        _ => return Err(format!("{method} expects an Integer index and count")),
    };
    let start = index
        .checked_sub(1)
        .and_then(|start| usize::try_from(start).ok());
    let range = start
        .zip(usize::try_from(count).ok())
        .and_then(|(start, count)| {
            let end = start.checked_add(count)?;
            (end <= len).then_some(start..end)
        });
    range.ok_or_else(|| {
        format!("{method}: index {index} and count {count} are out of range for {len} elements")
    })
}

fn list_index(method: &str, args: &[Value], len: usize) -> Result<usize, String> {
    let index = match args {
        [Value::Integer(index)] => *index,
        _ => return Err(format!("{method} expects exactly one Integer index")),
    };
    let zero_based = index
        .checked_sub(1)
        .and_then(|index| usize::try_from(index).ok())
        .filter(|index| *index < len)
        .ok_or_else(|| format!("{method}: index {index} out of range 1..{len}"))?;
    Ok(zero_based)
}

/// True if `method` is a `Text`/`Code` instance method implemented by the
/// local runtime (`s.Contains(...)`, `s.Split(...)`, …).
pub fn supports_text_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "contains"
            | "startswith"
            | "endswith"
            | "indexof"
            | "lastindexof"
            | "replace"
            | "split"
            | "trim"
            | "trimstart"
            | "trimend"
            | "tolower"
            | "toupper"
            | "substring"
            | "padleft"
            | "padright"
            | "remove"
    )
}

/// Execute a `Text`/`Code` instance method on the string bound to `recv`.
pub(crate) fn dispatch_text_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
) -> Eval {
    let s = match stack.lookup(recv) {
        Some(Value::Text(s)) | Some(Value::Code(s)) => s.clone(),
        _ => return eval_error(format!("text variable '{recv}' is not bound")),
    };
    let text_arg = |v: &Value| -> Option<String> {
        match v {
            Value::Text(t) | Value::Code(t) => Some(t.clone()),
            Value::Char(c) => Some(c.to_string()),
            _ => None,
        }
    };
    let lower = method.to_ascii_lowercase();
    match lower.as_str() {
        "contains" | "startswith" | "endswith" => match args.as_slice() {
            [needle] => match text_arg(needle) {
                Some(needle) => {
                    let result = match lower.as_str() {
                        "contains" => s.contains(&needle),
                        "startswith" => s.starts_with(&needle),
                        _ => s.ends_with(&needle),
                    };
                    Eval::Normal(Value::Boolean(result))
                }
                None => eval_error(format!(
                    "Text.{method} expects a Text argument, got {}",
                    needle.type_name()
                )),
            },
            _ => eval_error(format!("Text.{method} expects exactly one Text argument")),
        },
        "indexof" | "lastindexof" => match args.as_slice() {
            [needle] => match text_arg(needle) {
                Some(needle) if needle.is_empty() => Eval::Normal(Value::Integer(0)),
                Some(needle) => {
                    let byte_pos = if lower == "indexof" {
                        s.find(&needle)
                    } else {
                        s.rfind(&needle)
                    };
                    let result = byte_pos
                        .map(|i| s[..i].chars().count() as i64 + 1)
                        .unwrap_or(0);
                    Eval::Normal(Value::Integer(result))
                }
                None => eval_error(format!(
                    "Text.{method} expects a Text argument, got {}",
                    needle.type_name()
                )),
            },
            _ => eval_error(format!("Text.{method} expects exactly one Text argument")),
        },
        "replace" => match args.as_slice() {
            [old, new] => match (text_arg(old), text_arg(new)) {
                (Some(old), Some(new)) if !old.is_empty() => {
                    if let Err(error) = check_replace_size("Text.Replace", &s, &old, &new) {
                        return eval_error(error);
                    }
                    Eval::Normal(Value::Text(s.replace(&old, &new)))
                }
                (Some(_), Some(_)) => eval_error("Text.Replace: the old value cannot be empty"),
                _ => eval_error("Text.Replace expects (Text, Text)"),
            },
            _ => eval_error("Text.Replace expects exactly two Text arguments"),
        },
        "split" => {
            let mut separators = Vec::with_capacity(args.len());
            for arg in &args {
                match text_arg(arg) {
                    Some(sep) if !sep.is_empty() => separators.push(sep),
                    Some(_) => return eval_error("Text.Split: separators cannot be empty"),
                    None => {
                        return eval_error(format!(
                            "Text.Split expects Text separators, got {}",
                            arg.type_name()
                        ))
                    }
                }
            }
            if separators.is_empty() {
                return Eval::Normal(Value::list(vec![Value::Text(s)]));
            }
            let mut parts = vec![s];
            for sep in &separators {
                let count = parts
                    .iter()
                    .map(|part| part.matches(sep.as_str()).count() + 1)
                    .sum();
                if let Err(error) = check_collection_len("Text.Split", count) {
                    return eval_error(error);
                }
                parts = parts
                    .into_iter()
                    .flat_map(|part| {
                        part.split(sep.as_str())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .collect();
            }
            Eval::Normal(Value::list(parts.into_iter().map(Value::Text).collect()))
        }
        "trim" | "trimstart" | "trimend" => {
            if !args.is_empty() {
                return eval_error(format!("Text.{method} expects no arguments"));
            }
            let trimmed = match lower.as_str() {
                "trim" => s.trim(),
                "trimstart" => s.trim_start(),
                _ => s.trim_end(),
            };
            Eval::Normal(Value::Text(trimmed.to_string()))
        }
        "tolower" => {
            if !args.is_empty() {
                return eval_error("Text.ToLower expects no arguments");
            }
            Eval::Normal(Value::Text(s.to_lowercase()))
        }
        "toupper" => {
            if !args.is_empty() {
                return eval_error("Text.ToUpper expects no arguments");
            }
            Eval::Normal(Value::Text(s.to_uppercase()))
        }
        "substring" => {
            let (start, length) = match args.as_slice() {
                [Value::Integer(start)] => (*start, None),
                [Value::Integer(start), Value::Integer(length)] => (*start, Some(*length)),
                _ => return eval_error("Text.Substring expects (Integer[, Integer])"),
            };
            let chars: Vec<char> = s.chars().collect();
            if start < 1 || (start as usize) > chars.len() + 1 {
                return eval_error(format!(
                    "Text.Substring: start position {start} is out of range for a {}-character string",
                    chars.len()
                ));
            }
            let zero = start as usize - 1;
            match length {
                None => Eval::Normal(Value::Text(chars[zero..].iter().collect())),
                Some(length) if length < 0 => {
                    eval_error("Text.Substring: length must be >= 0".to_string())
                }
                Some(length) => {
                    let end = zero + length as usize;
                    if end > chars.len() {
                        return eval_error(format!(
                            "Text.Substring: start {start} plus length {length} exceeds the string length {}",
                            chars.len()
                        ));
                    }
                    Eval::Normal(Value::Text(chars[zero..end].iter().collect()))
                }
            }
        }
        // `PadLeft(count[, char])`: pad to `count` characters; a longer
        // text is returned unchanged.
        "padleft" | "padright" => {
            let (count, pad) = match args.as_slice() {
                [Value::Integer(count)] => (*count, ' '),
                [Value::Integer(count), pad] => match text_arg(pad).as_deref().map(str::chars) {
                    Some(mut chars) => match (chars.next(), chars.next()) {
                        (Some(pad), None) => (*count, pad),
                        _ => {
                            return eval_error(format!(
                                "Text.{method}: the pad must be one character"
                            ))
                        }
                    },
                    None => return eval_error(format!("Text.{method} expects (Integer[, Char])")),
                },
                _ => return eval_error(format!("Text.{method} expects (Integer[, Char])")),
            };
            let missing = usize::try_from(count)
                .unwrap_or(0)
                .saturating_sub(s.chars().count());
            let size = missing
                .saturating_mul(pad.len_utf8())
                .saturating_add(s.len());
            if let Err(error) = check_text_size(&format!("Text.{method}"), size) {
                return eval_error(error);
            }
            let padding: String = std::iter::repeat_n(pad, missing).collect();
            Eval::Normal(Value::Text(if lower == "padleft" {
                padding + &s
            } else {
                s + &padding
            }))
        }
        // `Remove(start[, count])`: 1-based, to the end without `count`.
        "remove" => {
            let chars: Vec<char> = s.chars().collect();
            let (start, count) = match args.as_slice() {
                [Value::Integer(start)] => (*start, None),
                [Value::Integer(start), Value::Integer(count)] => (*start, Some(*count)),
                _ => return eval_error("Text.Remove expects (Integer[, Integer])"),
            };
            if start < 1 || start as usize > chars.len() {
                return eval_error(format!(
                    "Text.Remove: start position {start} is out of range for a {}-character string",
                    chars.len()
                ));
            }
            let zero = start as usize - 1;
            let end = match count {
                None => chars.len(),
                Some(count) if count >= 0 && zero + count as usize <= chars.len() => {
                    zero + count as usize
                }
                Some(count) => {
                    return eval_error(format!(
                        "Text.Remove: {count} characters from position {start} exceed the string length {}",
                        chars.len()
                    ))
                }
            };
            Eval::Normal(Value::Text(
                chars[..zero].iter().chain(&chars[end..]).collect(),
            ))
        }
        other => eval_error(format!("unsupported Text method: {other}")),
    }
}

/// Refuse a replacement of `old` by `new` in `text` whose result is longer
/// than [`crate::interpreter::value::MAX_TEXT_BYTES`]. Replacing `x` with
/// `xx` doubles a text.
fn check_replace_size(operation: &str, text: &str, old: &str, new: &str) -> Result<(), String> {
    if new.len() <= old.len() {
        return Ok(());
    }
    let most = (text.len() / old.len()).saturating_mul(new.len() - old.len());
    if check_text_size(operation, text.len().saturating_add(most)).is_ok() {
        return Ok(());
    }
    let growth = text
        .matches(old)
        .count()
        .saturating_mul(new.len() - old.len());
    check_text_size(operation, text.len().saturating_add(growth))
}

/// True if `method` is a `TextBuilder` method implemented by the local
/// runtime.
pub fn supports_textbuilder_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "append" | "appendline" | "length" | "totext" | "clear" | "insert" | "remove" | "replace"
    )
}

/// Execute a `TextBuilder` method on the builder bound to `recv`, changing
/// it in place.
pub(crate) fn dispatch_textbuilder_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
) -> Eval {
    let Some(Value::TextBuilder(builder)) = stack.lookup(recv) else {
        return eval_error(format!("'{recv}' is not a TextBuilder"));
    };
    let builder = builder.clone();
    // A builder argument is read before the receiver is locked, since it may
    // be the receiver.
    let args: Vec<Value> = args
        .into_iter()
        .map(|arg| match arg {
            Value::TextBuilder(other) => Value::Text(other.snapshot()),
            other => other,
        })
        .collect();
    let mut guard = builder.lock();
    let text: &mut String = &mut guard;
    let as_text = |value: &Value| match value {
        Value::Text(t) | Value::Code(t) => t.clone(),
        Value::Char(c) => c.to_string(),
        other => crate::interpreter::dispatch::render_value(other),
    };
    // Positions are 1-based character indexes, as in BC.
    let byte_at = |text: &str, index: i64| -> Option<usize> {
        let zero = usize::try_from(index.checked_sub(1)?).ok()?;
        if zero == text.chars().count() {
            return Some(text.len());
        }
        text.char_indices().nth(zero).map(|(at, _)| at)
    };
    let lower = method.to_ascii_lowercase();
    let grow_by = |text: &str, added: usize| {
        check_text_size(
            &format!("TextBuilder.{method}"),
            text.len().saturating_add(added),
        )
    };
    match (lower.as_str(), args.as_slice()) {
        ("append", [value]) => {
            let value = as_text(value);
            if let Err(error) = grow_by(text, value.len()) {
                return eval_error(error);
            }
            text.push_str(&value);
            Eval::Normal(Value::Boolean(true))
        }
        ("appendline", []) => {
            if let Err(error) = grow_by(text, 2) {
                return eval_error(error);
            }
            text.push_str("\r\n");
            Eval::Normal(Value::Boolean(true))
        }
        ("appendline", [value]) => {
            let value = as_text(value);
            if let Err(error) = grow_by(text, value.len() + 2) {
                return eval_error(error);
            }
            text.push_str(&value);
            text.push_str("\r\n");
            Eval::Normal(Value::Boolean(true))
        }
        ("length", []) => Eval::Normal(Value::Integer(text.chars().count() as i64)),
        ("totext", []) => Eval::Normal(Value::Text(text.clone())),
        ("totext", [Value::Integer(start), Value::Integer(count)]) => {
            let chars: Vec<char> = text.chars().collect();
            let from = usize::try_from(start - 1)
                .ok()
                .filter(|from| *from <= chars.len());
            let to = from
                .zip(usize::try_from(*count).ok())
                .map(|(from, count)| from + count)
                .filter(|to| *to <= chars.len());
            match from.zip(to) {
                Some((from, to)) => Eval::Normal(Value::Text(chars[from..to].iter().collect())),
                None => eval_error(format!(
                    "TextBuilder.ToText: {count} characters from {start} are outside the {}-character text",
                    chars.len()
                )),
            }
        }
        ("clear", []) => {
            text.clear();
            Eval::Normal(Value::Empty)
        }
        ("insert", [Value::Integer(index), value]) => match byte_at(text, *index) {
            Some(at) => {
                let value = as_text(value);
                if let Err(error) = grow_by(text, value.len()) {
                    return eval_error(error);
                }
                text.insert_str(at, &value);
                Eval::Normal(Value::Boolean(true))
            }
            None => eval_error(format!("TextBuilder.Insert: index {index} is out of range")),
        },
        ("remove", [Value::Integer(index), Value::Integer(count)]) => {
            let start = byte_at(text, *index);
            let end = start.and_then(|_| byte_at(text, index + count));
            match start.zip(end).filter(|_| *count >= 0) {
                Some((start, end)) => {
                    text.replace_range(start..end, "");
                    Eval::Normal(Value::Boolean(true))
                }
                None => eval_error(format!(
                    "TextBuilder.Remove: {count} characters from {index} are out of range"
                )),
            }
        }
        ("replace", [old, new]) => {
            let (old, new) = (as_text(old), as_text(new));
            if old.is_empty() {
                return eval_error("TextBuilder.Replace: the old value cannot be empty");
            }
            if let Err(error) = check_replace_size("TextBuilder.Replace", text, &old, &new) {
                return eval_error(error);
            }
            *text = text.replace(&old, &new);
            Eval::Normal(Value::Boolean(true))
        }
        _ => eval_error(format!(
            "TextBuilder.{method} with these arguments is not supported by the local runtime"
        )),
    }
}

/// True if `method` is a `Dictionary of [K, V]` method implemented by the
/// local runtime.
pub fn supports_dict_method(method: &str) -> bool {
    matches!(
        method.to_ascii_lowercase().as_str(),
        "add" | "get" | "set" | "containskey" | "remove" | "count" | "keys" | "values"
    )
}

/// The value `recv` holds under `key`, for the two-argument
/// `Dictionary.Get(key, var value)`, which writes it back to its caller.
pub(crate) fn dict_lookup(
    recv: &str,
    key: &Value,
    stack: &ScopeStack,
) -> Result<Option<Value>, String> {
    let Some(Value::Dict(dict)) = stack.lookup(recv) else {
        return Err(format!("'{recv}' is not a Dictionary"));
    };
    let key = declared_member(key, dict.member_type())?;
    Ok(dict
        .lock()
        .get(&dict_key(&key)?)
        .map(|(_, value)| value.clone()))
}

/// `value` converted to `member_type`, the declared element type of a List
/// or key type of a Dictionary, as BC converts an argument to a typed
/// parameter: a Code value is trimmed and upper-cased, one character of Text
/// becomes a Char, a Char becomes Text, Code or its Integer code, and an
/// Integer becomes a Decimal.
fn declared_member(value: &Value, member_type: Option<&str>) -> Result<Value, String> {
    let Some(member_type) = member_type else {
        return Ok(value.clone());
    };
    let base = member_type.split('[').next().unwrap_or_default().trim();
    let Some(slot) = Value::default_for(base) else {
        return Ok(value.clone());
    };
    let capacity = crate::interpreter::dispatch::declared_text_length(member_type);
    match (&slot, value) {
        (Value::Char(_), Value::Text(text) | Value::Code(text)) => {
            crate::interpreter::value::check_string_capacity(text, Some(1))?;
            Ok(Value::Char(text.chars().next().unwrap_or('\0')))
        }
        (Value::Text(_) | Value::Code(_), Value::Char(c)) => {
            Value::coerce_into_slot(&slot, Value::Text(c.to_string()), capacity)
        }
        (Value::Integer(_) | Value::BigInteger(_), Value::Char(c)) => {
            Value::coerce_into_slot(&slot, Value::Integer(i64::from(u32::from(*c))), None)
        }
        _ => Value::coerce_into_slot(&slot, value.clone(), capacity),
    }
}

/// Convert the element arguments of the List method `method` to the list's
/// declared element type: the value `Add`, `Insert` and `Set` store, every
/// value `AddRange` stores, and the value `Contains`, `IndexOf`,
/// `LastIndexOf` and `Remove` look for.
fn declared_elements(
    method: &str,
    args: &mut [Value],
    element_type: Option<&str>,
) -> Result<(), String> {
    let elements = match method {
        "add" | "contains" | "indexof" | "lastindexof" | "remove" => args.get_mut(..1),
        "insert" | "set" => args.get_mut(1..2),
        "addrange" => Some(args),
        _ => None,
    };
    for element in elements.unwrap_or_default() {
        *element = declared_member(element, element_type)?;
    }
    Ok(())
}

/// Serialise a dictionary key value into the `Dict` map's string key space.
fn dict_key(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::Text(s) => s.clone(),
        // Code keys are caseless.
        Value::Code(s) => s.to_uppercase(),
        Value::Integer(n) | Value::BigInteger(n) => n.to_string(),
        Value::Decimal(d) => d.normalize().to_string(),
        Value::Boolean(b) => b.to_string(),
        Value::Date(d) => format!("D{d}"),
        Value::Time(t) => format!("T{t}"),
        Value::DateTime(dt) => format!("DT{dt}"),
        Value::Guid(g) => g.to_uppercase(),
        Value::Char(c) => format!("C{c}"),
        Value::Option {
            type_name, ordinal, ..
        } => format!("O{}:{ordinal}", type_name.to_ascii_lowercase()),
        other => {
            return Err(format!(
                "Dictionary keys of type {} are not supported by the local runtime",
                other.type_name()
            ))
        }
    })
}

/// Execute a `Dictionary of [K, V]` method call on the dictionary bound to
/// `recv`. The old value of `Set(key, value, var old)` goes to
/// `ctx.var_writebacks`.
pub(crate) fn dispatch_dict_method(
    recv: &str,
    method: &str,
    args: Vec<Value>,
    stack: &mut ScopeStack,
    ctx: &mut DispatchCtx,
) -> Eval {
    ctx.var_writebacks.clear();
    let lower = method.to_ascii_lowercase();
    let Some(slot) = stack.lookup_mut(recv) else {
        return eval_error(format!("dictionary variable '{recv}' is not bound"));
    };
    let Value::Dict(dict) = slot else {
        return eval_error(format!("'{recv}' is not a Dictionary"));
    };
    let dict = dict.clone();
    let mut args = args;
    if matches!(
        lower.as_str(),
        "add" | "set" | "get" | "containskey" | "remove"
    ) {
        if let Some(key) = args.first_mut() {
            match declared_member(key, dict.member_type()) {
                Ok(converted) => *key = converted,
                Err(error) => return eval_error(error),
            }
        }
    }
    let mut entries = dict.lock();
    if matches!(lower.as_str(), "add" | "set") {
        if let Err(error) = check_collection_len(
            &format!("Dictionary.{method}"),
            entries.len().saturating_add(1),
        ) {
            // Set on a key that is there replaces a value.
            let replaces = lower == "set"
                && args
                    .first()
                    .and_then(|key| dict_key(key).ok())
                    .is_some_and(|key| entries.contains_key(&key));
            if !replaces {
                return eval_error(error);
            }
        }
    }
    match lower.as_str() {
        "add" => match args.as_slice() {
            [key, value] => match dict_key(key) {
                Ok(key_text) => match entries.entry(key_text) {
                    indexmap::map::Entry::Occupied(_) => {
                        eval_error("Dictionary.Add: the key already exists")
                    }
                    indexmap::map::Entry::Vacant(slot) => {
                        slot.insert((key.clone(), value.clone()));
                        Eval::Normal(Value::Empty)
                    }
                },
                Err(error) => eval_error(error),
            },
            _ => eval_error("Dictionary.Add expects exactly a key and a value"),
        },
        "set" => match args.as_slice() {
            [key, value] => match dict_key(key) {
                Ok(key_text) => {
                    entries.insert(key_text, (key.clone(), value.clone()));
                    Eval::Normal(Value::Empty)
                }
                Err(error) => eval_error(error),
            },
            // True and the old value when the key was there, false when the
            // value was added.
            [key, value, _] => match dict_key(key) {
                Ok(key_text) => match entries.insert(key_text, (key.clone(), value.clone())) {
                    Some((_, old)) => {
                        ctx.var_writebacks.push((2, old));
                        Eval::Normal(Value::Boolean(true))
                    }
                    None => Eval::Normal(Value::Boolean(false)),
                },
                Err(error) => eval_error(error),
            },
            _ => eval_error(
                "Dictionary.Set expects a key, a value and an optional var for the old value",
            ),
        },
        "get" => match args.as_slice() {
            [key] => match dict_key(key) {
                Ok(key_text) => match entries.get(&key_text) {
                    Some((_, value)) => Eval::Normal(value.clone()),
                    None => eval_error("Dictionary.Get: the key does not exist"),
                },
                Err(error) => eval_error(error),
            },
            _ => eval_error(
                "Dictionary.Get with a var out-parameter requires live BC; \
                 only the one-argument returning form runs locally",
            ),
        },
        "containskey" => match args.as_slice() {
            [key] => match dict_key(key) {
                Ok(key_text) => Eval::Normal(Value::Boolean(entries.contains_key(&key_text))),
                Err(error) => eval_error(error),
            },
            _ => eval_error("Dictionary.ContainsKey expects exactly one key"),
        },
        "remove" => match args.as_slice() {
            [key] => match dict_key(key) {
                Ok(key_text) => {
                    Eval::Normal(Value::Boolean(entries.shift_remove(&key_text).is_some()))
                }
                Err(error) => eval_error(error),
            },
            _ => eval_error("Dictionary.Remove expects exactly one key"),
        },
        "count" => {
            if !args.is_empty() {
                return eval_error("Dictionary.Count expects no arguments");
            }
            match i64::try_from(entries.len()) {
                Ok(count) => Eval::Normal(Value::Integer(count)),
                Err(_) => eval_error("Dictionary.Count exceeds the supported Integer range"),
            }
        }
        // The lists carry the dictionary's declared types, so a typed
        // variable that takes one keeps its overloads and conversions.
        "keys" => {
            if !args.is_empty() {
                return eval_error("Dictionary.Keys expects no arguments");
            }
            Eval::Normal(Value::List(Collection::new(
                entries.values().map(|(key, _)| key.clone()).collect(),
                dict.member_type(),
            )))
        }
        "values" => {
            if !args.is_empty() {
                return eval_error("Dictionary.Values expects no arguments");
            }
            Eval::Normal(Value::List(Collection::new(
                entries.values().map(|(_, value)| value.clone()).collect(),
                dict.value_type(),
            )))
        }
        other => eval_error(format!("unsupported Dictionary method: {other}")),
    }
}
