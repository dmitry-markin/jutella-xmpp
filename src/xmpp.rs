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

//! XMPP agent.

use crate::message::Message;
use anyhow::anyhow;
use std::collections::HashMap;
use tokio::sync::mpsc::{Receiver, Sender, error::TrySendError};
use tokio_xmpp::starttls::ServerConfig;
use xmpp::{
    agent::{Agent, BareJid},
    ClientBuilder, ClientType, Event,
};
use xmpp_parsers::message::MessageType;

// It is OK to not set language.
const LANG: &str = "";

// Log target for this file.
const LOG_TARGET: &str = "jutella::xmpp";

// Responses channel size.
pub const RESPONSES_CHANNEL_SIZE: usize = 1024;

#[derive(Debug)]
pub struct Config {
    pub auth_jid: BareJid,
    pub auth_password: String,
    pub request_txs_map: HashMap<String, Sender<Message>>,
    pub response_rx: Receiver<Message>,
}

/// XMPP agent
pub struct Xmpp {
    client: Agent<ServerConfig>,
    request_txs_map: HashMap<String, Sender<Message>>,
    response_rx: Receiver<Message>,
    clogged_engine: bool,
}

impl Xmpp {
    pub fn new(config: Config) -> Self {
        let Config {
            auth_jid,
            auth_password,
            request_txs_map,
            response_rx,
        } = config;

        let client = ClientBuilder::new(auth_jid, &auth_password)
            .set_client(ClientType::Bot, "jutella-xmpp")
            .build();

        Self {
            client,
            request_txs_map,
            response_rx,
            clogged_engine: false,
        }
    }

    fn process_request(&mut self, message: Message) -> anyhow::Result<()> {
        let jid = message.jid.clone();

        let Some(request_tx) = self.request_txs_map.get(&jid) else {
            tracing::trace!(target: LOG_TARGET, jid, "message received from unknown user");
            return Ok(())
        };

        tracing::debug!(target: LOG_TARGET, jid, "received request");

        if let Err(e) = request_tx.try_send(message) {
            match e {
                TrySendError::Full(_) => {
                    if !self.clogged_engine {
                        self.clogged_engine = true;
                        tracing::error!(
                            target: LOG_TARGET,
                            size = crate::engine::REQUESTS_CHANNEL_SIZE,
                            "requests channel clogged",
                        );
                    }
                }
                TrySendError::Closed(_) => {
                    return Err(anyhow!("requests channel closed, terminating"))
                }
            }
        }

        Ok(())
    }

    async fn process_response(&mut self, message: Message) {
        let Message {
            jid,
            message,
        } = message;

        tracing::debug!(target: LOG_TARGET, jid, "sending response");

        let Ok(jid) = BareJid::new(&jid) else {
            // This must not happen as jids were checked to compare equal to string representation
            // of allowed users when receiving request.
            tracing::error!(target: LOG_TARGET, jid, "failed to convert to `BareJid`");
            debug_assert!(false);
            return
        };

        self.client
            .send_message(jid.into(), MessageType::Chat, LANG, &message)
            .await;
    }

    fn process_xmpp_event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Online => {
                tracing::info!(target: LOG_TARGET, "connected to XMPP server");
            }
            Event::Disconnected(error) => {
                tracing::warn!(target: LOG_TARGET, "disconnected from XMPP server: {error}");
            }
            Event::ChatMessage(_id, jid, body, _) => {
                let message = body.0;
                let jid = jid.as_str().to_owned();
                self.process_request(Message { jid, message })?;
            }
            _ => {}
        }

        Ok(())
    }

    pub async fn run(mut self) -> anyhow::Result<()>{
        loop {
            tokio::select! {
                events = self.client.wait_for_events() => {
                    if let Some(events) = events {
                        for event in events {
                            self.process_xmpp_event(event)?;
                        }
                    } else {
                        return Err(anyhow!("XMPP event stream was closed, terminating"))
                    }
                }
                message = self.response_rx.recv() => {
                    if let Some(message) = message {
                        self.process_response(message).await;
                    } else {
                        return Ok(())
                    }
                }
            }
        }
    }
}

