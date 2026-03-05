## Project overview

`streamdeck-atem-rs` is a cross‑platform Stream Deck plugin, written in Rust, for controlling Blackmagic ATEM switchers.

Key characteristics:

- Uses [`necromancer`](https://github.com/micolous/necromancer) for ATEM protocol and state management.
- Uses a custom `streamdeck-rs` fork for the Stream Deck WebSocket protocol.
- Caches ATEM connections per IP address so that multiple keys sharing the same ATEM only create a single controller instance.
- Pushes ATEM state (at least connectivity and bus selection) into the Property Inspector and uses it to drive key images.

## Actions and settings

The plugin exposes multiple dedicated Stream Deck actions (see `dev.miseinul.atem.sdPlugin/manifest.json`):

- `dev.miseinul.atem.cut` – ATEM Cut
- `dev.miseinul.atem.auto` – ATEM Auto
- `dev.miseinul.atem.program-input` – ATEM Program Input
- `dev.miseinul.atem.preview-input` – ATEM Preview Input
- `dev.miseinul.atem.cut-black` – ATEM Cut to Black
- `dev.miseinul.atem.fade-to-black` – ATEM Fade to Black

Each action has its own Property Inspector HTML (`PropertyInspector/*.html`) but they all bind the same per‑action settings struct on the Rust side:

- `atem_ip: Option<String>` – ATEM IP address.
- `me: Option<u8>` – Mix Effect index.
- `video_source: Option<u16>` – numeric video source ID (used by Program/Preview actions).

The PI and `ActionSettings` field names are intentionally kept in sync.

## Runtime architecture (Rust side)

The main executable (`src/main.rs`) is structured around `streamdeck-rs::StreamDeckSocket`:

- A `tokio` main loop reads `Message<GlobalSettings, ActionSettings, PIMessageIn>` from the socket and dispatches into `handle_message`.
- Outgoing messages (including `SendToPropertyInspector`, `LogMessage`, and `SetImage`) are built as `MessageOut<GlobalSettings, ActionSettings, PIMessageOut>`.

Important internal types:

- `GlobalSettings` – currently holds an optional global `atem_ip` (not central to the logic).
- `ActionSettings` – per‑key configuration (see above).
- `PiSubscriptions` – tracks which (action, context) pairs are subscribed to state updates for a given ATEM IP (used for PI updates).
- `KeyConfig` – in‑memory registry of keys (context, action UUID, device, and latest `ActionSettings`).
- `LogRequest` / `SetImageRequest` – messages sent over `mpsc::UnboundedSender` channels to decouple logging and image updates from the main loop.

The plugin maintains:

- `atem_cache: Arc<RwLock<HashMap<String, Arc<AtemController>>>>` – one `AtemController` per ATEM IP.
- `pi_subscriptions: Arc<RwLock<PiSubscriptions>>` – maps ATEM IPs to the PIs that should receive state.
- `key_registry: Arc<RwLock<HashMap<String, KeyConfig>>>` – keyed by Stream Deck `context`, used for key image updates.

### ATEM connection and state

`get_or_connect_atem` is responsible for:

- Returning an existing `AtemController` from `atem_cache` when available.
- Otherwise, creating a new `AtemController::connect_udp(addr, true)`, inserting it into `atem_cache`, and spawning a background task:
  - `forward_atem_state_to_pi(ip, controller, pi_tx, pi_subscriptions, key_registry, image_tx)`

`forward_atem_state_to_pi`:

- Subscribes to ATEM state updates with `controller.state_update_events()`, which yields `(CowCellReadTxn, StateUpdate)`.
- For every update:
  - Notifies subscribed PIs (`SendToPropertyInspector` with `event: "atem_state", connected: true`).
  - When `StateUpdate::PREVIEW_OR_PROGRAM_SOURCE` is set:
    - Calls `controller.get_state().await` to obtain an `AtemState` snapshot.
    - Delegates to `update_program_preview_key_images_from_state(&ip, state, &key_registry, &image_tx)`.

### Mapping actions to ATEM commands

High‑level actions are represented by the `AtemAction` enum:

- `Cut { me }`
- `Auto { me }`
- `ProgramInput { me, source: VideoSource }`
- `PreviewInput { me, source: VideoSource }`
- `CutBlack { me }`
- `FadeToBlack { me }`

They are built from a Stream Deck action UUID and `video_source` via:

- `build_atem_action(action_uuid: &str, me: u8, video_source_id: u16) -> AtemAction`
  - Uses `VideoSource::from_u16(video_source_id).unwrap_or(VideoSource::Unknown)` for Program/Preview.

Execution is encapsulated in:

- `impl AtemAction { async fn execute(&self, controller: &AtemController) -> Result<(), necromancer::Error> }`
  - Calls the appropriate `AtemController` method (`cut`, `auto`, `set_program_input`, `set_preview_input`, `cut_black`, `toggle_auto_black`).

`handle_message`’s `Message::KeyDown` branch:

- Logs the event.
- Extracts `ActionSettings` and resolves `atem_ip`.
- Calls `get_or_connect_atem` to obtain an `AtemController` (and ensure the state‑forwarding task is running for that IP).
- Builds an `AtemAction` from the UUID and settings and calls `execute`.

## Program/Preview key colour behaviour

The plugin deliberately avoids keeping its own notion of which input is on Program/Preview; instead, it always defers to ATEM state:

- A background task listens to `state_update_events`.
- When `StateUpdate::PREVIEW_OR_PROGRAM_SOURCE` changes:
  - It pulls an `AtemState` snapshot from `AtemController`.
  - It updates key images **solely** based on ATEM’s reported bus selection (`get_program_source(me)` / `get_preview_source(me)`).

`update_program_preview_key_images_from_state` performs the mapping:

- Filters `key_registry` for keys:
  - Whose `settings.atem_ip` matches the ATEM IP.
- Whose `action` UUID is `dev.miseinul.atem.program-input` or `dev.miseinul.atem.preview-input`.
- For each such key:
  - Interprets the configured `video_source` as a `VideoSource`.
  - Compares it against `AtemState::get_program_source(me)` or `get_preview_source(me)`.
  - Sends a `SetImage` message via `image_tx` with either a red or green solid PNG data URL:
    - Program keys: active = red, inactive = green.
    - Preview keys: active = green, inactive = red.

Images are small 1×1 PNGs embedded as base64 string constants and wrapped as `data:image/png;base64,...` URLs.

## Property Inspector design

There is one PI HTML file per action in `dev.miseinul.atem.sdPlugin/PropertyInspector/`:

- `cut.html`
- `auto.html`
- `program_input.html`
- `preview_input.html`
- `cut_black.html`
- `fade_to_black.html`

All of them:

- Use `sdpi-components` for markup.
- Bind `atem_ip`, `me`, and `video_source` settings (the Rust side ignores `video_source` for non Program/Preview actions).
- Display a simple connection status label that is updated via `SendToPropertyInspector` messages from the plugin (`event: "atem_state"`).

This setup makes it easy to evolve each action’s UI independently while keeping a single Rust `ActionSettings` type.

## Notes for future work

- Additional ATEM features (aux buses, keyers, FTB status, etc.) are available in `AtemState` and could drive more actions or key visualisation.
- Error handling is intentionally conservative: connection / registration failures currently just `exit(1)`.
- Program/Preview colour rules are hard‑coded; if a different scheme is desired, it can be centralized in `update_program_preview_key_images_from_state`.

