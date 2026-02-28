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

use crate::{
    asr::Asr,
    xmpp::{Attachment, XmppEvent, XmppHandle},
};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use bytes::Bytes;
use futures::stream::StreamExt;
use jutella::{
    ApiOptions, Auth, ChatClient, ChatClientConfig, Completion, Content, ContentPart, FilePart,
    ImagePart, TokenUsage,
};
use serde_json::value::Value;
use std::time::Duration;

// Log target for this file.
const LOG_TARGET: &str = "jutella::chat";

const INVALID_TYPE_ERROR: &str =
    "[ERROR] Only pdf, jpeg, png, webp, non-animated gif, wav, mp3 & m4a attachments supported";
const INVALID_TYPE_ERROR_NO_AUDIO: &str =
    "[ERROR] Only pdf, jpeg, png, webp & non-animated gif attachments supported";

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
    pub asr: Option<Asr>,
}

/// Single chatbot conversation handler.
pub struct Chat {
    jid: String,
    client: ChatClient,
    xmpp: XmppHandle,
    pending_attachments: Vec<ContentPart>,
    asr: Option<Asr>,
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
            asr,
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
            asr,
        })
    }

    fn log_response(
        &self,
        len: usize,
        image_sizes: Option<Vec<usize>>,
        TokenUsage {
            tokens_in,
            tokens_in_cached,
            tokens_out,
            tokens_reasoning,
        }: TokenUsage,
    ) {
        let tokens_cached = tokens_in_cached.and_then(|v| match v {
            0 => None,
            v => Some(v),
        });
        let tokens_reasoning = tokens_reasoning.and_then(|v| match v {
            0 => None,
            v => Some(v),
        });

        if let Some(image_sizes) = image_sizes {
            tracing::debug!(
                target: LOG_TARGET,
                jid = self.jid,
                len,
                ?image_sizes,
                tokens_in,
                tokens_cached,
                tokens_out,
                tokens_reasoning,
                "response with images",
            );
        } else {
            tracing::debug!(
                target: LOG_TARGET,
                jid = self.jid,
                len,
                tokens_in,
                tokens_cached,
                tokens_out,
                tokens_reasoning,
                "response",
            );
        }
    }

    async fn handle_completion(
        &mut self,
        Completion {
            response,
            reasoning: _,
            token_usage,
        }: Completion,
    ) -> anyhow::Result<()> {
        match response {
            Content::Text(response) => {
                self.log_response(response.len(), None, token_usage);

                self.xmpp.send_message(response).await?;
            }
            Content::ContentParts(parts) => {
                let mut len = 0;
                let mut image_sizes = Vec::new();

                for part in parts {
                    match part {
                        ContentPart::Text(text) => {
                            len += text.len();

                            self.xmpp.send_message(text).await?;
                        }
                        ContentPart::Image(ImagePart {
                            url: base64_url,
                            detail: _,
                        }) => match self.xmpp.upload_image(base64_url).await {
                            Ok((url, size)) => {
                                image_sizes.push(size);

                                self.xmpp.send_attachment(url).await?;
                            }
                            Err(error) => {
                                tracing::error!(
                                    target: LOG_TARGET,
                                    jid = self.jid,
                                    ?error,
                                    "attachment upload failure",
                                );

                                self.xmpp
                                    .send_message("[ERROR] Failed to upload attachment".to_string())
                                    .await?;
                            }
                        },
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

                self.log_response(len, Some(image_sizes), token_usage);
            }
        }

        Ok(())
    }

    async fn handle_text_prompt(&mut self, text: String) -> anyhow::Result<()> {
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

        Ok(())
    }

    async fn on_xmpp_event(&mut self, event: XmppEvent) -> anyhow::Result<()> {
        match event {
            XmppEvent::Message { text, id } => {
                tracing::debug!(target: LOG_TARGET, jid = self.jid, len = text.len(), "request");

                if let Some(id) = id {
                    self.xmpp.displayed(id).await?;
                }

                self.handle_text_prompt(text).await?;
            }
            // TODO: we might want to skip downloading an attachment if the file extension is not
            // supported.
            XmppEvent::Attachment { url, id } => match self.xmpp.download_attachment(url).await {
                Ok(Attachment {
                    content_type,
                    filename,
                    data,
                }) => {
                    let attachment_type = AttachmentType::from_content_type(&content_type);

                    match attachment_type {
                        AttachmentType::Image | AttachmentType::Pdf => {
                            tracing::debug!(
                                target: LOG_TARGET,
                                jid = self.jid,
                                size = data.len(),
                                "attachment",
                            );
                        }
                        AttachmentType::Other => {
                            tracing::debug!(
                                target: LOG_TARGET,
                                jid = self.jid,
                                size = data.len(),
                                content_type,
                                "attachment ignored, content-type not supported",
                            );
                        }
                        AttachmentType::Audio => {
                            // Will be logged below depending on ASR status.
                        }
                    }

                    if let Some(id) = id {
                        self.xmpp.displayed(id).await?;
                    }

                    match attachment_type {
                        AttachmentType::Image => {
                            self.pending_attachments.push(ContentPart::Image(ImagePart {
                                url: url_base64_encode(&content_type, data),
                                detail: None,
                            }));
                        }
                        AttachmentType::Pdf => {
                            self.pending_attachments.push(ContentPart::File(FilePart {
                                file_data: url_base64_encode(&content_type, data),
                                filename: Some(filename),
                            }));
                        }
                        AttachmentType::Audio => {
                            let audio_size = data.len();

                            if let Some(ref asr) = self.asr {
                                match asr.transcribe(filename, data).await {
                                    Ok(text) => {
                                        tracing::debug!(
                                            target: LOG_TARGET,
                                            jid = self.jid,
                                            len = text.len(),
                                            audio_size,
                                            "voice request",
                                        );

                                        let quotation = "> ".to_string() + &text;
                                        self.xmpp.send_message(quotation).await?;

                                        self.handle_text_prompt(text).await?;
                                    }
                                    Err(error) => {
                                        tracing::warn!(
                                            target: LOG_TARGET,
                                            jid = self.jid,
                                            content_type,
                                            audio_size,
                                            ?error,
                                            "speech recognition error",
                                        );

                                        self.xmpp
                                            .send_message(format!(
                                                "[ERROR] Speech recognition error: {error}"
                                            ))
                                            .await?;
                                    }
                                }
                            } else {
                                tracing::debug!(
                                    target: LOG_TARGET,
                                    jid = self.jid,
                                    audio_size,
                                    "voice request ignored, ASR not enabled",
                                );

                                self.xmpp
                                    .send_message(
                                        "[ERROR] Speech recognition not enabled".to_string(),
                                    )
                                    .await?;
                            }
                        }
                        AttachmentType::Other => {
                            let error_message = if self.asr.is_some() {
                                INVALID_TYPE_ERROR
                            } else {
                                INVALID_TYPE_ERROR_NO_AUDIO
                            };

                            self.xmpp.send_message(error_message.to_string()).await?;
                        }
                    }
                }
                Err(error) => {
                    tracing::debug!(
                        target: LOG_TARGET,
                        jid = self.jid,
                        ?error,
                        "failed to download attachment",
                    );

                    self.xmpp
                        .send_message("[ERROR] Failed to download attachment".to_string())
                        .await?;
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

enum AttachmentType {
    Image,
    Pdf,
    Audio,
    Other,
}

impl AttachmentType {
    fn from_content_type(content_type: &str) -> AttachmentType {
        match content_type {
            "image/jpeg" | "image/png" | "image/gif" | "image/webp" => AttachmentType::Image,
            "application/pdf" => AttachmentType::Pdf,
            "audio/mp4"     // .m4a
            | "audio/mpeg"  // .mp3
            | "audio/wav" => AttachmentType::Audio,
            _ => AttachmentType::Other,
        }
    }
}

fn url_base64_encode(content_type: &str, data: Bytes) -> String {
    let base64 = BASE64_STANDARD.encode(data);

    format!("data:{content_type};base64,{base64}")
}
