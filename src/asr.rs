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

//! Speech recognition for voice messages. Uses OpenAI-compatible API.

use bytes::Bytes;
use reqwest::{
    header::{HeaderValue, InvalidHeaderValue, AUTHORIZATION},
    multipart::{Form, Part},
    StatusCode, Url,
};
use std::time::Duration;

const TRANSCRIPTIONS_ENDPOINT: &str = "audio/transcriptions";

/// Speech recognition configuration.
#[derive(Debug, Clone)]
pub struct AsrConfig {
    /// API URL (everything before `audio/transcriptions`) of OpenAI-compatible API endpoint.
    pub api_url: String,
    /// Token for header `Authorization: Bearer {api_token}`, if required.
    pub api_token: Option<String>,
    /// Model to use, if required.
    pub model: Option<String>,
    /// HTTP request timeout.
    pub timeout: Option<Duration>,
}

/// Speech recognition using OpenAI-compatible API endpoint.
#[derive(Debug, Clone)]
pub struct Asr {
    client: reqwest::Client,
    endpoint: Url,
    model: Option<String>,
}

/// Error returned from [`Asr::new`].
#[derive(Debug, thiserror::Error)]
pub enum NewAsrError {
    #[error("Authorization token contains not allowed characters")]
    TokenInvalidCharacters(#[from] InvalidHeaderValue),
    #[error("Failed to initialize HTTP client: {0}")]
    HttpClientInitializationError(#[from] reqwest::Error),
    #[error("Invalid API URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
}

/// Error returned from [`Asr::transcribe`].
#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    #[error("HTTP request failed")]
    RequestFailed(#[from] reqwest::Error),
    #[error("Transcription service error")]
    HttpApiError { status: StatusCode, body: String },
}

impl Asr {
    pub fn new(
        AsrConfig {
            api_url,
            api_token,
            model,
            timeout,
        }: AsrConfig,
    ) -> Result<Self, NewAsrError> {
        let client_builder = reqwest::ClientBuilder::new();
        let client_builder = if let Some(token) = api_token {
            client_builder.default_headers(
                [(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {token}"))?,
                )]
                .into_iter()
                .collect(),
            )
        } else {
            client_builder
        };
        let client_builder = if let Some(timeout) = timeout {
            client_builder.timeout(timeout)
        } else {
            client_builder
        };

        let url = ensure_trailing_slash(api_url);

        Ok(Asr {
            client: client_builder.build()?,
            endpoint: Url::parse(&format!("{url}{TRANSCRIPTIONS_ENDPOINT}"))?,
            model,
        })
    }

    pub async fn transcribe(
        &self,
        filename: String,
        data: Bytes,
    ) -> Result<String, TranscribeError> {
        let form = Form::new().text("response_format", "text");
        let form = if let Some(ref model) = self.model {
            form.text("model", model.clone())
        } else {
            form
        };
        let form = form.part("file", Part::bytes(Vec::from(data)).file_name(filename));

        let response = self
            .client
            .post(self.endpoint.clone())
            .multipart(form)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if status.is_success() {
            Ok(body)
        } else {
            Err(TranscribeError::HttpApiError { status, body })
        }
    }
}

fn ensure_trailing_slash(url: String) -> String {
    if url.ends_with('/') {
        url
    } else {
        url + "/"
    }
}
