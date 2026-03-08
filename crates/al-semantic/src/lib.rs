//! .NET bridge for CodeAnalysis API — semantic analysis, analyzers, compilation.

pub mod protocol;
pub mod process;

use std::path::{Path, PathBuf};

use al_discovery::AlToolchain;
use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::{Diagnostic, Position};

/// Async bridge to the .NET CodeAnalysis subprocess.
pub struct SemanticBridge {
    _child: tokio::process::Child,
}

/// Request to analyze a file.
#[derive(Debug, Serialize)]
pub struct AnalyzeRequest {
    pub file: PathBuf,
    pub source: String,
    pub analyzers: Vec<String>,
    pub package_cache: PathBuf,
}

/// Result of compilation.
#[derive(Debug, Deserialize)]
pub struct CompileResult {
    pub success: bool,
    pub diagnostics: Vec<(PathBuf, Diagnostic)>,
    pub app_path: Option<PathBuf>,
}

/// A built-in type from CodeAnalysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinType {
    pub name: String,
    pub methods: Vec<BuiltinMethod>,
}

/// A method on a built-in type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinMethod {
    pub name: String,
    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub documentation: String,
}

/// A method parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub type_name: String,
    pub is_optional: bool,
}

/// Type information at a position.
#[derive(Debug, Clone, Deserialize)]
pub struct TypeInfo {
    pub name: String,
    pub kind: String,
}

impl SemanticBridge {
    pub async fn spawn(toolchain: &AlToolchain) -> Result<Self, SemanticError> {
        let _ = toolchain;
        todo!("Implement .NET subprocess spawn")
    }

    pub async fn analyze(&self, req: AnalyzeRequest) -> Result<Vec<Diagnostic>, SemanticError> {
        let _ = req;
        todo!()
    }

    pub async fn compile(&self, project: &Path) -> Result<CompileResult, SemanticError> {
        let _ = project;
        todo!()
    }

    pub async fn type_at(
        &self,
        file: &Path,
        pos: Position,
    ) -> Result<Option<TypeInfo>, SemanticError> {
        let _ = (file, pos);
        todo!()
    }

    pub async fn builtin_types(&self) -> Result<Vec<BuiltinType>, SemanticError> {
        todo!()
    }

    pub async fn ping(&self) -> Result<(), SemanticError> {
        todo!()
    }

    pub async fn shutdown(self) {
        todo!()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error("Failed to spawn .NET bridge: {0}")]
    SpawnFailed(String),
    #[error("Bridge communication error: {0}")]
    Communication(String),
    #[error("Bridge process died")]
    ProcessDied,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
