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
use futures::stream::StreamExt;
use std::{collections::HashMap, time::Duration};
use tokio::{
    sync::mpsc::{error::TrySendError, Receiver, Sender},
    time::MissedTickBehavior,
};
use tokio_xmpp::{starttls::ServerConfig, AsyncClient as XmppClient, Event};
use xmpp_parsers::{
    jid::BareJid,
    message::{Body, Message as XmppMessage, MessageType},
    presence::{Presence, Show as PresenceShow},
};

// Log target for this file.
const LOG_TARGET: &str = "jutella::xmpp";

// Responses channel size.
pub const RESPONSES_CHANNEL_SIZE: usize = 1024;

// Period to send presence with.
const PRESENSE_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub struct Config {
    pub auth_jid: BareJid,
    pub auth_password: String,
    pub request_txs_map: HashMap<String, Sender<Message>>,
    pub response_rx: Receiver<Message>,
}

/// XMPP agent
pub struct Xmpp {
    client: XmppClient<ServerConfig>,
    request_txs_map: HashMap<String, Sender<Message>>,
    response_rx: Receiver<Message>,
    online: bool,
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

        let mut client = XmppClient::new(auth_jid, auth_password);
        client.set_reconnect(true);

        Self {
            client,
            request_txs_map,
            response_rx,
            online: false,
            clogged_engine: false,
        }
    }

    async fn process_response(&mut self, message: Message) {
        let Message { jid, message } = message;

        tracing::debug!(target: LOG_TARGET, jid, "sending response");

        let Ok(bare_jid) = BareJid::new(&jid) else {
            // This must not happen as jids were checked to compare equal to string representation
            // of allowed users when receiving request.
            tracing::error!(target: LOG_TARGET, jid, "failed to convert to `BareJid`");
            debug_assert!(false);
            return;
        };

        let mut xmpp_message = XmppMessage::new(Some(bare_jid.into()));
        xmpp_message.bodies.insert(String::new(), Body(message));

        if let Err(error) = self.client.send_stanza(xmpp_message.into()).await {
            tracing::error!(target: LOG_TARGET, jid, ?error, "failed to send xmpp message");
        }
    }

    fn process_xmpp_message(&mut self, message: XmppMessage) -> anyhow::Result<()> {

        let Some(ref jid) = message.from else {
            tracing::trace!(target: LOG_TARGET, ?message, "xmpp message without `from` field");
            return Ok(());
        };

        let jid = jid.to_bare().as_str().to_owned();

        let Some(request_tx) = self.request_txs_map.get(&jid) else {
            tracing::trace!(target: LOG_TARGET, jid, ?message, "message from unknown user");
            return Ok(());
        };

        if message.type_ != MessageType::Chat {
            tracing::warn!(
                target: LOG_TARGET,
                jid,
                type_ = ?message.type_,
                ?message,
                "not a chat message received",
            );
            return Ok(());
        }

        let Some(body) = message.bodies.get("") else {
            tracing::trace!(target: LOG_TARGET, jid, ?message, "chat message without a body");
            return Ok(());
        };

        tracing::debug!(target: LOG_TARGET, jid, "received request");

        let message = Message {
            jid: jid.clone(),
            message: body.0.clone(),
        };

        if let Err(e) = request_tx.try_send(message) {
            match e {
                TrySendError::Full(_) => {
                    if !self.clogged_engine {
                        self.clogged_engine = true;
                        tracing::error!(
                            target: LOG_TARGET,
                            jid,
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

    async fn process_xmpp_event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Online { .. } => {
                tracing::info!(target: LOG_TARGET, "connected to XMPP server");
                self.online = true;
                self.send_presence().await;
            }
            Event::Disconnected(error) => {
                tracing::error!(target: LOG_TARGET, ?error, "disconnected from XMPP server");
                self.online = false;
            }
            Event::Stanza(stanza) => {
                if let Ok(message) = XmppMessage::try_from(stanza) {
                    self.process_xmpp_message(message)?;
                }
            }
        }

        Ok(())
    }

    async fn send_presence(&mut self) {
        tracing::trace!(target: LOG_TARGET, "sending presence");

        let presence = Presence::available().with_show(PresenceShow::Chat);

        if let Err(error) = self.client.send_stanza(presence.into()).await {
            tracing::error!(target: LOG_TARGET, ?error, "failed to send presence");
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let mut presence_tick = tokio::time::interval(PRESENSE_INTERVAL);
        presence_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                event = self.client.next() => {
                    if let Some(event) = event {
                        self.process_xmpp_event(event).await?;
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
                _ = presence_tick.tick() => {
                    if self.online {
                        // This makes sure we detect dropped TCP stream and reconnect.
                        self.send_presence().await;
                    }
                }
            }
        }
    }
}
