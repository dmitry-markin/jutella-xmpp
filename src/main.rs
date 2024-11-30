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

//! `jutella-xmpp`: XMPP – OpenAI API bridge.

mod config;
mod engine;
mod message;
mod xmpp;

use crate::{
    config::Config,
    engine::{ChatbotEngine, Config as ChatbotEngineConfig},
    xmpp::{Config as XmppConfig, Xmpp, RESPONSES_CHANNEL_SIZE},
};
use anyhow::{anyhow, Context as _};
use tokio::sync::mpsc::channel;
use tracing_log::LogTracer;
use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*, EnvFilter};

// Log target for this file.
const LOG_TARGET: &str = "jutella";

// Disable noisy log targets.
const LOG_FILTER_DERICTIVE: &str = "xmpp::disco=warn";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_logging()?;

    install_crypto_provider()?;

    let Config {
        auth_jid,
        auth_password,
        allowed_users,
        api_url,
        api_version,
        api_auth,
        model,
        system_message,
        min_history_tokens,
        max_history_tokens,
    } = Config::load().context("Failed to load config")?;

    tracing::debug!(
        target: LOG_TARGET,
        api_url,
        api_version,
        model,
        min_history_tokens,
        max_history_tokens,
        "configuration",
    );

    let (response_tx, response_rx) = channel(RESPONSES_CHANNEL_SIZE);

    let (chatbot_engine, request_txs_map) = ChatbotEngine::new(ChatbotEngineConfig {
        api_url,
        api_version,
        api_auth,
        model,
        system_message,
        min_history_tokens,
        max_history_tokens,
        allowed_users,
        response_tx,
    })
    .context("Failed to initialize chatbot engine")?;

    let xmpp = Xmpp::new(XmppConfig {
        auth_jid,
        auth_password,
        request_txs_map,
        response_rx,
    });

    tokio::select! {
        result = xmpp.run() => {
            return result.context("XMPP agent terminated")
        }
        result = chatbot_engine.run() => {
            return result.context("chatbot engine terminated")
        }
    }
}

fn setup_logging() -> anyhow::Result<()> {
    LogTracer::init().context("Failed to initialize `log` tracer")?;

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env()
        .context("Failed to parse env log filter")?
        .add_directive(
            LOG_FILTER_DERICTIVE
                .parse()
                .context("setting custom log filter directive failed")?,
        );

    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer());

    tracing::subscriber::set_global_default(subscriber)
        .context("Failed to set global tracing subscriber")
}

fn install_crypto_provider() -> anyhow::Result<()> {
    // For some reason `rustls` per-process default crypto provider must be set for `xmpp` to work.
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .map_err(|_| anyhow!("Failed to install default `rustls` crypto provider"))
}
