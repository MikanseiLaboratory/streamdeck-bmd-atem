## Project overview

`streamdeck-atem-rs` is a cross‑platform Stream Deck plugin, written in Rust, for controlling Blackmagic ATEM switchers.

Key characteristics:

- Uses [`necromancer`](https://github.com/micolous/necromancer) for ATEM protocol and state management.
- Uses [`StreamDeckPluginRust`](https://github.com/MikanseiLaboratory/StreamDeckPluginRust) (`streamdeck-plugin`) for the Stream Deck WebSocket host, action registry, and TypeScript contract export.
- The Property Inspector is a React app in `pi/`, built with [`@mikanseilaboratory/streamdeck-pi-client`](https://github.com/MikanseiLaboratory/streamdeck-pi-client). `cargo run --bin typegen` writes `pi/src/generated/contracts.ts`. Vite emits `dev.miseinul.atem.sdPlugin/ui/`.
- Caches ATEM connections per IP address so that multiple keys sharing the same ATEM only create a single controller instance.
- Pushes ATEM state (at least connectivity and bus selection) into the Property Inspector and uses it to drive key images.

## Actions and settings

The plugin exposes multiple dedicated Stream Deck actions (see `dev.miseinul.atem.sdPlugin/manifest.json`):

- `dev.miseinul.atem.cut` – ATEM Cut
- `dev.miseinul.atem.auto` – ATEM Auto
- `dev.miseinul.atem.programinput` – ATEM Program Input
- `dev.miseinul.atem.previewinput` – ATEM Preview Input
- `dev.miseinul.atem.cuttoblack` – ATEM Cut to Black
- `dev.miseinul.atem.fadetoblack` – ATEM Fade to Black

Every action shares `ActionSettings` in `src/contracts.rs`. Field names stay snake_case so existing profiles keep working:

- `atem_ip: Option<String>` – ATEM IP address.
- `me: Option<u8>` – Mix Effect index.
- `video_source: Option<u16>` – numeric video source ID (Program, Preview, AUX, DSK cut/fill).
- `aux_bus: Option<u8>` – AUX bus index.
- `key: Option<u8>` – downstream keyer index.
- `on_air: Option<bool>` – explicit DSK on-air target. `None` toggles.

The same struct is exported to TypeScript. The inspector shows only the fields the current action UUID needs. Legacy string values from the old `<select>` controls still deserialize.

## Runtime architecture (Rust side)

`src/main.rs` starts `streamdeck_plugin::Plugin` with `AppState` and the actions registered by `#[streamdeck_action]`. `src/lib.rs` is shared with the `typegen` binary so those registrations are linked.

`AppState` (`src/runtime.rs`) owns:

- one `AtemController` per ATEM IP
- the live key registry (context, action UUID, `ActionSettings`)
- which inspector contexts are open, and which IP they watch
- the plugin `CommandSender`, captured in `PluginService::start`

`connect` returns the cached controller or opens UDP port 9910 and spawns a task on `state_update_events()`. Dropping the cache entry drops the stop sender, which ends that task and releases the socket. When the last key for an IP disappears, the controller is evicted and open inspectors for that IP are told the connection is down.

State updates still drive key images:

- `PREVIEW_OR_PROGRAM_SOURCE` refreshes Program and/or Preview keys
- `TRANSITION_POSITION` refreshes Auto keys
- `DSK_STATE` refreshes DSK On Air and DSK Auto keys

Inspector messages use `type`:

- plugin → inspector: `atemState` (`connected`) and `options` (`kind`, `items`, `connected`)
- inspector → plugin: `requestOptions` (`kind` is `me`, `video`, `aux`, `auxVideo`, or `dsk`)

### Mapping actions to ATEM commands

`src/actions/mod.rs` registers one struct per manifest UUID. Keypad and dial presses call the same `AppState` method:

- Cut, Auto, Cut to Black, Fade to Black → `press_transition` (`cut`, `auto`, `cut_black`, `toggle_auto_black`)
- Program / Preview → `press_bus` (`set_program_input` / `set_preview_input`)
- AUX → `press_aux` (`set_aux_source`); dial rotation calls `rotate_aux` and `ctx.update_settings`
- DSK Auto / On Air / Cut / Fill → `press_dsk`

`video_source` IDs go through `VideoSource::from_u16`, falling back to `VideoSource::Unknown`. AUX and DSK show an alert when the IP is empty.

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

Settings round-trip through `useSettings`. Dynamic lists are not sdpi-components datasources anymore: the page sends `requestOptions`, and the plugin answers with the current ATEM topology (or a fallback list when the switcher is not connected yet). Connection status uses `atemState`.

## Notes for future work

- SuperSource actions remain open in issue #8. Confirm necromancer support before adding them.
- `Plugin::builder` panics when Stream Deck did not pass launch arguments. ATEM connection failures are logged and the plugin keeps running.
- Key colours live in the `Runtime::refresh_*` methods.

