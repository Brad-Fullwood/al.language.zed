//! scopes, variables and evaluate.
//!
//! BC returns globals either inline or behind one expandable wrapper node, and
//! locals behind a frame wrapper. The node helpers here strip those wrappers
//! so the client sees the variables themselves.

use std::path::{Path, PathBuf};

use crate::dap::Result;

use super::{
    decode_scope_reference, json_array, make_response, scope_reference, write_dap, NativeDapState,
    ResolvedObject, VariableHandle, VariableHandleStore, SCOPE_GLOBALS, SCOPE_LOCALS,
    VARIABLE_HANDLE_BASE,
};

impl<F, Fut, R, P, C, CompileFut, A> NativeDapState<F, R, P, C, A>
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    R: Fn(&str) -> Option<ResolvedObject> + Send + Sync + 'static,
    P: Fn(i32, i32) -> Option<PathBuf> + Send + Sync + 'static,
    C: Fn(PathBuf) -> CompileFut + Send + Sync + 'static,
    CompileFut: std::future::Future<Output = std::result::Result<String, String>> + Send,
    A: Fn(&Path) -> std::result::Result<Option<PathBuf>, String> + Send + Sync + 'static,
{
    pub(super) async fn handle_scopes<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let frame_id = arguments
            .get("frameId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let mut scopes = Vec::new();
        if self.session.lock().await.is_some() {
            match (
                scope_reference(SCOPE_GLOBALS, frame_id),
                scope_reference(SCOPE_LOCALS, frame_id),
            ) {
                (Some(globals_ref), Some(locals_ref)) => {
                    scopes.push(serde_json::json!({
                        "name": "Globals",
                        "variablesReference": globals_ref,
                        "expensive": true,
                    }));
                    scopes.push(serde_json::json!({
                        "name": "Locals",
                        "variablesReference": locals_ref,
                        "expensive": false,
                    }));
                }
                _ => {
                    self.reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("stack frame {frame_id} is outside the supported DAP range"),
                    )
                    .await?;
                    return Ok(());
                }
            }
        }
        self.reply_body(
            out,
            request_seq,
            command,
            serde_json::json!({ "scopes": scopes }),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn handle_variables<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let vars_ref = arguments
            .get("variablesReference")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let session_arc = self.session.lock().await.clone();
        let Some(session) = session_arc else {
            self.reply_body(
                out,
                request_seq,
                command,
                serde_json::json!({"variables": []}),
            )
            .await?;
            return Ok(());
        };

        let resolved = if vars_ref >= VARIABLE_HANDLE_BASE {
            let handle = self.variable_handles.lock().await.get(vars_ref);
            if let Some(handle) = handle {
                let nodes = match handle.nodes {
                    Some(nodes) => Ok(nodes),
                    None => session
                        .expand_node(handle.frame_id, &handle.parent_path)
                        .await
                        .map(json_array),
                };
                nodes.map(|nodes| (handle.frame_id, handle.parent_path, nodes))
            } else {
                Ok((0, String::new(), Vec::new()))
            }
        } else if let Some((group, frame_id)) = decode_scope_reference(vars_ref) {
            match session.get_variables(frame_id).await {
                Ok(root_nodes) if group == SCOPE_LOCALS => Ok((
                    frame_id,
                    String::new(),
                    local_nodes_from_frame_variables(&root_nodes),
                )),
                Ok(root_nodes) => match inline_global_nodes(&root_nodes) {
                    Some(nodes) => Ok((frame_id, String::new(), nodes)),
                    None => session.get_globals(frame_id).await.map(|expanded| {
                        let (parent_path, nodes) = expanded_global_nodes(&expanded);
                        (frame_id, parent_path, nodes)
                    }),
                },
                Err(error) => Err(error),
            }
        } else {
            Ok((0, String::new(), Vec::new()))
        };

        let (frame_id, parent_path, nodes) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                self.reply_failure(
                    out,
                    request_seq,
                    command,
                    format!("Business Central variable request failed: {error}"),
                )
                .await?;
                return Ok(());
            }
        };
        let variables = {
            let mut handles = self.variable_handles.lock().await;
            bc_vars_to_dap(&nodes, frame_id, &parent_path, &mut handles)
        };
        self.reply_body(
            out,
            request_seq,
            command,
            serde_json::json!({"variables": variables}),
        )
        .await?;
        Ok(())
    }

    pub(super) async fn handle_evaluate<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        out: &mut W,
        request_seq: i64,
        command: &str,
        arguments: &serde_json::Value,
    ) -> Result<()> {
        let expression = arguments
            .get("expression")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let frame_id = arguments
            .get("frameId")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        // Clone Arc and drop guard before async work.
        let session_arc = self.session.lock().await.clone();
        let result = match session_arc {
            Some(session) => match session.evaluate(frame_id, expression).await {
                Ok(result) => result,
                Err(error) => {
                    self.reply_failure(
                        out,
                        request_seq,
                        command,
                        format!("Business Central evaluation failed: {error}"),
                    )
                    .await?;
                    return Ok(());
                }
            },
            None => serde_json::Value::Null,
        };
        let display = bc_node_display_value(&result);
        let type_name = result
            .get("TypeName")
            .or_else(|| result.get("typeName"))
            .or_else(|| result.get("Type"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let children = bc_node_children(&result);
        let has_children = bc_node_has_children(&result)
            || children.as_ref().is_some_and(|value| !value.is_empty());
        let (variables_reference, named_variables) = if has_children && !expression.is_empty() {
            let named_variables = children.as_ref().map(Vec::len);
            let reference = self
                .variable_handles
                .lock()
                .await
                .create(VariableHandle {
                    frame_id,
                    parent_path: expression.to_string(),
                    nodes: children,
                })
                .unwrap_or(0);
            (reference, named_variables)
        } else {
            (0, None)
        };
        let mut body = serde_json::json!({
            "result": display,
            "type": type_name,
            "variablesReference": variables_reference,
        });
        if let Some(count) = named_variables {
            body["namedVariables"] = serde_json::json!(count);
        }
        write_dap(
            out,
            &make_response(&self.seq, request_seq, command, true, Some(body), None),
        )
        .await?;
        Ok(())
    }
}

fn bc_node_name(node: &serde_json::Value) -> Option<&str> {
    node.get("Name")
        .or_else(|| node.get("name"))
        .and_then(serde_json::Value::as_str)
}

fn bc_node_is(node: &serde_json::Value, expected: &str) -> bool {
    bc_node_name(node).is_some_and(|name| name.eq_ignore_ascii_case(expected))
}

fn bc_node_children(node: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    node.get("Children")
        .or_else(|| node.get("children"))
        .and_then(serde_json::Value::as_array)
        .cloned()
}

fn bc_node_has_children(node: &serde_json::Value) -> bool {
    node.get("HasChildren")
        .or_else(|| node.get("hasChildren"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || bc_node_children(node).is_some_and(|children| !children.is_empty())
}

fn local_nodes_from_frame_variables(variables: &serde_json::Value) -> Vec<serde_json::Value> {
    let Some(nodes) = variables.as_array() else {
        return Vec::new();
    };
    let mut start = usize::from(
        nodes
            .first()
            .is_some_and(|node| bc_node_is(node, "<Globals>")),
    );
    if nodes
        .get(start)
        .is_some_and(|node| bc_node_is(node, "<Database Statistics>"))
    {
        start += 1;
    }
    nodes[start..].to_vec()
}

/// Return globals already embedded in the `GetVariables` root node. `None`
/// means BC advertised children but requires the separate `ExpandGlobals` RPC.
fn inline_global_nodes(variables: &serde_json::Value) -> Option<Vec<serde_json::Value>> {
    let globals = variables.as_array()?.first()?;
    if !bc_node_is(globals, "<Globals>") {
        return None;
    }
    if let Some(children) = bc_node_children(globals) {
        return Some(children);
    }
    (!bc_node_has_children(globals)).then(Vec::new)
}

fn expanded_global_nodes(variables: &serde_json::Value) -> (String, Vec<serde_json::Value>) {
    let nodes = variables.as_array().cloned().unwrap_or_default();
    let Some(first) = nodes.first() else {
        return (String::new(), Vec::new());
    };
    if bc_node_is(first, "<Globals>") {
        let mut flattened = bc_node_children(first).unwrap_or_default();
        flattened.extend(nodes.into_iter().skip(1));
        return (String::new(), flattened);
    }

    // This is the legacy server shape handled by Microsoft's adapter: the
    // first expanded node names the parent whose children follow.
    (
        bc_node_name(first)
            .map(quote_al_identifier)
            .unwrap_or_default(),
        nodes,
    )
}

fn quote_al_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn bc_node_display_value(node: &serde_json::Value) -> String {
    let value = node
        .get("Summary")
        .or_else(|| node.get("summary"))
        .or_else(|| node.get("Value"))
        .or_else(|| node.get("value"));
    match value {
        Some(serde_json::Value::String(value)) => value
            .strip_prefix("\r\n")
            .or_else(|| value.strip_prefix('\n'))
            .unwrap_or(value)
            .to_string(),
        Some(serde_json::Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Convert BC `LocalNode` values into DAP variables while retaining structured
/// child nodes. BC casing varies by server version, so every wire field accepts
/// both its Newtonsoft PascalCase form and camelCase variants.
fn bc_vars_to_dap(
    nodes: &[serde_json::Value],
    frame_id: i64,
    parent_path: &str,
    handles: &mut VariableHandleStore,
) -> Vec<serde_json::Value> {
    nodes
        .iter()
        .filter_map(|node| {
            let name = bc_node_name(node)?;
            let value = bc_node_display_value(node);
            let type_name = node
                .get("TypeName")
                .or_else(|| node.get("typeName"))
                .or_else(|| node.get("Type"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let quoted_name = quote_al_identifier(name);
            let path = if parent_path.is_empty() {
                quoted_name
            } else {
                format!("{parent_path}.{quoted_name}")
            };
            let children = bc_node_children(node);
            let has_children = bc_node_has_children(node)
                || children.as_ref().is_some_and(|value| !value.is_empty());
            let (variables_reference, named_variables) = if has_children {
                let named_variables = children.as_ref().map(Vec::len);
                let reference = handles
                    .create(VariableHandle {
                        frame_id,
                        parent_path: path.clone(),
                        nodes: children,
                    })
                    .unwrap_or(0);
                (reference, named_variables)
            } else {
                (0, None)
            };
            let mut variable = serde_json::json!({
                "name": name,
                "value": value,
                "type": type_name,
                "evaluateName": path,
                "variablesReference": variables_reference,
            });
            if let Some(count) = named_variables {
                variable["namedVariables"] = serde_json::json!(count);
            }
            Some(variable)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::test_support::*;

    #[test]
    fn bc_vars_to_dap_maps_pascal_and_camel_case_to_dap_shape() {
        // BC's PascalCase and camelCase LocalNode keys must both produce DAP
        // `name`/`value`/`type` + `variablesReference`. A non-string value is
        // stringified, not dropped. A node without a name is skipped.
        let bc = serde_json::json!([
            { "Name": "Customer", "Value": "10000", "TypeName": "Record" },
            { "name": "i", "value": 5, "typeName": "Integer" },
            { "Value": "orphan" }
        ]);
        let mut handles = VariableHandleStore::default();
        let vars = bc_vars_to_dap(bc.as_array().expect("fixture array"), 0, "", &mut handles);
        assert_eq!(vars.len(), 2, "nameless node must be skipped");
        assert_eq!(vars[0]["name"], "Customer");
        assert_eq!(vars[0]["value"], "10000");
        assert_eq!(vars[0]["type"], "Record");
        assert_eq!(vars[0]["evaluateName"], "\"Customer\"");
        assert_eq!(vars[0]["variablesReference"], 0);
        assert_eq!(vars[1]["name"], "i");
        assert_eq!(vars[1]["value"], "5", "numeric value stringified");
        assert_eq!(vars[1]["type"], "Integer");
    }

    #[test]
    fn bc_vars_to_dap_retains_inline_and_lazy_child_nodes() {
        let bc = serde_json::json!([
            {
                "Name": "Customer",
                "Summary": "Record Customer",
                "TypeName": "Record Customer",
                "HasChildren": true,
                "Children": [{
                    "Name": "No.",
                    "Summary": "10000",
                    "TypeName": "Code[20]"
                }]
            },
            {
                "name": "Lines",
                "summary": "List of [Record Sales Line]",
                "typeName": "List",
                "hasChildren": true,
                "children": null
            }
        ]);
        let mut handles = VariableHandleStore::default();
        let vars = bc_vars_to_dap(bc.as_array().expect("fixture array"), 7, "", &mut handles);

        assert_eq!(vars[0]["variablesReference"], VARIABLE_HANDLE_BASE);
        assert_eq!(vars[0]["namedVariables"], 1);
        assert_eq!(vars[0]["evaluateName"], "\"Customer\"");
        let customer = handles.get(VARIABLE_HANDLE_BASE).expect("customer handle");
        assert_eq!(customer.frame_id, 7);
        assert_eq!(customer.parent_path, "\"Customer\"");
        assert_eq!(customer.nodes.expect("inline children").len(), 1);

        assert_eq!(vars[1]["variablesReference"], VARIABLE_HANDLE_BASE + 1);
        assert!(vars[1].get("namedVariables").is_none());
        let lines = handles
            .get(VARIABLE_HANDLE_BASE + 1)
            .expect("lazy list handle");
        assert_eq!(lines.parent_path, "\"Lines\"");
        assert!(lines.nodes.is_none(), "null children must expand lazily");
    }

    #[test]
    fn frame_variable_groups_strip_wrappers_without_losing_locals() {
        let roots = serde_json::json!([
            { "Name": "<Globals>", "HasChildren": true, "Children": null },
            { "Name": "<Database Statistics>", "HasChildren": true },
            { "Name": "Customer", "Summary": "10000" },
            { "Name": "Count", "Summary": "2" }
        ]);
        let locals = local_nodes_from_frame_variables(&roots);
        assert_eq!(locals.len(), 2);
        assert_eq!(bc_node_name(&locals[0]), Some("Customer"));
        assert!(
            inline_global_nodes(&roots).is_none(),
            "null advertised children require ExpandGlobals"
        );

        let inline = serde_json::json!([{
            "Name": "<Globals>",
            "HasChildren": true,
            "Children": [{ "Name": "GlobalValue", "Summary": "42" }]
        }]);
        assert_eq!(
            inline_global_nodes(&inline).expect("inline globals").len(),
            1
        );
    }

    #[tokio::test]
    async fn variables_without_session_returns_empty_array() {
        let (_, frames) =
            run_request("variables", serde_json::json!({"variablesReference": 101})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["variables"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn scopes_without_session_returns_no_scopes() {
        let (_, frames) = run_request("scopes", serde_json::json!({"frameId": 3})).await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["scopes"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn evaluate_without_session_returns_empty_result() {
        let (_, frames) = run_request(
            "evaluate",
            serde_json::json!({"expression": "Customer.Name", "frameId": 0}),
        )
        .await;
        assert_eq!(frames[0]["success"], true);
        assert_eq!(frames[0]["body"]["result"], "");
        assert_eq!(frames[0]["body"]["variablesReference"], 0);
    }
}
