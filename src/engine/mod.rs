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

//! Chatbot Engine.

mod handler;

use crate::{
    engine::handler::{ChatbotHandler, ChatbotHandlerConfig},
    message::Message,
};
use anyhow::Context as _;
use futures::{
    future::{BoxFuture, FutureExt},
    stream::{FuturesUnordered, StreamExt},
};
use reqwest::ClientBuilder;
use std::{collections::HashMap, time::Duration};
use tokio::sync::mpsc::{channel, Sender};

// Log target for this file.
const LOG_TARGET: &str = "jutella::engine";

// If we have 100 pending messages from user, something is extremely odd.
pub const REQUESTS_CHANNEL_SIZE: usize = 100;

// HTTP request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Configuration for [`Jutella`].
#[derive(Debug)]
pub struct Config {
    pub api_url: String,
    pub api_version: Option<String>,
    pub api_auth: jutella::Auth,
    pub model: String,
    pub system_message: Option<String>,
    pub max_history_tokens: Option<usize>,
    pub allowed_users: Vec<String>,
    pub response_tx: Sender<Message>,
}

pub struct ChatbotEngine {
    handlers_futures: FuturesUnordered<BoxFuture<'static, anyhow::Result<()>>>,
}

impl ChatbotEngine {
    pub fn new(config: Config) -> anyhow::Result<(Self, HashMap<String, Sender<Message>>)> {
        let Config {
            api_url,
            api_version,
            api_auth,
            model,
            system_message,
            max_history_tokens,
            allowed_users,
            response_tx,
        } = config;

        let reqwest_client = ClientBuilder::new()
            .default_headers(api_auth.try_into()?)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("Failed to initialize HTTP client")?;

        let handlers = allowed_users.into_iter().map(|jid| {
            // Channel `engine` -> `handler`
            let (request_tx, request_rx) = channel(REQUESTS_CHANNEL_SIZE);

            let handler_config = ChatbotHandlerConfig {
                jid: jid.clone(),
                api_url: api_url.clone(),
                api_version: api_version.clone(),
                model: model.clone(),
                system_message: system_message.clone(),
                max_history_tokens,
                reqwest_client: reqwest_client.clone(),
                request_rx,
                response_tx: response_tx.clone(),
            };

            let handler = ChatbotHandler::new(handler_config);

            ((jid, request_tx), handler.run().boxed())
        });

        let (handlers_txs, futures): (Vec<_>, Vec<_>) = handlers.unzip();

        let handlers_tx_map = handlers_txs.into_iter().collect();
        let handlers_futures = futures.into_iter().collect();

        Ok((Self { handlers_futures }, handlers_tx_map))
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        self.handlers_futures
            .select_next_some()
            .await
            .inspect_err(|error| {
                tracing::error!(
                    target: LOG_TARGET,
                    "terminating engine, one of the chat handlers terminated \
                     with error: {error}",
                );
            })
    }
}
