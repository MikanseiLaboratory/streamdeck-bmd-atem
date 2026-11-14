## Project overview

`streamdeck-bmd-atem` is a cross‑platform Stream Deck plugin, written in Rust, for controlling Blackmagic ATEM switchers.

Key characteristics:

- Uses [`necromancer`](https://github.com/MikanseiLaboratory/necromancer) (`feat/streamdeck` @ `c4cfb3c8`) for ATEM protocol and state management. That commit accepts protocol 2.30–2.32 and decodes `MPCS` clip descriptions. The protocol crate is not vendored.
- Uses [`StreamDeckPluginRust`](https://github.com/MikanseiLaboratory/StreamDeckPluginRust) (`streamdeck-plugin`) for the Stream Deck WebSocket host, action registry, and TypeScript contract export.
- The Property Inspector is a React app in `pi/`, built with [`@mikanseilaboratory/streamdeck-pi-client`](https://github.com/MikanseiLaboratory/streamdeck-pi-client). `cargo run --bin typegen` writes `pi/src/generated/contracts.ts`. Vite emits `dev.miseinul.atem.sdPlugin/ui/`.
- Keeps one supervised ATEM session per catalog entry (`src/pool.rs`). Instances and groups are edited in a separate configuration window, the same way as the HyperDeck, vMix, and OBS plugins. Each entry is Disabled, Connecting, Connected, or Disconnected with a message.
- Pushes ATEM state (at least connectivity and bus selection) into the Property Inspector and uses it to drive key images.

## Actions and settings

The plugin exposes multiple dedicated Stream Deck actions (see `dev.miseinul.atem.sdPlugin/manifest.json`):

- `dev.miseinul.atem.cut` – ATEM Cut
- `dev.miseinul.atem.auto` – ATEM Auto
- `dev.miseinul.atem.programinput` – ATEM Program Input
- `dev.miseinul.atem.previewinput` – ATEM Preview Input
- `dev.miseinul.atem.cuttoblack` – ATEM Cut to Black
- `dev.miseinul.atem.fadetoblack` – ATEM Fade to Black
- `dev.miseinul.atem.auxoutput` – ATEM AUX Output (keypad + dial)
- `dev.miseinul.atem.dskauto` / `dskonair` / `dskcut` / `dskfill` – downstream keyer
- `dev.miseinul.atem.mediaplayer` – ATEM Media Player (keypad + dial)

Every action shares `ActionSettings` in `src/contracts.rs`. Field names stay snake_case so existing profiles keep working:

- `atem_ip: Option<String>` – ATEM IP address.
- `me: Option<u8>` – Mix Effect index.
- `video_source: Option<u16>` – numeric video source ID (Program, Preview, AUX, DSK cut/fill).
- `aux_bus: Option<u8>` – AUX bus index.
- `key: Option<u8>` – downstream keyer index.
- `on_air: Option<bool>` – explicit DSK on-air target. `None` toggles.
- `media_player: Option<u8>` – media player index, `0` being Media Player 1.
- `media_index: Option<u8>` – slot inside the still or clip bank.
- `media_kind: Option<MediaKind>` – `"still"` or `"clip"`. `None` follows the switcher.

The same struct is exported to TypeScript. The inspector shows only the fields the current action UUID needs. Legacy string values from the old `<select>` controls still deserialize.

## Runtime architecture (Rust side)

`src/main.rs` starts `streamdeck_plugin::Plugin` with `AppState` and the actions registered by `#[streamdeck_action]`. `src/lib.rs` is shared with the `typegen` binary so those registrations are linked.

`AppState` (`src/runtime.rs`) owns:

- an `AtemPool` (`src/pool.rs`): one supervisor per catalog instance
- the live key registry (context, action UUID, `ActionSettings`)
- which inspector contexts are open, and which IP they watch
- `GlobalSettings` (`instances`, `seeded`), applied from `didReceiveGlobalSettings`
- the plugin `CommandSender`, captured in `PluginService::start`

`PluginService::start` asks Stream Deck for global settings. `GlobalHook` parses them and calls `AtemPool::reconcile`. The catalog is the source of truth: a supervisor stays up after its keys disappear, a disabled instance does not open a socket, and changing host, port, or `enabled` restarts that slot. A generation counter stops a replaced supervisor from writing status onto the new slot. `revision` stops an older settings echo from putting a deleted switcher back. The first settings read sets `seeded` and does not insert a localhost switcher.

The property inspector opens `configuration.html` in a separate window. That window edits `GlobalSettings.instances` (name, host, port, color, enabled) and `groups`, and can reconnect one switcher or all of them. Each action stores a `target`: all enabled switchers, one group, one switcher, or a list of instance ids. The inspector lists those choices in one **Send to** menu. Multiple selection uses a checkbox whose label is tied to the input, because the Stream Deck stylesheet hides the native box. Commands run on every selected switcher that is `Connected`. A profile that only has `atem_ip` still addresses that one host, and the plugin adds it to the catalog the first time the key appears.

The supervisor calls `AtemController::connect_udp` with necromancer's own reconnect disabled, including after an invalid address, and waits `1s`, `2s`, `4s`, … up to `30s` between attempts. A background task applies pool state events to key images and open inspectors.

State updates still drive key images:

- `PREVIEW_OR_PROGRAM_SOURCE` refreshes Program and/or Preview keys
- `TRANSITION_POSITION` refreshes Auto keys
- `DSK_STATE` refreshes DSK On Air and DSK Auto keys
- `MEDIA_PLAYER_TOPOLOGY` (capabilities, still and clip descriptions, source) rebuilds the media lists and key titles. The switcher sends one `MPfe` per still and one `MPCS` per clip after connecting, so the inspector would otherwise stay on a stale or empty list.

Inspector messages use `type`:

- plugin → inspector: `atemState` (`connected`, `detail`), `status` (`instances` with a tagged `status.kind`), and `options` (`kind`, `items`, `connected`, `detail`)
- inspector → plugin: `ready`, `reconnect` (`id` optional), and `requestOptions` (`kind` is `me`, `video`, `aux`, `auxVideo`, `dsk`, `mediaPlayer`, `mediaBank`, or `mediaIndex`)

### Mapping actions to ATEM commands

`src/actions/mod.rs` registers one struct per manifest UUID. Keypad and dial presses call the same `AppState` method:

- Cut, Auto, Cut to Black, Fade to Black → `press_transition` (`cut`, `auto`, `cut_black`, `toggle_auto_black`)
- Program / Preview → `press_bus` (`set_program_input` / `set_preview_input`)
- AUX → `press_aux` (`set_aux_source`); dial rotation calls `rotate_aux` and `ctx.update_settings`
- DSK Auto / On Air / Cut / Fill → `press_dsk`
- Media Player → `press_media` (`set_media_player_source`); dial rotation calls `rotate_media` and `ctx.update_settings`

`video_source` IDs go through `VideoSource::from_u16`, falling back to `VideoSource::Unknown`. AUX and DSK show an alert when the IP is empty.

### Media player

`press_media` loads `media_index` from the `media_kind` bank, or from the bank the player already uses when `media_kind` is unset. The source is a `MediaPlayerSourceID::{Still, VideoClip}`. necromancer range-checks the player, the bank size, and the still's `is_valid` flag before sending, so an empty slot is refused by the library rather than by the switcher.

Naming comes from the media pool state:

- `media_player_frame_descriptions` carries the still name typed on the ATEM. Slots with `is_valid == false` are labelled `Empty`.
- `media_pool_clip_descriptions` carries the clip name, decoded from `MPCS`. That atom is the only place the protocol sends a clip name: `MPfe` describes stills, and `RCPS` only reports playback state. Slots with `is_used == false` are labelled `Empty`. `MPCS` support comes from necromancer PR MikanseiLaboratory/necromancer#4.
- `media_title` feeds `set_title`, which names the key and the encoder touch strip. It falls back to the player number before the switcher reports.

`rotate_media` steps to the next or previous slot and skips slots the switcher reports as unused. It returns the settings to persist, so the key remembers where the dial landed.

This action deliberately has no tally colour: unlike Program/Preview it names its source instead of showing which bus is live.

## Key colour behaviour

Images come from the bundled files `Images/red.png`, `Images/green.png`, and `Images/gray.png`. The plugin does not keep its own bus state. `Runtime` compares each key's settings with `AtemState`:

- Program: active = red, inactive = gray (`get_program_source`)
- Preview: active = green, inactive = gray (`get_preview_source`)
- Auto: `transition_position.in_progress` = red, otherwise gray
- DSK On Air: `on_air` = red, otherwise gray
- DSK Auto: `in_transition` = red, otherwise gray

A successful Auto or DSK Auto press paints that key red immediately. The next matching state update repaints it from the switcher.

## Property Inspector design

`manifest.json` sets `PropertyInspectorPath` to `ui/index.html`. That bundle is the Vite build of `pi/`. One page serves every action and chooses fields from `actionInfo.action`.

Settings round-trip through `useSettings`. The target picker and **Manage instances** button follow the HyperDeck, vMix, and OBS inspectors: the button opens `ui/configuration.html`, which talks to the inspector through `window.atemConfig`. Dynamic lists are not sdpi-components datasources anymore: the page sends `requestOptions`, and the plugin answers with the current ATEM topology (or a fallback list when the switcher is not connected yet). Program, Preview, AUX, and DSK source lists include Black, color bars, and color generators. A connected switcher supplies those rows from its input properties, including the names stored on the ATEM.

## Notes for future work

- SuperSource actions remain open in issue #8. Confirm necromancer support before adding them.
- `Plugin::builder` panics when Stream Deck did not pass launch arguments. ATEM connection failures are logged and the plugin keeps running.
- Key colours live in the `Runtime::refresh_*` methods.

