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

//! Chatbot.

use crate::types::Message;
use anyhow::{anyhow, Context as _};
use jutella::ChatClient;
use tokio::sync::mpsc::{error::TrySendError, Receiver, Sender};

const LOG_TARGET: &str = "jutella::chatbot";

/// Configuration for [`Jutella`].
#[derive(Debug)]
pub struct Config {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub system_message: String,
    pub allowed_users: Vec<String>,
    pub request_rx: Receiver<Message>,
    pub response_tx: Sender<Message>,
}

pub struct Jutella {
    request_rx: Receiver<Message>,
    response_tx: Sender<Message>,
    clogged_fired: bool,
}

impl Jutella {
    pub fn new(config: Config) -> Self {
        let Config {
            api_url,
            api_key,
            model,
            system_message,
            allowed_users,
            request_rx,
            response_tx,
        } = config;

        Self {
            request_rx,
            response_tx,
            clogged_fired: false,
        }
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        loop {
            let message = self
                .request_rx
                .recv()
                .await
                .context("request channel closed, terminating chatbot client`")?;

            match self.response_tx.try_send(message) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    if !self.clogged_fired {
                        tracing::error!(target: LOG_TARGET, "responses channel clogged");
                        self.clogged_fired = true;
                    }
                }
                Err(TrySendError::Closed(_)) => {
                    return Err(anyhow!(
                        "responses channel closed, terminating chatbot client"
                    ))
                }
            }
        }
    }
}
