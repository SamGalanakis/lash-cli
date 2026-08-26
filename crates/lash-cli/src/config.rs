//! User config-file schema (`~/.lash/config.json`) and helpers.
//!
//! `LashConfig` owns the data shape that the CLI persists to disk. The lash
//! runtime itself takes already-built primitives (`ProviderHandle`, plugin
//! factories) — it does not load this file.

use std::collections::BTreeMap;

use lash::provider::{Provider, ProviderHandle, ProviderOptions, StreamTermination};
use lash_plugin_mcp::McpServerConfig;
use lash_provider_anthropic::AnthropicProvider;
use lash_provider_google::{GoogleOAuthClient, GoogleOAuthProvider};
use lash_provider_openai::{CodexProvider, OpenAiCompat, OpenAiCompatibleProvider, OpenAiProvider};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeName {
    #[default]
    Lash,
    System,
}

impl ThemeName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lash => "lash",
            Self::System => "system",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Lash => "Lash",
            Self::System => "System",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Lash => "Use Lash's high-contrast dark palette.",
            Self::System => "Use terminal defaults and ANSI palette colors.",
        }
    }

    pub const fn all() -> [Self; 2] {
        [Self::Lash, Self::System]
    }
}

/// Auxiliary service secrets that are independent of LLM provider auth.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct AuxiliarySecrets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tavily_api_key: Option<String>,
}

impl AuxiliarySecrets {
    fn is_empty(&self) -> bool {
        self.tavily_api_key.is_none()
    }
}

/// User-selected default model for fresh sessions, scoped to a provider kind.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelDefault {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

/// Host-owned provider persistence record.
///
/// This deliberately preserves the former flat `{ "type", ... }` JSON shape
/// so existing `~/.lash/config.json` credentials remain readable while
/// provider construction stays outside Lash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderConfig {
    pub kind: String,
    pub config: serde_json::Value,
}

impl Serialize for ProviderConfig {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut value = match &self.config {
            serde_json::Value::Object(map) => serde_json::Value::Object(map.clone()),
            serde_json::Value::Null => serde_json::Value::Object(serde_json::Map::new()),
            other => {
                return Err(serde::ser::Error::custom(format!(
                    "ProviderConfig.config must be a JSON object, got {other}"
                )));
            }
        };
        if let serde_json::Value::Object(map) = &mut value {
            map.insert(
                "type".to_string(),
                serde_json::Value::String(self.kind.clone()),
            );
        }
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProviderConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let kind = if let serde_json::Value::Object(map) = &mut value {
            let raw = map
                .remove("type")
                .ok_or_else(|| serde::de::Error::missing_field("type"))?;
            raw.as_str()
                .ok_or_else(|| serde::de::Error::custom("provider `type` must be a string"))?
                .to_string()
        } else {
            return Err(serde::de::Error::custom(
                "provider config must be a JSON object",
            ));
        };
        Ok(Self {
            kind,
            config: value,
        })
    }
}

impl ProviderConfig {
    pub fn from_provider(provider: &impl Provider) -> Self {
        Self {
            kind: provider.kind().to_string(),
            config: provider.serialize_config(),
        }
    }
}

/// Result of attempting to read `config.json`.
#[derive(Clone, Debug)]
pub enum ConfigLoadOutcome {
    Missing,
    Invalid { reason: String },
    Loaded(LashConfig),
}

impl ConfigLoadOutcome {
    pub fn loaded(&self) -> Option<&LashConfig> {
        match self {
            Self::Loaded(config) => Some(config),
            Self::Missing | Self::Invalid { .. } => None,
        }
    }

    pub fn into_loaded(self) -> Option<LashConfig> {
        match self {
            Self::Loaded(config) => Some(config),
            Self::Missing | Self::Invalid { .. } => None,
        }
    }

    pub fn status_line(&self) -> Option<String> {
        match self {
            Self::Missing => Some("config: not found".to_string()),
            Self::Invalid { reason } => Some(format!("config: present but invalid ({reason})")),
            Self::Loaded(_) => None,
        }
    }
}

