//! Errors reported when a package fails to load.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageLoadFailure {
    pub path: std::path::PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageLoadError {
    pub failures: Vec<PackageLoadFailure>,
}

impl std::fmt::Display for PackageLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} symbol package{} failed to load",
            self.failures.len(),
            if self.failures.len() == 1 { "" } else { "s" }
        )?;
        for failure in &self.failures {
            write!(
                formatter,
                "; '{}': {}",
                failure.path.display(),
                failure.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for PackageLoadError {}
