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

use crate::message::{RequestMessage, ResponseMessage};
use anyhow::anyhow;
use futures::{
    stream::{BoxStream, StreamExt},
    FutureExt,
};
use std::{collections::HashMap, time::Duration};
use tokio::{
    sync::mpsc::{error::TrySendError, Receiver, Sender},
    time::MissedTickBehavior,
};
use tokio_stream::StreamMap;
use tokio_xmpp::{starttls::ServerConfig, AsyncClient as XmppClient, Event};
use xmpp_parsers::{
    jid::BareJid,
    message::{Message as XmppMessage, MessageType},
    minidom::Element,
    presence::{Presence, Show as PresenceShow},
};

// Log target for this file.
const LOG_TARGET: &str = "jutella::xmpp";

// Delay before reconnecting to XMPP server. Built-in `tokio_xmpp` reconnect is too agressive
// and wastes up to 50% of a CPU core by reconnecting without a delay.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

// Responses channel size.
pub const RESPONSES_CHANNEL_SIZE: usize = 1024;

// Period to send presence with.
const PRESENSE_INTERVAL: Duration = Duration::from_secs(60);

// Delay before sending back a composing notification.
const COMPOSING_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct Config {
    pub auth_jid: BareJid,
    pub auth_password: String,
    pub request_txs_map: HashMap<String, Sender<RequestMessage>>,
    pub response_rx: Receiver<ResponseMessage>,
}

/// XMPP agent
pub struct Xmpp {
    auth_jid: BareJid,
    auth_password: String,
    client: XmppClient<ServerConfig>,
    request_txs_map: HashMap<String, Sender<RequestMessage>>,
    response_rx: Receiver<ResponseMessage>,
    pending_composing: StreamMap<BareJid, BoxStream<'static, ()>>,
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

        let client = XmppClient::new(auth_jid.clone(), auth_password.clone());

