//! Assistant settings (`agent.json`): LLM providers, depth server, MCP.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    OpenAi,
    OpenRouter,
    Ollama,
    LmStudio,
    /// Any other OpenAI-compatible endpoint (vLLM, llama.cpp server, …).
    Custom,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 6] = [Self::Anthropic, Self::OpenAi, Self::OpenRouter, Self::Ollama, Self::LmStudio, Self::Custom];

    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
            Self::OpenRouter => "OpenRouter",
            Self::Ollama => "Ollama (local)",
            Self::LmStudio => "LM Studio (local)",
            Self::Custom => "Custom OpenAI-compatible",
        }
    }

    pub fn env_var(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
            Self::OpenRouter => "OPENROUTER_API_KEY",
            _ => "OPENAI_API_KEY",
        }
    }

    pub fn needs_key(self) -> bool {
        matches!(self, Self::Anthropic | Self::OpenAi | Self::OpenRouter)
    }

    pub fn default_model(self) -> String {
        self.default_cfg().model
    }

    fn default_cfg(self) -> ProviderCfg {
        let (base_url, model) = match self {
            Self::Anthropic => ("https://api.anthropic.com", "claude-opus-5-5"),
            Self::OpenAi => ("https://api.openai.com/v1", "gpt-5"),
            Self::OpenRouter => ("https://openrouter.ai/api/v1", "anthropic/claude-opus-5.5"),
            Self::Ollama => ("http://localhost:11434/v1", "qwen2.5vl:7b"),
            Self::LmStudio => ("http://localhost:1234/v1", "local-model"),
            Self::Custom => ("http://localhost:8000/v1", "model"),
        };
        ProviderCfg { kind: self, base_url: base_url.into(), model: model.into(), api_key: String::new(), anthropic_fallbacks: true }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderCfg {
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    /// Used only when the provider's environment variable is unset.
    #[serde(default)]
    pub api_key: String,
    /// Anthropic only: server-side refusal fallbacks (`fallbacks: "default"`).
    #[serde(default = "yes")]
    pub anthropic_fallbacks: bool,
}

fn yes() -> bool {
    true
}

impl ProviderCfg {
    /// The configured model, or the provider default when left empty.
    pub fn effective_model(&self) -> String {
        let m = self.model.trim();
        if m.is_empty() { self.kind.default_model() } else { m.to_string() }
    }

    pub fn resolved_key(&self) -> String {
        match std::env::var(self.kind.env_var()) {
            Ok(k) if !k.trim().is_empty() && self.kind.needs_key() => k.trim().to_string(),
            _ => self.api_key.trim().to_string(),
        }
    }

    pub fn key_from_env(&self) -> bool {
        self.kind.needs_key() && std::env::var(self.kind.env_var()).map(|k| !k.trim().is_empty()).unwrap_or(false)
    }
}

/// An external MCP server the assistant can use: either a command speaking
/// stdio, or a streamable-HTTP URL.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct McpServerCfg {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentConfig {
    pub active: ProviderKind,
    pub providers: Vec<ProviderCfg>,
    /// `low` | `medium` | `high` | `xhigh` | `max` (Anthropic `output_config.effort`).
    #[serde(default = "default_effort")]
    pub effort: String,
    /// Assumed horizontal field of view of reference photos, degrees.
    #[serde(default = "default_hfov")]
    pub photo_hfov_deg: f32,
    #[serde(default = "yes")]
    pub mcp_server: bool,
    #[serde(default = "default_port")]
    pub mcp_port: u16,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerCfg>,
    #[serde(default = "default_steps")]
    pub max_steps: usize,
    /// Attach clearly relevant knowledge base passages to each message.
    #[serde(default = "yes")]
    pub knowledge_auto: bool,
}

fn default_effort() -> String {
    "high".into()
}
fn default_hfov() -> f32 {
    65.0
}
fn default_port() -> u16 {
    8765
}
fn default_steps() -> usize {
    150
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            active: ProviderKind::Anthropic,
            providers: ProviderKind::ALL.iter().map(|k| k.default_cfg()).collect(),
            effort: default_effort(),
            photo_hfov_deg: default_hfov(),
            mcp_server: true,
            mcp_port: default_port(),
            mcp_servers: BTreeMap::new(),
            max_steps: default_steps(),
            knowledge_auto: true,
        }
    }
}

impl AgentConfig {
    /// `agent.json` next to the executable wins; otherwise the per-user copy.
    pub fn path() -> PathBuf {
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
        {
            let p = dir.join("agent.json");
            if p.exists() {
                return p;
            }
        }
        let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
        Path::new(&base).join("SenseSim").join("agent.json")
    }

    pub fn load() -> Self {
        let mut cfg: Self = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        // Make sure every provider kind has an entry.
        for k in ProviderKind::ALL {
            if !cfg.providers.iter().any(|p| p.kind == k) {
                cfg.providers.push(k.default_cfg());
            }
        }
        // 60 was the old default: too few for a photo rebuild plus its self-check rounds.
        if cfg.max_steps == 60 {
            cfg.max_steps = default_steps();
        }
        cfg
    }

    pub fn save(&self) -> std::io::Result<()> {
        let p = Self::path();
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(p, serde_json::to_string_pretty(self).unwrap())
    }

    pub fn provider(&self) -> &ProviderCfg {
        self.providers.iter().find(|p| p.kind == self.active).unwrap_or(&self.providers[0])
    }

    pub fn provider_mut(&mut self) -> &mut ProviderCfg {
        let k = self.active;
        let i = self.providers.iter().position(|p| p.kind == k).unwrap_or(0);
        &mut self.providers[i]
    }
}
