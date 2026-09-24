use crate::model::Project;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const FORMAT_VERSION: u32 = 1;
const MAGIC: &str = "openatelier-project";

#[derive(Serialize, Deserialize)]
pub struct ProjectFile {
    pub format: String,
    pub version: u32,
    pub project: serde_json::Value,
}

#[derive(Debug)]
pub enum FileError {
    NotAProject,
    TooNew(u32),
    Json(serde_json::Error),
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FileError::NotAProject => write!(f, "not an OpenAtelier project file"),
            FileError::TooNew(v) => write!(f, "project format v{v} is newer than this build (v{FORMAT_VERSION})"),
            FileError::Json(e) => write!(f, "invalid project file: {e}"),
        }
    }
}

impl std::error::Error for FileError {}

impl From<serde_json::Error> for FileError {
    fn from(e: serde_json::Error) -> Self {
        FileError::Json(e)
    }
}

impl ProjectFile {
    pub fn to_json(project: &Project) -> Result<String, FileError> {
        let file = ProjectFile { format: MAGIC.into(), version: FORMAT_VERSION, project: serde_json::to_value(project)? };
        Ok(serde_json::to_string_pretty(&file)?)
    }

    /// Parses a project, upgrading older formats and repairing broken invariants.
    pub fn from_json(text: &str) -> Result<Project, FileError> {
        Self::load(text).map(|(project, _)| project)
    }

    /// Like [`from_json`](Self::from_json), also returning what [`crate::repair`] fixed or
    /// found, so the UI can tell the user.
    pub fn load(text: &str) -> Result<(Project, crate::Report), FileError> {
        let file: ProjectFile = serde_json::from_str(text)?;
        if file.format != MAGIC {
            return Err(FileError::NotAProject);
        }
        if file.version > FORMAT_VERSION {
            return Err(FileError::TooNew(file.version));
        }
        let value = migrate(file.version, file.project);
        let mut project: Project = serde_json::from_value(value)?;
        let report = crate::repair(&mut project);
        Ok((project, report))
    }
}

/// Upgrades older documents step by step, operating on raw JSON so old structs never
/// need to be kept around.
fn migrate(mut version: u32, value: serde_json::Value) -> serde_json::Value {
    while version < FORMAT_VERSION {
        // match version { 1 => value = v1_to_v2(value), ... }
        version += 1;
    }
    value
}
