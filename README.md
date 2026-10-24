# streamdeck-atem-rs

Cross-platform Stream Deck plugin for **Blackmagic ATEM** control. Built with Rust.

- **ATEM connection window** – switchers and groups are edited in a separate window, as in the HyperDeck, vMix, and OBS plugins. Each switcher keeps a supervised session with reconnect backoff.
- **Multiple dedicated actions** – Cut, Auto, Program, Preview, Cut to Black, Fade to Black, AUX, and downstream keyer controls.
- **Typed Property Inspector** – one React inspector, generated from the Rust settings contract, sets the ATEM IP and the fields each action needs.
- **Live state in PI** – ATEM connection status and input lists are pushed to the open inspector.
- **Program/Preview key colours** – Program/Preview action keys change colour based on the *actual* ATEM bus state (Program vs Preview), using ATEM state rather than local plugin state.

## Build

The plugin host is [`StreamDeckPluginRust`](https://github.com/MikanseiLaboratory/StreamDeckPluginRust). Property Inspector contracts are generated from the Rust settings types.

```bash
cargo test --lib
cargo run --bin typegen
cd pi && npm install && npm run build
```

`make pack` builds the native binary and the inspector, then packs `dev.miseinul.atem.sdPlugin`.

## Usage

1. Add one of the **ATEM actions** (Cut, Auto, Program, Preview, Cut to Black, Fade to Black, AUX, DSK) to your Stream Deck profile.
2. Open the **Property Inspector** for that key.
3. Choose **Send to**. The list contains every switcher and group, plus **Multiple switchers** when a key should hit more than one. Open **Manage instances** to add switchers (name, host, port) and groups in a separate window.
4. For **Program/Preview**, AUX, and DSK source actions, also set the **Video source**. The list includes Black, color bars, color generators, and media players. While the switcher is connected those rows, including their names, come from the ATEM. With no switcher connected, the list is the ATEM Mini set: one M/E, Black, color bars, Color 1 and 2, Media Player 1 and its key, and HDMI inputs 1–4. Use one key per input you care about.
5. Each switcher shows Connecting, Disabled, Connected, or the session error in the configuration window. The session stays up when the key leaves the profile. A key saved with only an ATEM IP still talks to that address.
6. Press the key to run the configured action on that ATEM.
7. For Program/Preview actions, key colours follow the ATEM:
   - Program Input keys: active input = red, inactive = gray.
   - Preview Input keys: active input = green, inactive = gray.

## License

Apache-2.0.
