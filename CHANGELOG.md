# Changelog

## v0.1.1

### Added
- **Update notice** — a chip in the top bar appears only when a newer release exists on GitHub; clicking it opens the release page.
- **Chat overlay: full Streamlabs Chat Box custom-code support**, verified side by side against a real Streamlabs widget:
  - Custom Fields (JSON) and widget settings (text colour, background colour, font size, hide delay) in the Chat Overlay card.
  - `onLoad` and `onEventReceived` events with the same detail shape and Twitch `tags` Streamlabs passes.
  - Same frame environment: jQuery, animate.css, Open Sans and the base reset CSS.
  - Badges get the `badge <type>-icon` classes; deleted messages get `.deleted`.
- Overlay and dock URLs in the Integrations and Sounds pages are clickable links that open in a new tab.

### Fixed
- **Chat dock sent the same message twice** when Enter was pressed again (or held) while a send was in flight. Only one send runs at a time now, and the box clears immediately.
- **Sounds overlay** no longer draws the small green status text; the browser source is fully transparent.

### Changed
- `/api/health` reports the running `version`.

## v0.1.0

- First public release.
