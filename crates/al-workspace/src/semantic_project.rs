//! The project context a request to the semantic bridge carries, so the
//! bridge answers from the project compilation: the project's root and the
//! editor buffers of its other files.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use al_semantic::{OpenDocument, ProjectContext};

use crate::Workspace;

/// The project context for a request about `file`, or `None` when the file
/// is outside the loaded project (a rendered symbol file, for one), which the
/// bridge then compiles alone.
pub async fn semantic_project_context(workspace: &Workspace, file: &Path) -> Option<ProjectContext> {
    let root = workspace
        .project
        .read()
        .await
        .as_ref()
        .map(|project| project.root.clone())
        .filter(|root| file.starts_with(root))?;
    let documents = &workspace.documents;
    let open = documents
        .open_uris()
        .into_iter()
        .filter(|uri| documents.get_client_version(uri).is_some())
        .filter_map(|uri| Some((uri.to_file_path().ok()?, documents.get_text_arc(&uri)?)));
    let open_documents = project_open_documents(open, &root, file);
    Some(ProjectContext {
        root,
        open_documents,
    })
}

/// Editor buffers of the project's other `.al` files. The bridge compiles
/// their unsaved text in place of the files on disk.
pub fn project_open_documents(
    open: impl IntoIterator<Item = (PathBuf, Arc<String>)>,
    root: &Path,
    target: &Path,
) -> Vec<OpenDocument> {
    open.into_iter()
        .filter(|(path, _)| path != target && path.starts_with(root))
        .filter(|(path, _)| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("al"))
        })
        .map(|(file, text)| OpenDocument {
            file,
            source: text.as_str().to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The semantic pass compiles the analyzed file with its project, so the
    /// other open `.al` buffers of that project travel with the request. The
    /// analyzed file itself goes as the request's own source, and buffers of
    /// other folders or other file kinds are not part of the compilation.
    #[test]
    fn only_other_al_buffers_of_the_project_are_sent_with_a_semantic_request() {
        let root = PathBuf::from("/work/app");
        let target = root.join("src/Target.Codeunit.al");
        let open = vec![
            (target.clone(), Arc::new("target".to_string())),
            (root.join("src/Sibling.Table.al"), Arc::new("sibling".to_string())),
            (root.join("src/Upper.Page.AL"), Arc::new("upper".to_string())),
            (root.join("app.json"), Arc::new("{}".to_string())),
            (PathBuf::from("/work/other/Other.al"), Arc::new("other".to_string())),
        ];

        let sent = project_open_documents(open, &root, &target);

        let mut files: Vec<_> = sent.iter().map(|d| d.file.clone()).collect();
        files.sort();
        assert_eq!(
            files,
            vec![root.join("src/Sibling.Table.al"), root.join("src/Upper.Page.AL")]
        );
        assert_eq!(
            sent.iter()
                .find(|d| d.file.ends_with("Sibling.Table.al"))
                .map(|d| d.source.as_str()),
            Some("sibling")
        );
    }
}
