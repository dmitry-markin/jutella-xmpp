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

use crate::message::Message;
use anyhow::anyhow;
use jutella::{ChatClient, ChatClientConfig};
use tokio::sync::mpsc::{error::TrySendError, Receiver, Sender};

// Log target for this file.
const LOG_TARGET: &str = "jutella::handler";

/// Configuration of [`ChatbotHandler`]
#[derive(Debug)]
pub struct ChatbotHandlerConfig {
    pub jid: String,
    pub api_url: String,
    pub api_version: Option<String>,
    pub model: String,
    pub system_message: Option<String>,
    pub max_history_tokens: usize,
    pub reqwest_client: reqwest::Client,
    pub response_tx: Sender<Message>,
    pub request_rx: Receiver<Message>,
}

/// Single chatbot conversation handler.
pub struct ChatbotHandler {
    jid: String,
    client: ChatClient,
    response_tx: Sender<Message>,
    request_rx: Receiver<Message>,
    clogged: bool,
}

impl ChatbotHandler {
    pub fn new(config: ChatbotHandlerConfig) -> Result<Self, jutella::Error> {
        let ChatbotHandlerConfig {
            jid,
            api_url,
            api_version,
            model,
            system_message,
            max_history_tokens,
            reqwest_client,
            response_tx,
            request_rx,
        } = config;

        let client = ChatClient::new_with_client(
            reqwest_client,
            ChatClientConfig {
                api_url,
                api_version,
                model,
                system_message,
                max_history_tokens: Some(max_history_tokens),
            },
        )?;

        Ok(Self {
            jid,
            client,
            response_tx,
            request_rx,
            clogged: false,
        })
    }

    async fn handle_message(&mut self, message: Message) -> anyhow::Result<()> {
        let Message { jid, message } = message;

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

        let message = match self.client.ask(message).await {
            Ok(reply) => reply,
            Err(error) => {
                tracing::warn!(target: LOG_TARGET, jid, "error from chatbot API: {error}");

                format!("[ERROR] {error}")
            }
        };

        if let Err(e) = self.response_tx.try_send(Message {
            jid: jid.clone(),
            message,
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
            if let Some(message) = self.request_rx.recv().await {
                self.handle_message(message).await?;
            } else {
                return Ok(());
            }
        }
    }
}
