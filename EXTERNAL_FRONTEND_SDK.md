# External frontend host API

This experimental API lets a thin desktop binary use Jellium as its native
window and playback runtime while displaying a separately hosted web frontend.
It does not expose the mpv bridge, Jellyfin credentials, settings, filesystem,
or arbitrary native commands to that frontend.

## Rust host

The embedding binary enables `jfn-rust`'s opt-in `external-frontend` feature
and configures one frontend:

```rust
use jfn_rust::{ExternalFrontend, HostOptions};

fn main() {
    let frontend = ExternalFrontend::new("https://media.example.com")
        .expect("valid external frontend URL");
    let options = HostOptions::with_external_frontend(frontend);
    std::process::exit(jfn_rust::app::jfn_app_main_with(options));
}
```

See `jfn_rust/examples/external_frontend.rs` for a buildable example. The
workspace crates are currently `publish = false`, so a downstream project must
use a pinned Git dependency or workspace checkout with
`features = ["external-frontend"]`.

Use distinct `--config-dir` and `--cache-dir` values when packaging a separate
application. The prototype otherwise inherits Jellium's paths, command-line
options, packaging, and platform runtime.

## Web contract

The configured page receives exactly one frozen API:

```js
if (window.jelliumHost) {
    const requestId = crypto.randomUUID();
    window.jelliumHost.playItem(requestId, jellyfinItemId);
}
```

The browser process accepts this call only from the exact HTTP(S) origin of the
configured start URL. Item IDs are length- and character-restricted before they
are forwarded.

The request is executed inside the existing, authenticated Jellyfin Web layer:

1. The external frontend requests an item ID.
2. Jellyfin Web's playback manager resolves the media source and starts the
   existing native mpv player.
3. Jellium keeps the external surface visible until native playback reports it
   has started, then reveals mpv without exposing the private web controller.
4. The existing Jellyfin/player layer remains responsible for playback UI.
5. Jellium restores the external surface when playback stops.

## Intentional limits

- One external frontend and one allowlisted origin per process.
- No direct stream URLs or mpv commands.
- No Jellyfin access token transfer to the external page.
- No arbitrary JavaScript evaluation requested by the external page.
- No external navigation or general plugin system.
- The embedding host can provide `HostAuthService` to install an authenticated
  private Jellyfin session without persisting a second token. Playback remains
  unavailable until that session reports matching server and user readiness.
- `clearSession(requestId)` clears the private identity and pending playback on
  logout or account change.

This boundary is intentionally smaller than a general-purpose Jellium SDK. It
exists only to support an external discovery/request UI followed by native
Jellyfin playback in the same window.
