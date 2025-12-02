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

//! XMPP agent.

use crate::xmpp::handle::XmppCommand;
use anyhow::{anyhow, Context as _};
use futures::{
    stream::{BoxStream, StreamExt},
    FutureExt,
};
use rxml::xml_ncname;
use std::{collections::HashMap, time::Duration};
use tokio::{
    sync::mpsc::{channel, error::TrySendError, Receiver, Sender},
    time::MissedTickBehavior,
};
use tokio_stream::StreamMap;
use tokio_xmpp::{Client as XmppClient, Event};
use wildmatch::WildMatch;
use xmpp_parsers::{
    jid::BareJid,
    message::{Id as MessageId, Lang, Message as XmppMessage, MessageType},
    minidom::Element,
    presence::{Presence, Show as PresenceShow},
};

pub use handle::{Attachment, XmppEvent, XmppHandle};

mod handle;

// Log target for this file.
const LOG_TARGET: &str = "jutella::xmpp";

// Delay before reconnecting to XMPP server. Built-in `tokio_xmpp` reconnect is too agressive
// and wastes up to 50% of a CPU core by reconnecting without a delay.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

// Period to send presence with.
const PRESENSE_INTERVAL: Duration = Duration::from_secs(60);

// Delay before sending back a composing notification.
const COMPOSING_DELAY: Duration = Duration::from_secs(1);

// OOB attachment download HTTP request timeout.
const ATTACHMENT_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

// Size of channel to deliver new XMPP chat instances. We deliver all new chats, maintaining
// backpressure.
const NEW_CHATS_CHANNEL_SIZE: usize = 1024;

// Requests channel size. If we have more than 10 pending messages from user something is extremely
// odd. We drop extra messages.
const REQUESTS_CHANNEL_SIZE: usize = 10;

// Responses channel size. We deliver all responses, maintaiing backpressure.
const RESPONSES_CHANNEL_SIZE: usize = 100;

#[derive(Debug)]
pub struct Config {
    pub auth_jid: BareJid,
    pub auth_password: String,
    pub allowed_jids: Vec<String>,
}

/// XMPP agent
pub struct Xmpp {
    auth_jid: BareJid,
    auth_password: String,
    client: XmppClient,
    allowed_jids: Vec<WildMatch>,
    new_chats_tx: Sender<XmppHandle>,
    to_handles_txs: HashMap<String, Sender<XmppEvent>>,
    from_handles_rx: Receiver<(BareJid, XmppCommand)>,
    from_handles_tx: Sender<(BareJid, XmppCommand)>,
    pending_composing: StreamMap<BareJid, BoxStream<'static, ()>>,
    online: bool,
    http_client: reqwest::Client,
}

impl Xmpp {
    pub fn new(config: Config) -> Result<(Self, Receiver<XmppHandle>), anyhow::Error> {
        let Config {
            auth_jid,
            auth_password,
            allowed_jids,
        } = config;

        let client = XmppClient::new(auth_jid.clone(), auth_password.clone());
        let http_client = reqwest::ClientBuilder::new()
            .timeout(ATTACHMENT_DOWNLOAD_TIMEOUT)
            .build()
            .context("failed to initialize HTTP client")?;
        let (new_chats_tx, new_chats_rx) = channel(NEW_CHATS_CHANNEL_SIZE);
        let (from_handles_tx, from_handles_rx) = channel(RESPONSES_CHANNEL_SIZE);

        Ok((
            Self {
                auth_jid,
                auth_password,
                client,
                allowed_jids: allowed_jids
                    .into_iter()
                    .map(|p| WildMatch::new(&p))
                    .collect(),
                new_chats_tx,
                to_handles_txs: HashMap::new(),
                from_handles_tx,
                from_handles_rx,
                pending_composing: StreamMap::new(),
                online: false,
                http_client,
            },
            new_chats_rx,
        ))
    }

    fn reconnect(&mut self) {
        self.client = XmppClient::new(self.auth_jid.clone(), self.auth_password.clone());
    }

    async fn send_xmpp_message(&mut self, bare_jid: BareJid, message: String) {
        let jid = bare_jid.as_str().to_owned();

        // If we are sending a message, we have finished composing.
        self.pending_composing.remove(&bare_jid);
        self.send_chat_state_active(bare_jid.clone()).await;

        let xmpp_message = XmppMessage::new(Some(bare_jid.into())).with_body(Lang::new(), message);

        if let Err(error) = self.client.send_stanza(xmpp_message.into()).await {
            tracing::error!(target: LOG_TARGET, jid, ?error, "failed to send xmpp message");
        }
    }

