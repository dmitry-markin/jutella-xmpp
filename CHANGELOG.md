# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.0] - 2025-11-02

This is a second release of `jutella-xmpp` and it brings multiple improvements compared to the initial release. User-facing ones include presence, read receipts, and "composing" notifications. Server can now talk to OpenRouter, supporting resoning budget/effort & verbosity settings. Allowed users can now be matched by wildcards (i.e., you can whitelist an entire XMPP domain instead of listing all the individual users). Memory footprint is reduced substantially by sharing a tokenizer across all chat instances.

### Added

- Wildcard user matching ([#11](https://github.com/dmitry-markin/jutella-xmpp/pull/11))
- Support `reasoning_budget` option in OpenRouter API ([#9](https://github.com/dmitry-markin/jutella-xmpp/pull/9))
- Support OpenRouter API, expose `reasoning_effort` & `verbosity` options ([#7](https://github.com/dmitry-markin/jutella-xmpp/pull/7))
- Send read receipts (XMPP "displayed" markers) ([#4](https://github.com/dmitry-markin/jutella-xmpp/pull/4))
- Presence and composing ([#2](https://github.com/dmitry-markin/jutella-xmpp/pull/2))

### Changed

- Increase HTTP timeout 2 min -> 5 min ([#8](https://github.com/dmitry-markin/jutella-xmpp/pull/8))
- Don't send composing notifications if response takes less than 1 sec ([#3](https://github.com/dmitry-markin/jutella-xmpp/pull/3))

### Fixed

- Introduce backpressure instead of dropping messages ([#12](https://github.com/dmitry-markin/jutella-xmpp/pull/12))
- Share tokenizer across instances & expose `http_timeout` setting ([#10](https://github.com/dmitry-markin/jutella-xmpp/pull/10))
- Rate-limit reconnection attempts and don't spam with "disconnected" error ([#6](https://github.com/dmitry-markin/jutella-xmpp/pull/6))

## [0.1.0] - 2024-09-24

Initial release.