        Self {
            auth_jid,
            auth_password,
            client,
            request_txs_map,
            response_rx,
            pending_composing: StreamMap::new(),
            online: false,
            clogged_engine: false,
        }
    }

    fn reconnect(&mut self) {
        self.client = XmppClient::new(self.auth_jid.clone(), self.auth_password.clone());
    }

    async fn send_xmpp_message(&mut self, bare_jid: BareJid, message: String) {
        let jid = bare_jid.as_str().to_owned();
        let xmpp_message =
            XmppMessage::new(Some(bare_jid.into())).with_body(String::new(), message);

        self.client
            .send_stanza(xmpp_message.into())
            .await
            .inspect_err(|error| {
                tracing::error!(target: LOG_TARGET, jid, ?error, "failed to send xmpp message");
            })
            .unwrap_or_default();
    }

    async fn process_response(&mut self, resp: ResponseMessage) {
        let ResponseMessage {
            jid,
            response,
            tokens_in,
            tokens_out,
        } = resp;

        tracing::debug!(
            target: LOG_TARGET,
            jid,
            len = response.len(),
            tokens_in,
            tokens_out,
            "response"
        );

        let Ok(bare_jid) = BareJid::new(&jid) else {
            // This must not happen as jids were checked to compare equal to string representation
            // of allowed users when receiving request.
            tracing::error!(target: LOG_TARGET, jid, "failed to convert to `BareJid`; this is a bug");
            debug_assert!(false);
            return;
        };

        self.pending_composing.remove(&bare_jid);
        self.send_chat_state_active(bare_jid.clone()).await;
        self.send_xmpp_message(bare_jid, response).await;
    }

    async fn process_xmpp_message(&mut self, message: XmppMessage) -> anyhow::Result<()> {
        let Some(ref jid) = message.from else {
            tracing::trace!(target: LOG_TARGET, ?message, "xmpp message without `from` field");
            return Ok(());
        };

        let bare_jid = jid.to_bare();
        let jid = bare_jid.as_str().to_owned();

        if !self.request_txs_map.contains_key(&jid) {
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

        if message.payloads.iter().any(|p| p.name() == "encrypted") {
            tracing::debug!(target: LOG_TARGET, jid, "encrypted message");
            self.send_xmpp_message(
                bare_jid.clone(),
                "[ERROR] Encrypted messages are not supported".to_string(),
            )
            .await;
            return Ok(());
        }

        let req = RequestMessage {
            jid: jid.clone(),
            request: body.0.clone(),
        };

        tracing::debug!(target: LOG_TARGET, jid, len = req.request.len(), "request");

        let request_tx = self
            .request_txs_map
            .get(&jid)
            .expect("was checked above to contain jid; qed");

        match request_tx.try_send(req) {
            Ok(()) => {
                self.schedule_pending_composing(bare_jid.clone());

                if let Some(id) = message.id {
                    self.send_displayed_marker(bare_jid, &id).await;
                }
            }
            Err(e) => match e {
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
            },
        }

        Ok(())
    }

    async fn send_displayed_marker(&mut self, bare_jid: BareJid, id: &str) {
        tracing::trace!(target: LOG_TARGET, jid = bare_jid.as_str(), "sending displayed marker");

        let displayed = Element::builder("displayed", "urn:xmpp:chat-markers:0")
            .attr("id", id)
            .build();
        let message =
            XmppMessage::new(Some(bare_jid.clone().into())).with_payloads(vec![displayed]);

        self.client
            .send_stanza(message.into())
            .await
            .inspect_err(|error| {
                tracing::warn!(
                    target: LOG_TARGET,
                    jid = bare_jid.as_str(),
                    ?error,
                    "error sending displayed marker",
                );
            })
            .unwrap_or_default();
    }

    fn schedule_pending_composing(&mut self, bare_jid: BareJid) {
        self.pending_composing.insert(
            bare_jid,
            async {
                tokio::time::sleep(COMPOSING_DELAY).await;
            }
            .into_stream()
            .boxed(),
        );
    }

    async fn send_chat_state_notification(&mut self, bare_jid: BareJid, state: &str) {
        let composing = Element::builder(state, "http://jabber.org/protocol/chatstates")
            .prefix(None, "http://jabber.org/protocol/chatstates")
            .expect("not a duplicate prefix; qed")
            .build();
        let no_store = Element::builder("no-store", "urn:xmpp:hints")
            .prefix(None, "urn:xmpp:hints")
            .expect("not a duplicate prefix; qed")
            .build();
        let message = XmppMessage::new(Some(bare_jid.clone().into()))
            .with_payloads(vec![composing, no_store]);

        self.client
            .send_stanza(message.into())
            .await
            .inspect_err(|error| {
                tracing::warn!(
                    target: LOG_TARGET,
                    jid = bare_jid.as_str(),
                    ?error,
                    "error sending chat state notification",
                );
            })
            .unwrap_or_default();
    }

    async fn send_chat_state_composing(&mut self, bare_jid: BareJid) {
        tracing::trace!(target: LOG_TARGET, jid = bare_jid.as_str(), "sending state composing");

        self.send_chat_state_notification(bare_jid, "composing")
            .await;
    }

    async fn send_chat_state_active(&mut self, bare_jid: BareJid) {
        tracing::trace!(target: LOG_TARGET, jid = bare_jid.as_str(), "sending state active");

        self.send_chat_state_notification(bare_jid, "active").await;
    }

    async fn pre_approve_presence_subscriptions(&mut self) {
        let users = self.request_txs_map.keys();

        for jid in users {
            if let Ok(bare_jid) = BareJid::new(jid) {
                tracing::trace!(target: LOG_TARGET, jid, "pre-approving presence subscription");

                let presence = Presence::subscribed().with_to(bare_jid);
                self.client
                    .send_stanza(presence.into())
                    .await
                    .inspect_err(|error| {
                        tracing::error!(
                            target: LOG_TARGET,
                            jid,
                            ?error,
                            "error sending presence subscription pre-approval",
                        )
                    })
                    .unwrap_or_default();
            } else {
                tracing::error!(target: LOG_TARGET, jid, "cannot construct `BareJid`");
            }
        }
    }

    async fn send_initial_chat_state_active(&mut self) {
        let users = self.request_txs_map.keys().cloned().collect::<Vec<_>>();

        for jid in users {
            if let Ok(bare_jid) = BareJid::new(&jid) {
                tracing::trace!(target: LOG_TARGET, jid, "sending initial chat state `active`");

                self.send_chat_state_active(bare_jid).await;
            } else {
                tracing::error!(target: LOG_TARGET, jid, "cannot construct `BareJid`");
            }
        }
    }

    async fn process_xmpp_event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Online { .. } => {
                tracing::info!(target: LOG_TARGET, "connected to XMPP server");
                self.online = true;
                self.pre_approve_presence_subscriptions().await;
                self.send_presence().await;
                // This will clear "composing" notification from the last run if we previously crashed.
                self.send_initial_chat_state_active().await;
            }
            Event::Disconnected(error) => {
                // Make sure to not spam with error during every reconnection attemp.
                if self.online {
                    tracing::error!(
                        target: LOG_TARGET,
                        ?error,
                        "disconnected from XMPP server, reconnecting",
                    );
                    self.online = false;
                }
                // It is safe to sleep here, because we don't have any events to process while
                // XMPP cllient is disconnected.
                tokio::time::sleep(RECONNECT_DELAY).await;
                self.reconnect();
            }
            Event::Stanza(stanza) => {
                if let Ok(message) = XmppMessage::try_from(stanza) {
                    self.process_xmpp_message(message).await?;
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
                // TODO: checking for `self.online` here is a band-aid to reduce the chances of
                // losing responses. Ideally, we should queue responses and only discard them
                // once they have been sent out without errors.
                message = self.response_rx.recv(), if self.online => {
                    if let Some(message) = message {
                        self.process_response(message).await;
                    } else {
                        tracing::trace!(target: LOG_TARGET, "response channel closed, shutting down");
                        return Ok(())
                    }
                }
                _ = presence_tick.tick() => {
                    if self.online {
                        // This makes sure we detect dropped TCP stream and reconnect.
                        self.send_presence().await;
                    }
                }
                event = self.pending_composing.next(), if !self.pending_composing.is_empty() => {
                    if let Some((bare_jid, ())) = event {
                        self.send_chat_state_composing(bare_jid).await;
                    }
                }
            }
        }
    }
}