    async fn process_xmpp_message(&mut self, message: XmppMessage) -> anyhow::Result<()> {
        let Some(ref jid) = message.from else {
            tracing::trace!(target: LOG_TARGET, ?message, "xmpp message without `from` field");
            return Ok(());
        };

        let bare_jid = jid.to_bare();
        let jid = bare_jid.as_str().to_owned();

        let event_tx = if let Some(event_tx) = self.to_handles_txs.get(&jid) {
            event_tx
        } else if self.allowed_jids.iter().any(|p| p.matches(&jid)) {
            let (event_tx, event_rx) = channel(REQUESTS_CHANNEL_SIZE);
            let handle = XmppHandle {
                jid: bare_jid.clone(),
                rx: event_rx,
                tx: self.from_handles_tx.clone(),
                client: self.http_client.clone(),
            };
            if self.new_chats_tx.send(handle).await.is_err() {
                tracing::error!(
                    target: LOG_TARGET,
                    "new chats channel closed, terminating XMPP",
                );
                return Err(anyhow!("new chats channel closed"));
            };
            self.approve_presence_subscription(bare_jid.clone()).await;
            self.send_chat_state_active(bare_jid.clone()).await;
            self.to_handles_txs.insert(jid.clone(), event_tx);
            self.to_handles_txs.get(&jid).expect("inserted above; qed")
        } else {
            tracing::trace!(target: LOG_TARGET, jid, "message from unknown user");
            return Ok(());
        };

        if message.type_ != MessageType::Chat {
            tracing::debug!(
                target: LOG_TARGET,
                jid,
                type_ = ?message.type_,
                "not a chat message received",
            );
            return Ok(());
        }

        let Some((_lang, body)) = message.get_best_body_cloned(Vec::new()) else {
            tracing::trace!(target: LOG_TARGET, jid, "chat message without a body");
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

        let event = match message
            .payloads
            .into_iter()
            .find(|p| p.name() == "x" && p.ns() == "jabber:x:oob")
        {
            Some(oob) => {
                if let Some(url) = oob.get_child("url", "jabber:x:oob").map(|e| e.text()) {
                    XmppEvent::Attachment {
                        url,
                        id: message.id,
                    }
                } else {
                    tracing::debug!(target: LOG_TARGET, jid, "attachment message without url");
                    self.send_xmpp_message(bare_jid, "[ERROR] Attachment without URL".to_string())
                        .await;

                    return Ok(());
                }
            }
            None => XmppEvent::Message {
                text: body,
                id: message.id,
            },
        };

        match event_tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                tracing::debug!(target: LOG_TARGET, jid, "chat channel full, discarding message")
            }
            Err(TrySendError::Closed(_)) => {
                tracing::warn!(target: LOG_TARGET, jid, "chat channel closed");
                self.to_handles_txs.remove(&jid);
            }
        }

        Ok(())
    }

    async fn send_displayed_marker(&mut self, bare_jid: BareJid, id: MessageId) {
        tracing::trace!(target: LOG_TARGET, jid = bare_jid.as_str(), "sending displayed marker");

        let displayed = Element::builder("displayed", "urn:xmpp:chat-markers:0")
            .attr(xml_ncname!("id").to_owned(), id)
            .build();
        let message =
            XmppMessage::new(Some(bare_jid.clone().into())).with_payloads(vec![displayed]);

        if let Err(error) = self.client.send_stanza(message.into()).await {
            tracing::warn!(
                target: LOG_TARGET,
                jid = bare_jid.as_str(),
                ?error,
                "error sending displayed marker",
            );
        }
    }

    fn schedule_pending_composing(&mut self, bare_jid: BareJid) {
        self.pending_composing.insert(
            bare_jid,
            tokio::time::sleep(COMPOSING_DELAY).into_stream().boxed(),
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

        if let Err(error) = self.client.send_stanza(message.into()).await {
            tracing::warn!(
                target: LOG_TARGET,
                jid = bare_jid.as_str(),
                ?error,
                "error sending chat state notification",
            );
        }
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

    async fn approve_presence_subscription(&mut self, bare_jid: BareJid) {
        let presence = Presence::subscribed().with_to(bare_jid.clone());
        if let Err(error) = self.client.send_stanza(presence.into()).await {
            tracing::error!(
                target: LOG_TARGET,
                jid = bare_jid.to_string(),
                ?error,
                "error sending presence subscription pre-approval",
            )
        }
    }

    async fn process_xmpp_event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Online { .. } => {
                tracing::info!(target: LOG_TARGET, "connected to XMPP server");
                self.online = true;
                self.send_presence().await;
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

    async fn process_command(&mut self, jid: BareJid, command: XmppCommand) {
        match command {
            XmppCommand::Message(message) => self.send_xmpp_message(jid, message).await,
            XmppCommand::Displayed(id) => self.send_displayed_marker(jid, id).await,
            XmppCommand::Composing => self.schedule_pending_composing(jid),
        }
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
                command = self.from_handles_rx.recv(), if self.online => {
                    if let Some((jid, command)) = command {
                        self.process_command(jid, command).await;
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
