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

//! XMPP chat handle.

use base64::prelude::{Engine, BASE64_STANDARD};
use futures::{
    stream::Stream,
    task::{Context, Poll},
};
use std::pin::Pin;
use tokio::sync::mpsc::{error::SendError, Receiver, Sender};
use tokio_xmpp::jid::BareJid;
use xmpp_parsers::message::Id as MessageId;

/// XMPP event.
pub enum XmppEvent {
    /// Text message received.
    Message { text: String, id: Option<MessageId> },
    /// Attachment URL received.
    Attachment { url: String, id: Option<MessageId> },
}

/// Command from handle to XMPP.
pub enum XmppCommand {
    /// Send text message.
    Message(String),
    /// Show displayed marker.
    Displayed(MessageId),
    /// Composing notification. Automatically cleared once [`XmppCommand::Message`] is sent.
    Composing,
}

/// XMPP attachment.
#[derive(Clone)]
pub enum Attachment {
    /// Image attachment in base64 encoding.
    Image(String),
    /// PDF attachment.
    Pdf {
        /// File name passed to the model.
        filename: String,
        /// Base64-encoded file content.
        data: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("HTTP request error: {0}")]
    Request(#[from] reqwest::Error),
    #[error("unknown MIME-type")]
    UnknownMimeType,
    #[error("unsupported MIME-type {0}")]
    UnsupportedMimeType(String),
    #[error("{0}")]
    OtherError(&'static str),
}

impl DownloadError {
    pub fn is_invalid_type(&self) -> bool {
        matches!(self, Self::UnknownMimeType | Self::UnsupportedMimeType(_))
    }
}

/// Handle of XMPP chat with a specific user.
pub struct XmppHandle {
    /// JID of this chat,
    pub jid: BareJid,
    /// Receiver of XMPP events.
    pub rx: Receiver<XmppEvent>,
    /// Sender of commands to XMPP.
    pub tx: Sender<(BareJid, XmppCommand)>,
    /// HTTP client for downloading atachmens.
    pub client: reqwest::Client,
}

impl XmppHandle {
    /// JID of this chat.
    pub fn jid(&self) -> &BareJid {
        &self.jid
    }

    /// Send text message.
    pub async fn send_message(
        &self,
        text: String,
    ) -> Result<(), SendError<(BareJid, XmppCommand)>> {
        self.tx
            .send((self.jid.clone(), XmppCommand::Message(text)))
            .await
    }

    /// Send displayed marker.
    pub async fn displayed(&self, id: MessageId) -> Result<(), SendError<(BareJid, XmppCommand)>> {
        self.tx
            .send((self.jid.clone(), XmppCommand::Displayed(id)))
            .await
    }

    /// Start composing notification. Will be stopped once a message is sent.
    pub async fn start_composing(&self) -> Result<(), SendError<(BareJid, XmppCommand)>> {
        self.tx
            .send((self.jid.clone(), XmppCommand::Composing))
            .await
    }

    /// Download attachment from the message.
    pub async fn download_attachment(
        &self,
        url: String,
    ) -> Result<(Attachment, usize), DownloadError> {
        use DownloadError::{OtherError, UnknownMimeType, UnsupportedMimeType};

        let filename = match url.split('/').next_back() {
            Some(filename) => filename.to_owned(),
            None => return Err(OtherError("empty URL")),
        };

        let response = self.client.get(url).send().await?;
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
                Attachment::Pdf {
                    filename: filename.to_owned(),
                    data: encoded_data,
                },
                size,
            ))
        } else {
            Ok((Attachment::Image(encoded_data), size))
        }
    }
}

impl Stream for XmppHandle {
    type Item = XmppEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.as_mut().rx.poll_recv(cx)
    }
}
