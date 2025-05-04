# jutella-xmpp

[![License](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/dmitry-markin/jutella-xmpp/blob/master/LICENSE) [![crates.io](https://img.shields.io/crates/v/jutella-xmpp.svg)](https://crates.io/crates/jutella-xmpp)

XMPP – OpenAI API bridge based on [tokio-xmpp](https://docs.rs/tokio-xmpp/latest/tokio_xmpp/) and [jutella](https://github.com/dmitry-markin/jutella).

Supports OpenAI and Azure endpoints and implements rolling context window to reduce costs.

## Installation

### Install the executable

1. Install `cargo` from https://rustup.rs/.
2. Install `jutella-xmpp` from [crates.io](https://crates.io/crates/jutella-xmpp) with `cargo install jutella-xmpp`.  
   The executable will be installed as `$HOME/.cargo/bin/jutellaxmpp`.
3. Alternatively, clone the repo and build the executable with `cargo build --release`. The resulting executable will be `target/release/jutellaxmpp`.
4. Copy the executable to `/usr/local/bin`.

### Create a user for runnig the daemon

```bash
sudo useradd --system --shell /sbin/nologin --home-dir /nonexistent jutella
```

### Install the config

1. Copy the config [example](https://github.com/dmitry-markin/jutella-xmpp/blob/master/config/jutellaxmpp.toml) to `/etc`.
2. Make it readable by `jutella` group:  
   ```bash
   sudo chmod 640 /etc/jutellaxmpp.toml
   sudo chown root:jutella /etc/jutellaxmpp.toml
   ```
3. Edit the config to match your configuration.

### Install the systemd service

1. Copy systemd [service](https://github.com/dmitry-markin/jutella-xmpp/blob/master/systemd/jutellaxmpp.service) to `/etc/systemd/system`.
2. Enable it to run on system startup: `sudo systemctl enable jutellaxmpp.service`.
3. Start the service: `sudo systemctl start jutellaxmpp.service`.
