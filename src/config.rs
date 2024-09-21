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

//! `jutella-xmpp` configuration.

use anyhow::{anyhow, Context as _, Result};
use clap::Parser;
use std::{fs, path::PathBuf};
use xmpp::agent::BareJid;

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Config file location.
    #[arg(short, long, default_value = "/etc/jutellaxmpp.toml")]
    config: PathBuf,
}

#[derive(Debug, serde::Deserialize)]
struct ConfigFile {
    jid: String,
    password: String,
    allowed_users: Vec<String>,
    api_url: String,
    api_key: String,
    model: String,
    system_message: String,
}

impl ConfigFile {
    fn load(path: PathBuf) -> Result<Self> {
        let config = fs::read_to_string(path.clone()).with_context(|| {
            anyhow!(
                "Failed to read config file {}",
                path.to_str().expect("to have only unicode characters"),
            )
        })?;

        toml::from_str(&config).context("Invalid config")
    }
}

#[derive(Debug)]
pub struct Config {
    pub auth_jid: BareJid,
    pub auth_password: String,
    pub allowed_users: Vec<String>,
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub system_message: String,
}

impl Config {
    pub fn load() -> Result<Self> {
        let Args { config } = Args::parse();
        let ConfigFile {
            jid,
            password,
            allowed_users,
            api_url,
            api_key,
            model,
            system_message,
        } = ConfigFile::load(config)?;

        let auth_jid = BareJid::new(&jid).context("Invalid auth JID")?;

        Ok(Self {
            auth_jid,
            auth_password: password,
            allowed_users,
            api_url,
            api_key,
            model,
            system_message,
        })
    }
}