/// Stored configuration: provider credentials + service API keys + MCP
/// servers + per-session defaults.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LashConfig {
    pub active_provider: String,
    #[serde(default)]
    pub theme: ThemeName,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Default execution mode for newly created sessions.
    #[serde(default)]
    pub execution_mode: crate::execution_settings::ExecutionMode,
    /// Default RLM dialect for newly created RLM sessions.
    #[serde(default)]
    pub rlm_dialect: crate::execution_settings::RlmDialect,
    #[serde(default, skip_serializing_if = "AuxiliarySecrets::is_empty")]
    pub auxiliary_secrets: AuxiliarySecrets,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
    /// User-overridable model names per subagent capability. Generic
    /// name → model map; the meaning of each name is owned by whatever
    /// builds the subagent capability registry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_models: BTreeMap<String, String>,
    /// Fresh-session model defaults keyed by provider kind. Session
    /// resumes still use the session head's persisted model instead.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_defaults: BTreeMap<String, ModelDefault>,
}

impl LashConfig {
    /// Construct a config from an already-serialized provider record.
    pub fn new(provider: ProviderConfig) -> Self {
        let kind = provider.kind.clone();
        let mut providers = BTreeMap::new();
        providers.insert(kind.clone(), provider);
        Self {
            active_provider: kind,
            theme: ThemeName::default(),
            providers,
            execution_mode: crate::execution_settings::ExecutionMode::default(),
            rlm_dialect: crate::execution_settings::RlmDialect::default(),
            auxiliary_secrets: AuxiliarySecrets::default(),
            mcp_servers: BTreeMap::new(),
            agent_models: BTreeMap::new(),
            model_defaults: BTreeMap::new(),
        }
    }

    pub fn active_provider_config(&self) -> &ProviderConfig {
        self.providers
            .get(&self.active_provider)
            .expect("active provider missing from config")
    }

    pub fn active_provider_kind(&self) -> &str {
        &self.active_provider
    }

    pub fn set_active_provider_kind(&mut self, kind: &str) -> Result<(), String> {
        if !self.providers.contains_key(kind) {
            return Err(format!("provider `{}` is not configured", kind));
        }
        self.active_provider = kind.to_string();
        Ok(())
    }

    pub fn provider_config(&self, kind: &str) -> Option<&ProviderConfig> {
        self.providers.get(kind)
    }

    pub fn provider_kinds(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }

    pub fn has_provider(&self, kind: &str) -> bool {
        self.providers.contains_key(kind)
    }

    pub fn upsert_provider(&mut self, provider: ProviderConfig) {
        self.providers.insert(provider.kind.clone(), provider);
    }

