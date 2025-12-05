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

//! Chatbot chat handler.

use crate::xmpp::{Attachment, XmppEvent, XmppHandle};
use futures::stream::StreamExt;
use jutella::{
    ApiOptions, Auth, ChatClient, ChatClientConfig, Completion, Content, ContentPart, FilePart,
    ImagePart, TokenUsage,
};
use serde_json::value::Value;
use std::time::Duration;

// Log target for this file.
const LOG_TARGET: &str = "jutella::chat";

/// Configuration of [`Chat`]
// Can't implement `Debug` due to `tiktoken_rs::CoreBPE` not implementing it.
pub struct ChatConfig {
    pub api_url: String,
    pub api_options: ApiOptions,
    pub api_version: Option<String>,
    pub auth: Auth,
    pub http_timeout: Duration,
    pub model: String,
    pub system_message: Option<String>,
    pub system_message_tokens: usize,
    pub verbosity: Option<String>,
    pub sanitize_links: bool,
    pub min_history_tokens: Option<usize>,
    pub max_history_tokens: usize,
    pub reqwest_client: reqwest::Client,
    pub xmpp_handle: XmppHandle,
    pub extra_params: Option<serde_json::map::Map<String, Value>>,
}

/// Single chatbot conversation handler.
pub struct Chat {
    jid: String,
    client: ChatClient,
    xmpp: XmppHandle,
    pending_attachments: Vec<ContentPart>,
}

impl Chat {
    pub fn new(config: ChatConfig) -> Result<Self, jutella::Error> {
        let ChatConfig {
            api_url,
            api_options,
            api_version,
            auth,
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
        } = config;

        let client = ChatClient::new_with_client_and_system_tokens(
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
                sanitize_links,
                extra_params,
            },
            reqwest_client,
            system_message_tokens,
        )?;

        Ok(Self {
            jid: xmpp_handle.jid().as_str().to_owned(),
            client,
            xmpp: xmpp_handle,
            pending_attachments: Vec::new(),
        })
    }

    async fn handle_completion(
        &mut self,
        Completion {
            response,
            reasoning: _,
            token_usage:
                TokenUsage {
                    tokens_in,
                    tokens_in_cached,
                    tokens_out,
                    tokens_reasoning,
                },
        }: Completion,
    ) -> anyhow::Result<()> {
        match response {
            Content::Text(response) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    jid = self.jid,
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
                    "response",
                );

                self.xmpp.send_message(response).await?;
            }
            Content::ContentParts(parts) => {
                tracing::debug!(
                    target: LOG_TARGET,
                    jid = self.jid,
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
                    "response with content",
                );

                for part in parts {
                    match part {
                        ContentPart::Text(text) => {
                            tracing::debug!(
                                target: LOG_TARGET,
                                jid = self.jid,
                                len = text.len(),
                                "text content",
                            );

                            self.xmpp.send_message(text).await?;
                        }
                        ContentPart::Image(ImagePart {
                            url: base64_url,
                            detail: _,
                        }) => {
                            tracing::debug!(
                                target: LOG_TARGET,
                                jid = self.jid,
                                encoded_len = base64_url.len(),
                                "image content",
                            );

                            match self.xmpp.upload_attachment(base64_url).await {
                                Ok(url) => self.xmpp.send_attachment(url).await?,
                                Err(error) => {
                                    tracing::error!(
                                        target: LOG_TARGET,
                                        jid = self.jid,
                                        ?error,
                                        "attachment upload failure",
                                    );

                                    self.xmpp
                                        .send_message(
                                            "[ERROR] Failed to upload attachment".to_string(),
                                        )
                                        .await?;
                                }
                            }
                        }
                        ContentPart::File(_) => {
                            tracing::error!(
                                target: LOG_TARGET,
                                jid = self.jid,
                                "discarding unsupported file content part",
                            );

                            self.xmpp
                                .send_message(
                                    "[ERROR] Unsupported file content discarded".to_string(),
                                )
                                .await?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn on_xmpp_event(&mut self, event: XmppEvent) -> anyhow::Result<()> {
        match event {
            XmppEvent::Message { text, id } => {
                tracing::debug!(target: LOG_TARGET, jid = self.jid, len = text.len(), "request");

                if let Some(id) = id {
                    self.xmpp.displayed(id).await?;
                }

                self.xmpp.start_composing().await?;

                match self.generate_completion(text).await {
                    Ok(completion) => self.handle_completion(completion).await?,
                    Err(error) => {
                        tracing::warn!(
                            target: LOG_TARGET,
                            jid = self.jid,
                            "error from chatbot API: {error}",
                        );

                        self.xmpp.send_message(format!("[ERROR] {error}")).await?;
                    }
                }
            }
            XmppEvent::Attachment { url, id } => match self.xmpp.download_attachment(url).await {
                Ok((attachment, size)) => {
                    tracing::debug!(target: LOG_TARGET, jid = self.jid, size, "attachment");

                    if let Some(id) = id {
                        self.xmpp.displayed(id).await?;
                    }

                    let content_part = match attachment {
                        Attachment::Image(base64_string) => ContentPart::Image(ImagePart {
                            url: base64_string,
                            detail: None,
                        }),
                        Attachment::Pdf { filename, data } => ContentPart::File(FilePart {
                            file_data: data,
                            filename: Some(filename),
                        }),
                    };

                    self.pending_attachments.push(content_part);
                }
                Err(error) => {
                    tracing::debug!(
                        target: LOG_TARGET,
                        jid = self.jid,
                        ?error,
                        "failed to download attachment",
                    );

                    let error_message = if error.is_invalid_type() {
                        "[ERROR] Only PDF, JPEG, PNG, WEBP & non-animated GIF files are supported"
                    } else {
                        "[ERROR] Failed to download attachment"
                    };

                    self.xmpp.send_message(error_message.to_string()).await?;
                }
            },
        }

        Ok(())
    }

    async fn generate_completion(&mut self, request: String) -> Result<Completion, jutella::Error> {
        let request = if self.pending_attachments.is_empty() {
            Content::Text(request)
        } else {
            let mut parts = std::mem::take(&mut self.pending_attachments);
            parts.push(ContentPart::Text(request));

            Content::ContentParts(parts)
        };

        self.client.request_completion(request).await
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        loop {
            if let Some(event) = self.xmpp.next().await {
                self.on_xmpp_event(event).await?;
            } else {
                return Ok(());
            }
        }
    }
}
