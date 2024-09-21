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

use crate::types::Message;
use anyhow::anyhow;
use std::collections::HashSet;
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

#[derive(Debug)]
pub struct Config {
    pub auth_jid: BareJid,
    pub auth_password: String,
    pub allowed_users: Vec<String>,
    pub request_tx: Sender<Message>,
    pub response_rx: Receiver<Message>,
}

/// XMPP agent
pub struct Xmpp {
    client: Agent<ServerConfig>,
    request_tx: Sender<Message>,
    response_rx: Receiver<Message>,
    allowed_users: HashSet<String>,
    clogged_fired: bool,
}

impl Xmpp {
    pub fn new(config: Config) -> Self {
        let Config {
            auth_jid,
            auth_password,
            allowed_users,
            request_tx,
            response_rx,
        } = config;

        let client = ClientBuilder::new(auth_jid, &auth_password)
            .set_client(ClientType::Bot, "jutella-xmpp")
            .build();

        let allowed_users = allowed_users.into_iter().collect();

        Self {
            client,
            request_tx,
            response_rx,
            allowed_users,
            clogged_fired: false,
        }
    }

    async fn process_response(&mut self, message: Message) {
        let Message {
            jid,
            message,
        } = message;

        tracing::trace!(target: LOG_TARGET, "processing response from {jid}");

        self.client
            .send_message(jid.into(), MessageType::Chat, LANG, &message)
            .await;
    }

    fn process_xmpp_event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Online => {
                tracing::info!(target: LOG_TARGET, "Connected to XMPP server");
            }
            Event::Disconnected(error) => {
                tracing::warn!(target: LOG_TARGET, "Disconnected from XMPP server: {error}");
            }
            Event::ChatMessage(_id, jid, body, _) => {
                let message = body.0;
                if self.allowed_users.contains(jid.as_str()) {
                    tracing::debug!(target: LOG_TARGET, "Message received from allowed JID {jid}");
                    if let Err(e) = self.request_tx.try_send(Message {
                        jid,
                        message,
                    }) {
                        match e {
                            TrySendError::Full(_) => {
                                if !self.clogged_fired {
                                    tracing::error!(target: LOG_TARGET, "requests channel clogged");
                                    self.clogged_fired = true;
                                }
                            }
                            TrySendError::Closed(_) => {
                                return Err(anyhow!("requests channel closed, terminating XMPP agent"))
                            }
                        }
                    }
                } else {
                    tracing::trace!(target: LOG_TARGET, "Message received from unknown JID {jid}");
                }
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
                        return Err(anyhow!("XMPP event stream was closed, terminating XMPP agent"))
                    }
                }
                message = self.response_rx.recv() => {
                    if let Some(message) = message {
                        self.process_response(message).await;
                    } else {
                        return Err(
                            anyhow!("Message stream from chatbot was closed, terminating XMPP agent")
                        )
                    }
                }
            }
        }
    }
}