    pub fn remove_provider(&mut self, kind: &str) -> Option<ProviderConfig> {
        let removed = self.providers.remove(kind)?;
        if self.providers.is_empty() {
            return Some(removed);
        }
        if self.active_provider == kind {
            self.active_provider = self
                .providers
                .keys()
                .next()
                .cloned()
                .expect("providers should be non-empty after removal");
        }
        Some(removed)
    }

    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }

    pub fn model_default(&self, provider_kind: &str) -> Option<&ModelDefault> {
        self.model_defaults.get(provider_kind)
    }

    pub fn set_model_default(
        &mut self,
        provider_kind: impl Into<String>,
        model: impl Into<String>,
        variant: Option<String>,
    ) {
        self.model_defaults.insert(
            provider_kind.into(),
            ModelDefault {
                model: model.into(),
                variant,
            },
        );
    }

    /// Materialize the active provider with the providers compiled into the CLI.
    pub fn build_active_provider(&self) -> Result<ProviderHandle, String> {
        materialize_provider(self.active_provider_config())
    }

    /// Load from the given config path. Returns `None` if missing or
    /// malformed.
    pub fn load(path: &std::path::Path) -> Option<Self> {
        Self::load_outcome(path).into_loaded()
    }

    /// Load from the given config path with a diagnostic outcome.
    pub fn load_outcome(path: &std::path::Path) -> ConfigLoadOutcome {
        if !path.exists() {
            return ConfigLoadOutcome::Missing;
        }
        let data = match std::fs::read_to_string(path) {
            Ok(data) => data,
            Err(err) => {
                return ConfigLoadOutcome::Invalid {
                    reason: format!("could not read config: {err}"),
                };
            }
        };
        let config = match serde_json::from_str::<Self>(&data) {
            Ok(config) => config,
            Err(err) => {
                return ConfigLoadOutcome::Invalid {
                    reason: format!("invalid config JSON: {err}"),
                };
            }
        };
        if !config.providers.contains_key(&config.active_provider) {
            return ConfigLoadOutcome::Invalid {
                reason: format!(
                    "active_provider `{}` is not configured in providers",
                    config.active_provider
                ),
            };
        }
        ConfigLoadOutcome::Loaded(config)
    }

    /// Save to the given config path (mode 0o600 on Unix).
    pub fn save(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, &data)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    pub fn tavily_api_key(&self) -> Option<&str> {
        self.auxiliary_secrets.tavily_api_key.as_deref()
    }

    pub fn set_tavily_api_key(&mut self, key: Option<String>) {
        self.auxiliary_secrets.tavily_api_key = key;
    }

    pub fn mcp_servers(&self) -> &BTreeMap<String, McpServerConfig> {
        &self.mcp_servers
    }

    /// Delete the config file at `path`.
    pub fn clear(path: &std::path::Path) -> Result<(), std::io::Error> {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnthropicConfig {
    api_key: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    options: ProviderOptions,
    #[serde(default = "require_terminal_evidence")]
    stream_termination: StreamTermination,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiConfig {
    api_key: String,
    #[serde(default)]
    options: ProviderOptions,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiCompatibleConfig {
    api_key: String,
    base_url: String,
    #[serde(default)]
    options: ProviderOptions,
    #[serde(default)]
    compat: OpenAiCompat,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexConfig {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    options: ProviderOptions,
    #[serde(default)]
    transport: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoogleConfig {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
    #[serde(default)]
    oauth_client_id: Option<String>,
    #[serde(default)]
    oauth_client_secret: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    api_version: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    options: ProviderOptions,
    #[serde(default = "eof_tolerated")]
    stream_termination: StreamTermination,
}

fn require_terminal_evidence() -> StreamTermination {
    StreamTermination::RequireTerminalEvidence
}

fn eof_tolerated() -> StreamTermination {
    StreamTermination::EofTolerated
}

fn decode<T: for<'de> Deserialize<'de>>(config: &ProviderConfig) -> Result<T, String> {
    serde_json::from_value(config.config.clone())
        .map_err(|error| format!("invalid {} provider config: {error}", config.kind))
}

fn google_oauth_client(config: &GoogleConfig) -> Result<GoogleOAuthClient, String> {
    let id = config
        .oauth_client_id
        .clone()
        .or_else(|| std::env::var("LASH_GOOGLE_CLIENT_ID").ok());
    let secret = config
        .oauth_client_secret
        .clone()
        .or_else(|| std::env::var("LASH_GOOGLE_CLIENT_SECRET").ok());
    match (id, secret) {
        (Some(id), Some(secret)) if !id.trim().is_empty() && !secret.trim().is_empty() => {
            Ok(GoogleOAuthClient { id, secret })
        }
        _ => Err(
            "google_oauth provider config requires oauth_client_id/oauth_client_secret or both LASH_GOOGLE_CLIENT_ID and LASH_GOOGLE_CLIENT_SECRET"
                .to_string(),
        ),
    }
}

pub fn materialize_provider(config: &ProviderConfig) -> Result<ProviderHandle, String> {
    let components = match config.kind.as_str() {
        "anthropic" => {
            let cfg: AnthropicConfig = decode(config)?;
            AnthropicProvider::new(cfg.api_key)
                .with_base_url(cfg.base_url)
                .with_options(cfg.options)
                .with_stream_termination(cfg.stream_termination)
                .into_components()
        }
        "openai" => {
            let cfg: OpenAiConfig = decode(config)?;
            OpenAiProvider::new(cfg.api_key)
                .with_options(cfg.options)
                .into_components()
        }
        "openai-compatible" => {
            let cfg: OpenAiCompatibleConfig = decode(config)?;
            OpenAiCompatibleProvider::new(cfg.api_key, cfg.base_url)
                .with_options(cfg.options)
                .with_compat(cfg.compat)
                .into_components()
        }
        "codex" => {
            let cfg: CodexConfig = decode(config)?;
            let mut provider =
                CodexProvider::new(cfg.access_token, cfg.refresh_token, cfg.expires_at)
                    .with_account_id(cfg.account_id)
                    .with_options(cfg.options);
            match cfg.transport.as_deref() {
                None | Some("auto") => {}
                Some("sse") => provider = provider.force_sse_transport(),
                Some(other) => {
                    return Err(format!(
                        "codex transport `{other}` cannot be restored by this provider API"
                    ));
                }
            }
            provider.into_components()
        }
        "google_oauth" => {
            let cfg: GoogleConfig = decode(config)?;
            let oauth_client = google_oauth_client(&cfg)?;
            let mut provider = GoogleOAuthProvider::new(
                cfg.access_token,
                cfg.refresh_token,
                cfg.expires_at,
                oauth_client,
            )
            .with_project_id(cfg.project_id)
            .with_options(cfg.options)
            .with_stream_termination(cfg.stream_termination);
            if let Some(endpoint) = cfg.endpoint {
                provider = provider.with_endpoint(endpoint);
            }
            if let Some(api_version) = cfg.api_version {
                provider = provider.with_api_version(api_version);
            }
            provider.into_components()
        }
        #[cfg(feature = "test-provider")]
        "test" => return materialize_test_provider(config),
        other => {
            return Err(format!(
                "provider `{other}` is not supported by this CLI build"
            ));
        }
    };
    Ok(ProviderHandle::new(components))
}

#[cfg(feature = "test-provider")]
fn materialize_test_provider(config: &ProviderConfig) -> Result<ProviderHandle, String> {
    let scenario = config
        .config
        .get("scenario")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("rlm-subagent-smoke");
    match scenario {
        "standard-echo" => Ok(standard_echo_provider().into_handle()),
        "standard-slow-echo" => Ok(standard_slow_echo_provider().into_handle()),
        "standard-gated-escape" => Ok(standard_gated_escape_provider().into_handle()),
        "rlm-subagent-smoke" => Ok(rlm_subagent_smoke_provider().into_handle()),
        "rlm-typescript-smoke" => Ok(rlm_typescript_smoke_provider().into_handle()),
        "rlm-workspace-smoke" => Ok(rlm_workspace_smoke_provider().into_handle()),
        "rlm-nonzero-exit-smoke" => Ok(rlm_nonzero_exit_smoke_provider().into_handle()),
        other => Err(format!("unknown CLI test provider scenario `{other}`")),
    }
}

#[cfg(feature = "test-provider")]
fn rlm_typescript_smoke_provider() -> lash::testing::TestProvider {
    lash::testing::TestProvider::builder()
        .kind("test")
        .serialize_config(|| {
            serde_json::json!({
                "scenario": "rlm-typescript-smoke",
            })
        })
        .complete(|request| async move {
            let result = if request_contains_text(&request, "Second TypeScript turn") {
                "typescript-ok-2"
            } else {
                "typescript-ok-1"
            };
            let response = format!("<typescript>\nfinish(\"{result}\");\n</typescript>");
            Ok(lash::provider::LlmResponse {
                parts: vec![lash::direct::LlmOutputPart::Text {
                    text: response,
                    response_meta: None,
                }],
                ..Default::default()
            })
        })
        .build()
}

#[cfg(feature = "test-provider")]
fn standard_echo_provider() -> lash::testing::TestProvider {
    lash::testing::TestProvider::builder()
        .kind("test")
        .serialize_config(|| {
            serde_json::json!({
                "scenario": "standard-echo",
            })
        })
        .complete(|request| async move {
            record_test_provider_request("standard-echo", &request);
            let prompt = if request_contains_text(&request, "hello from pty") {
                "hello from pty"
            } else {
                "interactive prompt"
            };
            let response = format!("test-provider echo: {prompt}");
            Ok(lash::provider::LlmResponse {
                parts: vec![lash::direct::LlmOutputPart::Text {
                    text: response,
                    response_meta: None,
                }],
                ..Default::default()
            })
        })
        .build()
}

#[cfg(feature = "test-provider")]
fn standard_slow_echo_provider() -> lash::testing::TestProvider {
    lash::testing::TestProvider::builder()
        .kind("test")
        .serialize_config(|| {
            serde_json::json!({
                "scenario": "standard-slow-echo",
            })
        })
        .complete(|request| async move {
            record_test_provider_request("standard-slow-echo", &request);
            let response = if request_contains_text(&request, "queued after escape") {
                "test-provider echo: queued after escape"
            } else if request_contains_text(&request, "slow initial prompt") {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                "test-provider echo: slow initial prompt"
            } else {
                "test-provider echo: interactive prompt"
            };
            Ok(lash::provider::LlmResponse {
                parts: vec![lash::direct::LlmOutputPart::Text {
                    text: response.to_string(),
                    response_meta: None,
                }],
                ..Default::default()
            })
        })
        .build()
}

#[cfg(feature = "test-provider")]
fn standard_gated_escape_provider() -> lash::testing::TestProvider {
    lash::testing::TestProvider::builder()
        .kind("test")
        .serialize_config(|| {
            serde_json::json!({
                "scenario": "standard-gated-escape",
            })
        })
        .complete(|request| async move {
            record_test_provider_request("standard-gated-escape", &request);
            let response = if request_contains_text(&request, "queued after escape") {
                "test-provider echo: queued after escape"
            } else if request_contains_text(&request, "gated initial prompt") {
                write_test_provider_marker("gated-initial-started");
                wait_for_test_provider_marker("gated-initial-release").await;
                "test-provider echo: gated initial prompt"
            } else {
                "test-provider echo: interactive prompt"
            };
            Ok(lash::provider::LlmResponse {
                parts: vec![lash::direct::LlmOutputPart::Text {
                    text: response.to_string(),
                    response_meta: None,
                }],
                ..Default::default()
            })
        })
        .build()
}

#[cfg(feature = "test-provider")]
fn rlm_subagent_smoke_provider() -> lash::testing::TestProvider {
    lash::testing::TestProvider::builder()
        .kind("test")
        .serialize_config(|| {
            serde_json::json!({
                "scenario": "rlm-subagent-smoke",
            })
        })
        .complete(|request| async move {
            let response = if request_contains_subagent_prompt(&request) {
                r#"<lashlang>
finish { value: "subagent-ok" }
</lashlang>"#
            } else {
                r#"<lashlang>
result = await agents.spawn({
  capability: "explore",
  task: "Finish `{ value: \"subagent-ok\" }` exactly.",
  output: Type { value: str }
})?
finish result.value
</lashlang>"#
            };
            Ok(lash::provider::LlmResponse {
                parts: vec![lash::direct::LlmOutputPart::Text {
                    text: response.to_string(),
                    response_meta: None,
                }],
                ..Default::default()
            })
        })
        .build()
}

#[cfg(feature = "test-provider")]
fn rlm_workspace_smoke_provider() -> lash::testing::TestProvider {
    lash::testing::TestProvider::builder()
        .kind("test")
        .serialize_config(|| {
            serde_json::json!({
                "scenario": "rlm-workspace-smoke",
            })
        })
        .complete(|_request| async move {
            let response = r#"<lashlang>
pwd = await shell.exec({ cmd: "pwd" })?
write = await shell.exec({ cmd: "printf '%s\n' workspace-smoke-ok > qc-workspace.txt" })?
if write.exit_code == 0 {
  finish format("workspace-smoke-ok cwd={}", trim(pwd.output))
} else {
  finish format("workspace-smoke-failed exit={}", write.exit_code)
}
</lashlang>"#;
            Ok(lash::provider::LlmResponse {
                parts: vec![lash::direct::LlmOutputPart::Text {
                    text: response.to_string(),
                    response_meta: None,
                }],
                ..Default::default()
            })
        })
        .build()
}

#[cfg(feature = "test-provider")]
fn rlm_nonzero_exit_smoke_provider() -> lash::testing::TestProvider {
    lash::testing::TestProvider::builder()
        .kind("test")
        .serialize_config(|| {
            serde_json::json!({
                "scenario": "rlm-nonzero-exit-smoke",
            })
        })
        .complete(|_request| async move {
            let response = r#"<lashlang>
result = await shell.exec({ cmd: "sh -c 'echo qc-nonzero-stderr >&2; exit 7'" })?
finish format("nonzero-smoke-ok exit={}", result.exit_code)
</lashlang>"#;
            Ok(lash::provider::LlmResponse {
                parts: vec![lash::direct::LlmOutputPart::Text {
                    text: response.to_string(),
                    response_meta: None,
                }],
                ..Default::default()
            })
        })
        .build()
}

#[cfg(feature = "test-provider")]
fn record_test_provider_request(scenario: &str, request: &lash::provider::LlmRequest) {
    let Some(lash_home) = std::env::var_os("LASH_HOME") else {
        return;
    };
    let path = std::path::PathBuf::from(lash_home).join("test-provider-requests.jsonl");
    let user_texts = request_visible_user_texts(request);
    let payload = serde_json::json!({
        "scenario": scenario,
        "last_user_text": user_texts.last().cloned().unwrap_or_default(),
        "user_texts": user_texts,
    });
    let Ok(line) = serde_json::to_string(&payload) else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write as _;
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(feature = "test-provider")]
fn request_visible_user_texts(request: &lash::provider::LlmRequest) -> Vec<String> {
    request
        .messages
        .iter()
        .filter(|message| message.role == lash::provider::LlmRole::User)
        .filter_map(|message| {
            let text = message
                .blocks
                .iter()
                .filter_map(|part| match part {
                    lash::provider::LlmContentBlock::Text { text, .. } => Some(text.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim_start().starts_with("<system-reminder>")).then_some(text)
        })
        .collect()
}

#[cfg(feature = "test-provider")]
fn write_test_provider_marker(name: &str) {
    if let Some(lash_home) = std::env::var_os("LASH_HOME") {
        let path = std::path::PathBuf::from(lash_home).join(name);
        let _ = std::fs::write(path, b"ready\n");
    }
}

#[cfg(feature = "test-provider")]
async fn wait_for_test_provider_marker(name: &str) {
    loop {
        let Some(lash_home) = std::env::var_os("LASH_HOME") else {
            return;
        };
        if std::path::PathBuf::from(lash_home).join(name).exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[cfg(feature = "test-provider")]
fn request_contains_text(request: &lash::provider::LlmRequest, needle: &str) -> bool {
    request.messages.iter().any(|message| {
        message.blocks.iter().any(|part| match part {
            lash::provider::LlmContentBlock::Text { text, .. } => text.contains(needle),
            _ => false,
        })
    })
}

#[cfg(feature = "test-provider")]
fn request_contains_subagent_prompt(request: &lash::provider::LlmRequest) -> bool {
    request_contains_text(request, "Subagent capability: explore. Depth: 1/5.")
        || request_contains_text(request, "Finish `{ value: \\\"subagent-ok\\\" }` exactly.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_outcome_reports_missing_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        assert!(matches!(
            LashConfig::load_outcome(&path),
            ConfigLoadOutcome::Missing
        ));
        assert!(LashConfig::load(&path).is_none());
    }

    #[test]
    fn load_outcome_reports_unknown_fields() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "active_provider": "openai-compatible",
                "unexpected_top_level_field": true,
                "providers": {
                    "openai-compatible": {
                        "type": "openai-compatible",
                        "api_key": "k",
                        "base_url": "https://example.com/v1"
                    }
                }
            })
            .to_string(),
        )
        .expect("write config");

        let outcome = LashConfig::load_outcome(&path);
        assert!(matches!(outcome, ConfigLoadOutcome::Invalid { .. }));
        let reason = match outcome {
            ConfigLoadOutcome::Invalid { reason } => reason,
            other => panic!("expected invalid config, got {other:?}"),
        };
        assert!(
            reason.contains("unknown field `unexpected_top_level_field`"),
            "{reason}"
        );
    }

    #[test]
    fn load_outcome_reports_active_provider_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "active_provider": "anthropic",
                "providers": {
                    "openai-compatible": {
                        "type": "openai-compatible",
                        "api_key": "k",
                        "base_url": "https://example.com/v1"
                    }
                }
            })
            .to_string(),
        )
        .expect("write config");

        let outcome = LashConfig::load_outcome(&path);
        assert!(matches!(outcome, ConfigLoadOutcome::Invalid { .. }));
        let reason = match outcome {
            ConfigLoadOutcome::Invalid { reason } => reason,
            other => panic!("expected invalid config, got {other:?}"),
        };
        assert!(reason.contains("active_provider `anthropic`"));
    }

    #[test]
    fn lash_config_roundtrips_existing_shape() {
        let raw = serde_json::json!({
            "active_provider": "openai",
            "providers": {
                "openai": {
                    "type": "openai",
                    "api_key": "direct-k"
                },
                "openai-compatible": {
                    "type": "openai-compatible",
                    "api_key": "k",
                    "base_url": "https://example.com/v1"
                }
            }
        });
        let cfg: LashConfig = serde_json::from_value(raw.clone()).expect("valid config");
        assert_eq!(cfg.active_provider, "openai");
        assert_eq!(cfg.theme, ThemeName::Lash);
        let spec = cfg.active_provider_config();
        assert_eq!(spec.kind, "openai");
        assert_eq!(spec.config["api_key"], serde_json::json!("direct-k"));
        let compatible = cfg
            .provider_config("openai-compatible")
            .expect("compatible");
        assert_eq!(compatible.config["base_url"], "https://example.com/v1");

        let rendered = serde_json::to_value(&cfg).expect("serialize config");
        assert_eq!(rendered["providers"]["openai"], raw["providers"]["openai"]);
        assert_eq!(
            rendered["providers"]["openai-compatible"],
            raw["providers"]["openai-compatible"]
        );
    }

    #[test]
    fn theme_preference_roundtrips() {
        let raw = serde_json::json!({
            "active_provider": "openai-compatible",
            "theme": "system",
            "providers": {
                "openai-compatible": {
                    "type": "openai-compatible",
                    "api_key": "k",
                    "base_url": "https://example.com/v1"
                }
            }
        });
        let cfg: LashConfig = serde_json::from_value(raw).expect("valid config json");
        assert_eq!(cfg.theme, ThemeName::System);

        let rendered = serde_json::to_value(&cfg).expect("serialize config");
        assert_eq!(rendered["theme"], serde_json::json!("system"));
    }

    #[test]
    fn rejects_unknown_top_level_config_fields() {
        let raw = serde_json::json!({
            "active_provider": "openai-compatible",
            "providers": {
                "openai-compatible": {
                    "type": "openai-compatible",
                    "api_key": "k",
                    "base_url": "https://example.com/v1"
                }
            },
            "tavily_api_key": "legacy-key"
        });
        let err = serde_json::from_value::<LashConfig>(raw).expect_err("unknown field rejected");
        assert!(err.to_string().contains("unknown field `tavily_api_key`"));
    }

    #[test]
    fn auxiliary_secrets_preserved() {
        let raw = serde_json::json!({
            "active_provider": "openai-compatible",
            "providers": {
                "openai-compatible": {
                    "type": "openai-compatible",
                    "api_key": "k",
                    "base_url": "https://example.com/v1"
                }
            },
            "auxiliary_secrets": {
                "tavily_api_key": "new-key"
            }
        });
        let cfg: LashConfig = serde_json::from_value(raw).expect("valid config json");
        assert_eq!(cfg.tavily_api_key(), Some("new-key"));
    }

    #[test]
    fn model_defaults_are_provider_scoped() {
        let raw = serde_json::json!({
            "active_provider": "openai-compatible",
            "providers": {
                "openai-compatible": {
                    "type": "openai-compatible",
                    "api_key": "k",
                    "base_url": "https://example.com/v1"
                }
            },
            "model_defaults": {
                "openai-compatible": {
                    "model": "gpt-5.4",
                    "variant": "high"
                }
            }
        });
        let mut cfg: LashConfig = serde_json::from_value(raw).expect("valid config json");
        assert_eq!(
            cfg.model_default("openai-compatible"),
            Some(&ModelDefault {
                model: "gpt-5.4".to_string(),
                variant: Some("high".to_string()),
            })
        );

        cfg.set_model_default("anthropic", "claude-sonnet-4.6", None);
        assert_eq!(
            cfg.model_default("anthropic"),
            Some(&ModelDefault {
                model: "claude-sonnet-4.6".to_string(),
                variant: Default::default(),
            })
        );
    }
}
