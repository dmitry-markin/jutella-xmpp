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

use crate::message::{Content, RequestMessage, ResponseMessage};
use anyhow::{anyhow, Context as _};
use base64::prelude::{Engine, BASE64_STANDARD};
use futures::{
    stream::{BoxStream, StreamExt},
    FutureExt,
};
use rxml::xml_ncname;
use std::{collections::HashSet, time::Duration};
use tokio::{
    sync::mpsc::{Receiver, Sender},
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

// Requests channel size.
pub const REQUESTS_CHANNEL_SIZE: usize = 1024;

// Responses channel size.
pub const RESPONSES_CHANNEL_SIZE: usize = 1024;

#[derive(Debug)]
pub struct Config {
    pub auth_jid: BareJid,
    pub auth_password: String,
    pub allowed_jids: Vec<String>,
    pub request_tx: Sender<RequestMessage>,
    pub response_rx: Receiver<ResponseMessage>,
}

/// XMPP agent
pub struct Xmpp {
    auth_jid: BareJid,
    auth_password: String,
    client: XmppClient,
    allowed_jids: Vec<WildMatch>,
    active_jids: HashSet<String>,
    request_tx: Sender<RequestMessage>,
    response_rx: Receiver<ResponseMessage>,
    pending_composing: StreamMap<BareJid, BoxStream<'static, ()>>,
    online: bool,
    http_client: reqwest::Client,
}

impl Xmpp {
    pub fn new(config: Config) -> Result<Self, anyhow::Error> {
        let Config {
            auth_jid,
            auth_password,
            allowed_jids,
            request_tx,
            response_rx,
        } = config;

        let client = XmppClient::new(auth_jid.clone(), auth_password.clone());
        let http_client = reqwest::ClientBuilder::new()
            .timeout(ATTACHMENT_DOWNLOAD_TIMEOUT)
            .build()
            .context("failed to initialize HTTP client")?;

        Ok(Self {
            auth_jid,
            auth_password,
            client,
            allowed_jids: allowed_jids
                .into_iter()
                .map(|p| WildMatch::new(&p))
                .collect(),
            active_jids: HashSet::new(),
            request_tx,
            response_rx,
            pending_composing: StreamMap::new(),
            online: false,
            http_client,
        })
    }

    fn reconnect(&mut self) {
        self.client = XmppClient::new(self.auth_jid.clone(), self.auth_password.clone());
    }

    async fn send_xmpp_message(&mut self, bare_jid: BareJid, message: String) {
        let jid = bare_jid.as_str().to_owned();
        let xmpp_message = XmppMessage::new(Some(bare_jid.into())).with_body(Lang::new(), message);

        if let Err(error) = self.client.send_stanza(xmpp_message.into()).await {
            tracing::error!(target: LOG_TARGET, jid, ?error, "failed to send xmpp message");
        }
    }

    async fn process_response(&mut self, resp: ResponseMessage) {
        let ResponseMessage {
            jid,
            response,
            tokens_in,
            tokens_in_cached,
            tokens_out,
            tokens_reasoning,
        } = resp;

        tracing::debug!(
            target: LOG_TARGET,
            jid,
            len = response.len(),
            tokens_in,
            tokens_cached = tokens_in_cached.and_then(|v| match v {
                0 => None,
                v => Some(v),
            }),
            tokens_out,
            tokens_reasoning = tokens_reasoning.and_then(|v| match v {
                0 => None,
                v => Some(v),
            }),
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

        if !self.active_jids.contains(&jid) {
            if self.allowed_jids.iter().any(|p| p.matches(&jid)) {
                self.approve_presence_subscription(bare_jid.clone()).await;
                self.send_chat_state_active(bare_jid.clone()).await;
                self.active_jids.insert(jid.clone());
            } else {
                tracing::trace!(target: LOG_TARGET, jid, "message from unknown user");
                return Ok(());
            }
        }

        if message.type_ != MessageType::Chat {
            tracing::debug!(
                target: LOG_TARGET,
                jid,
                type_ = ?message.type_,
                "not a chat message received",
            );
            return Ok(());
        }

        let Some(body) = message.bodies.get("") else {
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

        match message
            .payloads
            .into_iter()
            .find(|p| p.name() == "x" && p.ns() == "jabber:x:oob")
        {
            Some(oob) => {
                self.process_attachment_message(bare_jid, oob, message.id)
                    .await
            }
            None => {
                self.process_text_message(bare_jid, body.clone(), message.id)
                    .await
            }
        }
    }

    async fn process_text_message(
        &mut self,
        bare_jid: BareJid,
        request: String,
        message_id: Option<MessageId>,
    ) -> anyhow::Result<()> {
        let jid = bare_jid.as_str().to_owned();

        tracing::debug!(target: LOG_TARGET, jid, len = request.len(), "request");

        let req = RequestMessage {
            jid,
            request: Content::Text(request),
        };

        match self.request_tx.send(req).await {
            Ok(()) => {
                self.schedule_pending_composing(bare_jid.clone());

                if let Some(id) = message_id {
                    self.send_displayed_marker(bare_jid, id).await;
                }
            }
            Err(_) => return Err(anyhow!("requests channel closed, terminating")),
        }

        Ok(())
    }

    async fn process_attachment_message(
        &mut self,
        bare_jid: BareJid,
        oob: Element,
        message_id: Option<MessageId>,
    ) -> anyhow::Result<()> {
        let jid = bare_jid.as_str().to_owned();

        let Some(url) = oob.get_child("url", "jabber:x:oob").map(|e| e.text()) else {
            tracing::debug!(target: LOG_TARGET, jid, "attachment message without url");
            self.send_xmpp_message(bare_jid, "[ERROR] Attachment without URL".to_string())
                .await;

            return Ok(());
        };

        let (content, size) = match download_attachment(self.http_client.clone(), url).await {
            Ok(attachment) => attachment,
            Err(error) => {
                tracing::debug!(target: LOG_TARGET, jid, ?error, "failed to download attachment");

                let error_message = if error.is_invalid_type() {
                    "[ERROR] Only PDF, JPEG, PNG, WEBP & non-animated GIF files are supported"
                } else {
                    "[ERROR] Failed to download attachment"
                };

                self.send_xmpp_message(bare_jid, error_message.to_string())
                    .await;

                return Ok(());
            }
        };

        tracing::debug!(target: LOG_TARGET, jid, size, "attachment");

        let req = RequestMessage {
            jid,
            request: content,
        };

        match self.request_tx.send(req).await {
            Ok(()) => {
                if let Some(id) = message_id {
                    self.send_displayed_marker(bare_jid, id).await;
                }
            }
            Err(_) => return Err(anyhow!("requests channel closed, terminating")),
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

#[derive(Debug, thiserror::Error)]
enum AttachmentDownloadError {
    #[error("request error: {0}")]
    Request(#[from] reqwest::Error),
    #[error("unknown MIME-type")]
    UnknownMimeType,
    #[error("unsupported MIME-type {0}")]
    UnsupportedMimeType(String),
    #[error("{0}")]
    OtherError(&'static str),
}

impl AttachmentDownloadError {
    fn is_invalid_type(&self) -> bool {
        matches!(self, Self::UnknownMimeType | Self::UnsupportedMimeType(_))
    }
}

async fn download_attachment(
    client: reqwest::Client,
    url: String,
) -> Result<(Content, usize), AttachmentDownloadError> {
    use AttachmentDownloadError::{OtherError, UnknownMimeType, UnsupportedMimeType};

    let filename = match url.split('/').next_back() {
        Some(filename) => filename.to_owned(),
        None => return Err(OtherError("empty URL")),
    };

    let response = client.get(url).send().await?;
    let Some(mime_type) = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
    else {
        return Err(UnknownMimeType);
    };

    let is_pdf = match mime_type {
        "application/pdf" => true,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" => false,
        mime_type => return Err(UnsupportedMimeType(mime_type.to_string())),
    };

    let mime_type = mime_type.to_owned();
    let bytes = response.bytes().await?;
    let size = bytes.len();
    let base64_string = BASE64_STANDARD.encode(bytes);
    let encoded_data = format!("data:{mime_type};base64,{base64_string}");

    if is_pdf {
        Ok((
            Content::Pdf {
                filename: filename.to_owned(),
                data: encoded_data,
            },
            size,
        ))
    } else {
        Ok((Content::Image(encoded_data), size))
    }
}
