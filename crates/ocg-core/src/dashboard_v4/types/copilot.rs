use super::*;
pub use crate::copilot_application::{
    CopilotInspection, CopilotInstallation, CopilotStatus, CopilotTarget,
};
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CopilotApplication {
    #[serde(flatten)]
    #[schemars(flatten)]
    pub inspection: CopilotInspection,
    pub gateway_v1_url: String,
    pub revision: ControlRevision,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopilotInstallRequest {
    pub expected_revision: u64,
    pub process_generation: u64,
    pub target: CopilotTarget,
    pub expected_fingerprint: String,
    pub key_id: Option<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CopilotMutationRequest {
    pub expected_revision: u64,
    pub process_generation: u64,
    pub target: CopilotTarget,
    pub expected_fingerprint: String,
}
