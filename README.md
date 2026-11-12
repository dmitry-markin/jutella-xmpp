# jutella-xmpp

[![License](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/dmitry-markin/jutella-xmpp/blob/master/LICENSE) [![crates.io](https://img.shields.io/crates/v/jutella-xmpp.svg)](https://crates.io/crates/jutella-xmpp)

XMPP bridge to LLM APIs based on [tokio-xmpp](https://docs.rs/tokio-xmpp/latest/tokio_xmpp/) and [jutella](https://github.com/dmitry-markin/jutella).

Supports the OpenAI chat completions API (including Azure-style authentication) and the OpenRouter chat completions API.

<p align="center">
  <img src="doc/jutella-conversations.webp" width="400" height="487" alt="Chat with Jutella in Conversations">
</p>

## Features

1. Allowlist of user JIDs with wildcards.
2. Rolling context window with min/max token limits (see the configuration parameter descriptions for details).
3. Presence, read receipts & composing notifications.
4. Image & PDF file attachments.
5. Image generation & editing (only with OpenRouter, no support for multi-turn conversations).
6. Voice message recognition.

## Installation

### Install the executable

1. Install `cargo` from https://rustup.rs/.
2. Install `jutella-xmpp` from [crates.io](https://crates.io/crates/jutella-xmpp) with `cargo install jutella-xmpp`.  
   The executable will be installed as `$HOME/.cargo/bin/jutellaxmpp`.
3. Alternatively, clone the repo and build the executable with `cargo build --release`. The resulting executable will be `target/release/jutellaxmpp`.
4. Copy the executable to `/usr/local/bin`.

### Create a user to run the daemon

```bash
sudo useradd --system --shell /sbin/nologin --home-dir /nonexistent jutella
```

### Install the config

1. Copy the [example](https://github.com/dmitry-markin/jutella-xmpp/blob/master/config/jutellaxmpp.toml) configuration to `/etc`.
2. Make it readable by the `jutella` group:  
   ```bash
   sudo chmod 640 /etc/jutellaxmpp.toml
   sudo chown root:jutella /etc/jutellaxmpp.toml
   ```
3. Edit the configuration file to suit your setup.

### Install the systemd service

1. Copy the systemd [service](https://github.com/dmitry-markin/jutella-xmpp/blob/master/systemd/jutellaxmpp.service) file to `/etc/systemd/system`.
2. Start the service and enable it to run on system startup: `sudo systemctl enable --now jutellaxmpp.service`.
