# streamdeck-atem-rs

Cross-platform Stream Deck plugin for **Blackmagic ATEM** control. Built with Rust.

- **ATEM connection window** – switchers and groups are edited in a separate window, as in the HyperDeck, vMix, and OBS plugins. Each switcher keeps a supervised session with reconnect backoff.
- **Multiple dedicated actions** – Cut, Auto, Program, Preview, Cut to Black, Fade to Black, AUX, downstream keyer controls, and media player source selection.
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

1. Add one of the **ATEM actions** (Cut, Auto, Program, Preview, Cut to Black, Fade to Black, AUX, DSK, Media Player) to your Stream Deck profile.
2. Open the **Property Inspector** for that key.
3. Choose **Send to**. The list contains every switcher and group, plus **Multiple switchers** when a key should hit more than one. Open **Manage instances** to add switchers (name, host, port) and groups in a separate window.
4. For **Program/Preview**, AUX, and DSK source actions, also set the **Video source**. While a switcher is connected, each menu lists sources that switcher reports for that bus: program and preview use the selected M/E, downstream keyers use key sources, and an AUX bus uses sources flagged for that auxiliary output. Names come from the ATEM. With no switcher connected, program, preview, and downstream-keyer menus show the ATEM Mini set: one M/E, Black, color bars, Color 1 and 2, Media Player 1 and its key, and HDMI inputs 1–4. The disconnected AUX list is what an ATEM Mini HDMI output can take: HDMI inputs 1–4, Input 1 Direct, and M/E 1 program and preview. Use one key per input you care about.
5. Each switcher shows Connecting, Disabled, Connected, or the session error in the configuration window. The session stays up when the key leaves the profile. A key saved with only an ATEM IP still talks to that address.
6. Press the key to run the configured action on that ATEM.
7. For Program/Preview actions, key colours follow the ATEM:
   - Program Input keys: active input = red, inactive = gray.
   - Preview Input keys: active input = green, inactive = gray.
8. **Media Player** actions set what a media player plays. Pick the player, the bank (**Still** or **Clip**), and the slot. Both still and clip slots are labelled with the name typed on the ATEM and update as soon as the switcher reports them. The key and the encoder touch strip show the current slot name. Turning the dial steps to the next or previous still (or clip), skipping unused slots, and pressing loads the configured slot.
   - Stream Deck clips the encoder touch strip at about 16 characters. Long still names are shortened with `…`.

## License

Apache-2.0.
