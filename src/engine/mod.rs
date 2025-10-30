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
use futures::{
    future::{BoxFuture, FutureExt},
    stream::{FuturesUnordered, StreamExt},
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::mpsc::{channel, error::TrySendError, Receiver, Sender};

// Log target for this file.
const LOG_TARGET: &str = "jutella::engine";

// If we have 100 pending messages from user, something is extremely odd.
pub const REQUESTS_CHANNEL_SIZE: usize = 10;

/// Configuration for [`Jutella`].
#[derive(Debug, Clone)]
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
}

pub struct ChatbotEngine {
    config: Config,
    reqwest_client: reqwest::Client,
    tokenizer: Arc<tiktoken_rs::CoreBPE>,
    request_rx: Receiver<RequestMessage>,
    response_tx: Sender<ResponseMessage>,
    handlers_futures: FuturesUnordered<BoxFuture<'static, anyhow::Result<()>>>,
    request_txs: HashMap<String, Sender<RequestMessage>>,
}

impl ChatbotEngine {
    pub fn new(
        config: Config,
        request_rx: Receiver<RequestMessage>,
        response_tx: Sender<ResponseMessage>,
    ) -> anyhow::Result<Self> {
        let reqwest_client = reqwest::Client::new();
        let tokenizer = Arc::new(tiktoken_rs::o200k_base()?);

        Ok(Self {
            config,
            reqwest_client,
            tokenizer,
            request_rx,
            response_tx,
            handlers_futures: FuturesUnordered::new(),
            request_txs: HashMap::new(),
        })
    }

    fn handle_request(&mut self, request: RequestMessage) {
        let request_tx = match self.request_txs.get(&request.jid) {
            Some(request_tx) => request_tx,
            None => {
                match create_handler(
                    self.config.clone(),
                    request.jid.clone(),
                    self.reqwest_client.clone(),
                    self.tokenizer.clone(),
                    self.response_tx.clone(),
                ) {
                    Ok((handler, request_tx)) => {
                        tracing::info!(
                            target: LOG_TARGET,
                            jid = request.jid,
                            "initialized chat instance",
                        );

                        self.handlers_futures.push(handler.run().boxed());
                        self.request_txs.insert(request.jid.clone(), request_tx);
                        self.request_txs
                            .get(&request.jid)
                            .expect("request_tx inserted above")
                    }
                    Err(error) => {
                        tracing::error!(
                            target: LOG_TARGET,
                            jid = request.jid,
                            ?error,
                            "failed to create chat instance"
                        );

                        return;
                    }
                }
            }
        };

        let jid = request.jid.clone();

        match request_tx.try_send(request) {
            Ok(()) => (),
            Err(TrySendError::Full(_)) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    jid,
                    size = REQUESTS_CHANNEL_SIZE,
                    "chat instance requests channel clogged",
                );
            }
            Err(TrySendError::Closed(_)) => {
                // This should never happen.
                tracing::error!(
                    target: LOG_TARGET,
                    jid,
                    "chat instance requests channel closed. this is a bug",
                );
            }
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                Err(e) = self.handlers_futures.select_next_some(),
                    if !self.handlers_futures.is_empty() =>
                {
                    tracing::error!(
                        target: LOG_TARGET,
                        "terminating engine, one of the chat handlers terminated \
                        with error: {e}",
                    );
                    return Err(e)
                },
                request = self.request_rx.recv() => {
                    if let Some(request) = request {
                        self.handle_request(request);
                    } else {
                        tracing::debug!(
                            target: LOG_TARGET,
                            "request channel terminated, terminating ChatbotEngine",
                        );
                        return Ok(())
                    }
                }
            }
        }
    }
}

fn create_handler(
    Config {
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
    }: Config,
    jid: String,
    reqwest_client: reqwest::Client,
    tokenizer: Arc<tiktoken_rs::CoreBPE>,
    response_tx: Sender<ResponseMessage>,
) -> Result<(ChatbotHandler, Sender<RequestMessage>), jutella::Error> {
    let (request_tx, request_rx) = channel(REQUESTS_CHANNEL_SIZE);

    let handler = ChatbotHandler::new(ChatbotHandlerConfig {
        jid,
        api_url,
        api_options,
        api_version,
        auth: api_auth,
        http_timeout,
        model,
        system_message,
        verbosity,
        min_history_tokens,
        max_history_tokens,
        reqwest_client,
        tokenizer,
        request_rx,
        response_tx,
    })?;

    Ok((handler, request_tx))
}
