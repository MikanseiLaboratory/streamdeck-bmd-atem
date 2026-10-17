# streamdeck-atem-rs

Cross-platform Stream Deck plugin for **Blackmagic ATEM** control. Built with Rust.

- **ATEM connection cache by IP** – one connection per switcher IP, reused across keys.
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
3. Set **ATEM IP** (e.g. `192.168.1.100`) and the action's bus (ME, AUX, or DSK).
4. For **Program/Preview**, AUX, and DSK source actions, also set the **Video source**. Use one key per input you care about.
5. The status line shows whether the plugin considers the ATEM connected; connection state is sent to the PI by context.
6. Press the key to run the configured action on that ATEM.
7. For Program/Preview actions, key colours follow the ATEM:
   - Program Input keys: active input = red, inactive = gray.
   - Preview Input keys: active input = green, inactive = gray.

## License

Apache-2.0.
