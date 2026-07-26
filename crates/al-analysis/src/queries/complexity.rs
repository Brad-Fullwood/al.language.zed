//! Fail-closed whole-workspace complexity reporting.

use al_workspace::Workspace;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplexityProcedure {
    pub name: String,
    pub line: u32,
    pub nesting_depth: u32,
    pub cyclomatic: u32,
    pub cognitive: u32,
}

impl From<&al_syntax::complexity::ProcedureComplexity> for ComplexityProcedure {
    fn from(value: &al_syntax::complexity::ProcedureComplexity) -> Self {
        Self {
            name: value.name.clone(),
            line: value.line,
            nesting_depth: value.nesting_depth,
            cyclomatic: value.cyclomatic,
            cognitive: value.cognitive,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileComplexity {
    pub file: String,
    pub procedures: Vec<ComplexityProcedure>,
    pub hotspots: Vec<ComplexityProcedure>,
}

pub fn workspace_complexity(
    workspace: &Workspace,
    threshold_cyclomatic: u32,
    threshold_cognitive: u32,
) -> Result<Vec<FileComplexity>, super::WorkspaceQueryError> {
    let sources =
        crate::workspace_sources::snapshot(workspace).map_err(super::WorkspaceQueryError::from)?;
    Ok(sources
        .into_iter()
        .filter_map(|source| {
            let procedures = al_syntax::complexity::compute_complexity(&source.tree, &source.text)
                .iter()
                .map(ComplexityProcedure::from)
                .collect::<Vec<_>>();
            if procedures.is_empty() {
                return None;
            }
            let hotspots = procedures
                .iter()
                .filter(|procedure| {
                    procedure.cyclomatic >= threshold_cyclomatic
                        || procedure.cognitive >= threshold_cognitive
                })
                .cloned()
                .collect();
            Some(FileComplexity {
                file: source.path.to_string_lossy().into_owned(),
                procedures,
                hotspots,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn workspace_complexity_rejects_malformed_or_incomplete_sources() {
        let workspace = Workspace::new();
        workspace.file_index.add_file(
            PathBuf::from("/project/Broken.al"),
            "codeunit 50100 Broken { procedure Incomplete(".to_string(),
        );
        assert!(workspace_complexity(&workspace, 10, 15).is_err());

        let workspace = Workspace::new();
        workspace.file_index.files.insert(
            PathBuf::from("/project/MissingCache.al"),
            "codeunit 50100 MissingCache { }".to_string(),
        );
        assert!(workspace_complexity(&workspace, 10, 15).is_err());
    }
}
