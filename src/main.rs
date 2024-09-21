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
mod jutella;
mod types;
mod xmpp;

use crate::{
    config::Config,
    xmpp::{Config as XmppConfig, Xmpp},
    jutella::{Config as JutellaConfig, Jutella},
};
use anyhow::{anyhow, Context as _};
use tokio::sync::mpsc::channel;
use tracing_log::LogTracer;
use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*, EnvFilter};

// That many messages mean something is not OK.
const CHANNEL_SIZE: usize = 1024;

// Disable noisy log targets.
const LOG_FILTER_DERICTIVE: &str = "xmpp::disco=warn";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    setup_logging()?;

    // For some reason `rustls` per-process default crypto provider must be set for `xmpp` to work.
    install_crypto_provider()?;

    let Config {
        auth_jid,
        auth_password,
        allowed_users,
        api_url,
        api_key,
        model,
        system_message,
    } = Config::load().context("Failed to load config")?;

    let (request_tx, request_rx) = channel(CHANNEL_SIZE);
    let (response_tx, response_rx) = channel(CHANNEL_SIZE);

    let xmpp = Xmpp::new(XmppConfig {
        auth_jid,
        auth_password,
        allowed_users: allowed_users.clone(),
        request_tx,
        response_rx,
    });

    let jutella = Jutella::new(JutellaConfig{
        api_url,
        api_key,
        model,
        system_message,
        allowed_users,
        request_rx,
        response_tx,
    });

    loop {
        tokio::select! {
            result = xmpp.run() => {
                return result.context("XMPP agent terminated");
            }
            result = jutella.run() => {
                return result.context("chatbot client terminated");
            }
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
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .map_err(|_| anyhow!("Failed to install default `rustls` crypto provider"))
}
