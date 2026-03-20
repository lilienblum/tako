#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct BuildStageCommand {
    pub name: Option<String>,
    pub working_dir: Option<String>,
    pub install: Option<String>,
    pub run: String,
}
