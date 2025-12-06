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

//! Chatbot Engine.

mod chat;

use crate::{
    engine::chat::{Chat, ChatConfig},
    xmpp::XmppHandle,
};
use futures::{
    future::{BoxFuture, FutureExt},
    stream::{FuturesUnordered, StreamExt},
};
use serde_json::value::Value;
use std::time::Duration;
use tokio::sync::mpsc::Receiver;

// Log target for this file.
const LOG_TARGET: &str = "jutella::engine";

/// Configuration for [`ChatEngine`].
#[derive(Debug, Clone)]
pub struct Config {
    pub api_url: String,
    pub api_options: jutella::ApiOptions,
    pub api_version: Option<String>,
    pub api_auth: jutella::Auth,
    pub http_timeout: Duration,
    pub model: String,
    pub system_message: Option<String>,
    pub system_message_tokens: usize,
    pub verbosity: Option<String>,
    pub sanitize_links: bool,
    pub min_history_tokens: Option<usize>,
    pub max_history_tokens: usize,
    pub extra_params: Option<serde_json::map::Map<String, Value>>,
}

pub struct ChatEngine {
    config: Config,
    reqwest_client: reqwest::Client,
    new_chats_rx: Receiver<XmppHandle>,
    chat_futures: FuturesUnordered<BoxFuture<'static, anyhow::Result<()>>>,
}

impl ChatEngine {
    pub fn new(config: Config, new_chats_rx: Receiver<XmppHandle>) -> anyhow::Result<Self> {
        let reqwest_client = reqwest::Client::new();

        Ok(Self {
            config,
            reqwest_client,
            new_chats_rx,
            chat_futures: FuturesUnordered::new(),
        })
    }

    fn handle_new_chat(&mut self, new_chat: XmppHandle) {
        let jid = new_chat.jid().as_str().to_owned();

        match create_chat_handler(self.config.clone(), self.reqwest_client.clone(), new_chat) {
            Ok(handler) => {
                tracing::info!(
                    target: LOG_TARGET,
                    jid,
                    "initialized chat instance",
                );

                self.chat_futures.push(handler.run().boxed());
            }
            Err(error) => {
                tracing::error!(
                    target: LOG_TARGET,
                    jid,
                    ?error,
                    "failed to create chat instance"
                );
            }
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                Err(e) = self.chat_futures.select_next_some(),
                    if !self.chat_futures.is_empty() =>
                {
                    tracing::error!(
                        target: LOG_TARGET,
                        "terminating engine, one of the chat handlers terminated \
                        with error: {e}",
                    );
                    return Err(e)
                },
                new_chat = self.new_chats_rx.recv() => {
                    if let Some(new_chat) = new_chat {
                        self.handle_new_chat(new_chat);
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

fn create_chat_handler(
    Config {
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
    }: Config,
    reqwest_client: reqwest::Client,
    xmpp_handle: XmppHandle,
) -> Result<Chat, jutella::Error> {
    Chat::new(ChatConfig {
        api_url,
        api_options,
        api_version,
        auth: api_auth,
        http_timeout,
        model,
        system_message,
        system_message_tokens,
        verbosity,
        sanitize_links,
        min_history_tokens,
        max_history_tokens,
        reqwest_client,
        xmpp_handle,
        extra_params,
    })
}
