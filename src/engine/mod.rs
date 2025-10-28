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
    message::{RequestMessage, ResponseMessage},
};
use anyhow::Context as _;
use futures::{
    future::{BoxFuture, FutureExt},
    stream::{FuturesUnordered, StreamExt},
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::mpsc::{channel, Sender};

// Log target for this file.
const LOG_TARGET: &str = "jutella::engine";

// If we have 100 pending messages from user, something is extremely odd.
pub const REQUESTS_CHANNEL_SIZE: usize = 100;

/// Configuration for [`Jutella`].
#[derive(Debug)]
pub struct Config {
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
    pub allowed_users: Vec<String>,
    pub response_tx: Sender<ResponseMessage>,
}

pub struct ChatbotEngine {
    handlers_futures: FuturesUnordered<BoxFuture<'static, anyhow::Result<()>>>,
}

impl ChatbotEngine {
    pub fn new(config: Config) -> anyhow::Result<(Self, HashMap<String, Sender<RequestMessage>>)> {
        let Config {
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
            allowed_users,
            response_tx,
        } = config;

        let reqwest_client = reqwest::Client::new();
        let tokenizer = Arc::new(tiktoken_rs::o200k_base()?);

        let handlers = allowed_users.into_iter().map(|jid| {
            // Channel `engine` -> `handler`
            let (request_tx, request_rx) = channel(REQUESTS_CHANNEL_SIZE);

            let handler_config = ChatbotHandlerConfig {
                jid: jid.clone(),
                api_url: api_url.clone(),
                api_options: api_options.clone(),
                api_version: api_version.clone(),
                auth: api_auth.clone(),
                http_timeout,
                model: model.clone(),
                system_message: system_message.clone(),
                verbosity: verbosity.clone(),
                min_history_tokens,
                max_history_tokens,
                reqwest_client: reqwest_client.clone(),
                tokenizer: tokenizer.clone(),
                request_rx,
                response_tx: response_tx.clone(),
            };

            let handler = ChatbotHandler::new(handler_config);

            ((jid, request_tx), handler.map(|h| h.run().boxed()))
        });

        let (handlers_txs, futures): (Vec<_>, Vec<_>) = handlers.unzip();

        let handlers_tx_map = handlers_txs.into_iter().collect();
        let handlers_futures = futures
            .into_iter()
            .collect::<Result<_, _>>()
            .context("Failed to initialize `ChatClient`")?;

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
