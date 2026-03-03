// Copyright (c) 2025 Dmitry Markin
//
// SPDX-License-Identifier: MIT
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

//! `jutella-xmpp` configuration.

use anyhow::{anyhow, Context as _};
use clap::Parser;
use serde_json::value::Value;
use std::{fs, path::PathBuf, str::FromStr, time::Duration};
use xmpp_parsers::jid::BareJid;

const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Config file location.
    #[arg(short, long, default_value = "/etc/jutellaxmpp.toml")]
    config: PathBuf,
}

#[derive(Debug, serde::Deserialize)]
struct ConfigFile {
    jid: String,
    password: String,
    allowed_users: Vec<String>,
    api: Option<String>,
    api_url: String,
    api_version: Option<String>,
    api_key: Option<String>,
    api_token: Option<String>,
    http_timeout: Option<u64>,
    model: String,
    system_message: Option<String>,
    system_message_tokens: Option<usize>,
    reasoning_effort: Option<String>,
    reasoning_budget: Option<i64>,
    verbosity: Option<String>,
    sanitize_links: Option<bool>,
    min_history_tokens: Option<usize>,
    max_history_tokens: usize,
    openrouter_pdf_engine: Option<String>,
    image_generation: Option<bool>,
    extra_params_json: Option<String>,
    asr_url: Option<String>,
    asr_token: Option<String>,
    asr_model: Option<String>,
}

impl ConfigFile {
    fn load(path: PathBuf) -> anyhow::Result<Self> {
        let config = fs::read_to_string(path.clone()).with_context(|| {
            anyhow!(
                "Failed to read config file {}",
                path.to_str().expect("to have only unicode characters"),
            )
        })?;

        toml::from_str(&config).context("Invalid config")
    }
}

#[derive(Debug)]
pub struct Config {
    pub auth_jid: BareJid,
    pub auth_password: String,
    pub allowed_users: Vec<String>,
    pub api_url: String,
    pub api_options: jutella::ApiOptions,
    pub api_version: Option<String>,
    pub api_auth: jutella::Auth,
    pub http_timeout: Duration,
    pub model: String,
    pub system_message: Option<String>,
    pub system_message_tokens: Option<usize>,
    pub verbosity: Option<String>,
    pub sanitize_links: bool,
    pub min_history_tokens: Option<usize>,
    pub max_history_tokens: usize,
    pub extra_params: Option<serde_json::map::Map<String, Value>>,
    pub asr_url: Option<String>,
    pub asr_token: Option<String>,
    pub asr_model: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum ApiType {
    OpenAi,
    OpenRouter,
}

impl FromStr for ApiType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "openai" => Ok(ApiType::OpenAi),
            "openrouter" => Ok(ApiType::OpenRouter),
            _ => Err(anyhow!("Unsupported API flavor in config: {}", s)),
        }
    }
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let Args { config } = Args::parse();
        let ConfigFile {
            jid,
            password,
            allowed_users,
            api,
            api_url,
            api_version,
            api_key,
            api_token,
            http_timeout,
            model,
            system_message,
            system_message_tokens,
            reasoning_effort,
            reasoning_budget,
            verbosity,
            sanitize_links,
            min_history_tokens,
            max_history_tokens,
            openrouter_pdf_engine: pdf_engine,
            image_generation,
            extra_params_json,
            asr_url,
            asr_token,
            asr_model,
        } = ConfigFile::load(config)?;

        let auth_jid = BareJid::new(&jid).context("Invalid auth JID")?;

        let api_auth = match (api_key, api_token) {
            (Some(api_key), None) => jutella::Auth::ApiKey(api_key),
            (None, Some(token)) => jutella::Auth::Token(token),
            _ => {
                return Err(anyhow!(
                    "Exactly one of `api_key` & `api_token` must be provided"
                ))
            }
        };

        let api_type = api
            .as_deref()
            .map_or(Ok(ApiType::OpenAi), ApiType::from_str)?;

        let image_generation = image_generation.unwrap_or_default();

        let api_options = match (api_type, reasoning_effort, reasoning_budget) {
            (ApiType::OpenAi, effort, None) => jutella::ApiOptions::OpenAi {
                reasoning_effort: effort,
            },
            (ApiType::OpenRouter, None, None) => jutella::ApiOptions::OpenRouter {
                reasoning: None,
                pdf_engine,
                image_generation,
            },
            (ApiType::OpenRouter, Some(effort), None) => jutella::ApiOptions::OpenRouter {
                reasoning: Some(jutella::ReasoningSettings::Effort(effort)),
                pdf_engine,
                image_generation,
            },
            (ApiType::OpenRouter, None, Some(budget)) => jutella::ApiOptions::OpenRouter {
                reasoning: Some(jutella::ReasoningSettings::Budget(budget)),
                pdf_engine,
                image_generation,
            },
            _ => {
                return Err(anyhow!(
                    "Only one of `reasoning_effort` or `reasoning_budget` can be supplied. \
                     `reasoning_budget` is only supported by OpenRouter API."
                ))
            }
        };

        let http_timeout = http_timeout
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_HTTP_TIMEOUT);

        let sanitize_links = sanitize_links.unwrap_or_default();

        let extra_params = extra_params_json
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .context("not a valid JSON in `extra_params_json`")?
            .map(|json: Value| {
                json.as_object()
                    .cloned()
                    .context("not a JSON map in `extra_params_json`")
            })
            .transpose()?;

        Ok(Self {
            auth_jid,
            auth_password: password,
            allowed_users,
            api_url,
            api_options,
            api_version,
            api_auth,
            http_timeout,
            model,
            system_message,
            system_message_tokens,
            verbosity,
            sanitize_links,
            min_history_tokens,
            max_history_tokens,
            extra_params,
            asr_url,
            asr_token,
            asr_model,
        })
    }
}
