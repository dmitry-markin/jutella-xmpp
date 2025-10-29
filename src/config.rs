// Copyright (c) 2024 Dmitry Markin
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
    reasoning_effort: Option<String>,
    reasoning_budget: Option<i64>,
    verbosity: Option<String>,
    min_history_tokens: Option<usize>,
    max_history_tokens: usize,
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
    pub verbosity: Option<String>,
    pub min_history_tokens: Option<usize>,
    pub max_history_tokens: usize,
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
            reasoning_effort,
            reasoning_budget,
            verbosity,
            min_history_tokens,
            max_history_tokens,
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

        let api_options = match (api_type, reasoning_effort, reasoning_budget) {
            (ApiType::OpenAi, effort, None) => jutella::ApiOptions::OpenAi {
                reasoning_effort: effort,
            },
            (ApiType::OpenRouter, None, None) => {
                jutella::ApiOptions::OpenRouter { reasoning: None }
            }
            (ApiType::OpenRouter, Some(effort), None) => jutella::ApiOptions::OpenRouter {
                reasoning: Some(jutella::ReasoningSettings::Effort(effort)),
            },
            (ApiType::OpenRouter, None, Some(budget)) => jutella::ApiOptions::OpenRouter {
                reasoning: Some(jutella::ReasoningSettings::Budget(budget)),
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
            verbosity,
            min_history_tokens,
            max_history_tokens,
        })
    }
}
