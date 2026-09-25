use serde::Deserialize;
#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(alias = "affectsIndividualThreshold")]
    pub affects_individual_threshold: Option<f64>,
    #[serde(alias = "allowMock")]
    pub allow_mock: Option<bool>,
    #[serde(alias = "cacheTtlSeconds")]
    pub cache_ttl_seconds: Option<i64>,
    #[serde(alias = "clientIdHeader")]
    pub client_id_header: Option<String>,
    #[serde(alias = "cloudflareAccountId")]
    pub cloudflare_account_id: Option<String>,
    #[serde(alias = "customAuthHeader")]
    pub custom_auth_header: Option<String>,
    #[serde(alias = "failMode")]
    pub fail_mode: Option<String>,
    #[serde(alias = "highRiskCriteria")]
    pub high_risk_criteria: Option<String>,
    #[serde(alias = "jevApiKey")]
    pub jev_api_key: Option<String>,
    #[serde(alias = "jevModel")]
    pub jev_model: Option<String>,
    #[serde(alias = "jevPath")]
    pub jev_path: Option<String>,
    #[serde(alias = "jevProvider")]
    pub jev_provider: Option<String>,
    #[serde(alias = "jevService", deserialize_with = "pdk::serde::deserialize_service")]
    pub jev_service: pdk::hl::Service,
    #[serde(alias = "jevTimeoutMs")]
    pub jev_timeout_ms: Option<i64>,
    #[serde(alias = "logStateSample")]
    pub log_state_sample: Option<bool>,
    #[serde(alias = "maxStateTokens")]
    pub max_state_tokens: Option<i64>,
    #[serde(alias = "minConfidence")]
    pub min_confidence: Option<f64>,
    #[serde(alias = "minimalCriteria")]
    pub minimal_criteria: Option<String>,
    #[serde(alias = "mode")]
    pub mode: Option<String>,
    #[serde(alias = "onHighRisk")]
    pub on_high_risk: Option<String>,
    #[serde(alias = "onProhibited")]
    pub on_prohibited: Option<String>,
    #[serde(alias = "prohibitedConfidence")]
    pub prohibited_confidence: Option<f64>,
    #[serde(alias = "prohibitedCriteria")]
    pub prohibited_criteria: Option<String>,
    #[serde(alias = "registeredUseCases")]
    pub registered_use_cases: Option<Vec<String>>,
    #[serde(alias = "riskHeader")]
    pub risk_header: Option<String>,
    #[serde(alias = "routes")]
    pub routes: Option<Vec<String>>,
    #[serde(alias = "sampleRate")]
    pub sample_rate: Option<f64>,
    #[serde(alias = "stripClientJevHeaders")]
    pub strip_client_jev_headers: Option<bool>,
    #[serde(alias = "transparencyCriteria")]
    pub transparency_criteria: Option<String>,
    #[serde(alias = "useCaseHeader")]
    pub use_case_header: Option<String>,
}
#[pdk::hl::entrypoint_flex]
fn init(abi: &dyn pdk::flex_abi::api::FlexAbi) -> Result<(), anyhow::Error> {
    let config: Config = serde_json::from_slice(abi.get_configuration())
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to parse configuration '{}'. Cause: {}",
                String::from_utf8_lossy(abi.get_configuration()), err
            )
        })?;
    abi.service_create(config.jev_service)?;
    abi.setup()?;
    Ok(())
}
