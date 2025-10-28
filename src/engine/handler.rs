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

//! Chatbot chat handler.

use crate::message::{RequestMessage, ResponseMessage};
use anyhow::anyhow;
use jutella::{ApiOptions, Auth, ChatClient, ChatClientConfig, Completion, TokenUsage};
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc::{error::TrySendError, Receiver, Sender};

// Log target for this file.
const LOG_TARGET: &str = "jutella::handler";

/// Configuration of [`ChatbotHandler`]
// Can't implement `Debug` due to `tiktoken_rs::CoreBPE` not implementing it.
pub struct ChatbotHandlerConfig {
    pub jid: String,
    pub api_url: String,
    pub api_options: ApiOptions,
    pub api_version: Option<String>,
    pub auth: Auth,
    pub http_timeout: Duration,
    pub model: String,
    pub system_message: Option<String>,
    pub verbosity: Option<String>,
    pub min_history_tokens: Option<usize>,
    pub max_history_tokens: usize,
    pub reqwest_client: reqwest::Client,
    pub tokenizer: Arc<tiktoken_rs::CoreBPE>,
    pub response_tx: Sender<ResponseMessage>,
    pub request_rx: Receiver<RequestMessage>,
}

/// Single chatbot conversation handler.
pub struct ChatbotHandler {
    jid: String,
    client: ChatClient,
    response_tx: Sender<ResponseMessage>,
    request_rx: Receiver<RequestMessage>,
    clogged: bool,
}

impl ChatbotHandler {
    pub fn new(config: ChatbotHandlerConfig) -> Result<Self, jutella::Error> {
        let ChatbotHandlerConfig {
            jid,
            api_url,
            api_options,
            api_version,
            auth,
            http_timeout,
            model,
            system_message,
            verbosity,
            min_history_tokens,
            max_history_tokens,
            reqwest_client,
            tokenizer,
            response_tx,
            request_rx,
        } = config;

        let client = ChatClient::new_with_client_and_tokenizer(
            ChatClientConfig {
                api_url,
                api_options,
                api_version,
                auth,
                http_timeout,
                model,
                system_message,
                verbosity,
                min_history_tokens,
                max_history_tokens: Some(max_history_tokens),
            },
            reqwest_client,
            tokenizer,
        )?;

        Ok(Self {
            jid,
            client,
            response_tx,
            request_rx,
            clogged: false,
        })
    }

    async fn handle_request(&mut self, req: RequestMessage) -> anyhow::Result<()> {
        let RequestMessage { jid, request } = req;

        if jid != self.jid {
            tracing::error!(
                target: LOG_TARGET,
                jid,
                jid_config = self.jid,
                "received jid does not match configured jid, this is a bug",
            );
            debug_assert!(false);
            return Err(anyhow!("jid mismatch in request handler"));
        }

        let Completion {
            response,
            reasoning: _,
            token_usage:
                TokenUsage {
                    tokens_in,
                    tokens_in_cached,
                    tokens_out,
                    tokens_reasoning,
                },
        } = self
            .client
            .request_completion(request)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(target: LOG_TARGET, jid, "error from chatbot API: {error}");

                Completion {
                    response: format!("[ERROR] {error}"),
                    reasoning: None,
                    // TODO: return real token count once `jutella` supports it in errors.
                    token_usage: TokenUsage {
                        tokens_in: 0,
                        tokens_in_cached: None,
                        tokens_out: 0,
                        tokens_reasoning: None,
                    },
                }
            });

        if let Err(e) = self.response_tx.try_send(ResponseMessage {
            jid: jid.clone(),
            response,
            tokens_in,
            tokens_in_cached,
            tokens_out,
            tokens_reasoning,
        }) {
            match e {
                TrySendError::Closed(_) => return Err(anyhow!("responses channel closed")),
                TrySendError::Full(_) => {
                    if !self.clogged {
                        self.clogged = true;
                        tracing::error!(
                            target: LOG_TARGET,
                            jid,
                            size = crate::xmpp::RESPONSES_CHANNEL_SIZE,
                            "responses channel clogged",
                        );
                    }
                }
            }
        }

        Ok(())
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        loop {
            if let Some(req) = self.request_rx.recv().await {
                self.handle_request(req).await?;
            } else {
                return Ok(());
            }
        }
    }
}
