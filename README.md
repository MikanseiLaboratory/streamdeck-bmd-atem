# streamdeck-atem-rs

Cross-platform Stream Deck plugin for **Blackmagic ATEM** control. Built with Rust.

- **ATEM connection cache by IP** – one connection per switcher IP, reused across keys.
- **Property Inspector** – set ATEM IP, action type (cut, auto, program/preview), ME index, and video source.
- **Live state in PI** – ATEM connection status (and state updates) are pushed to the Property Inspector by context.
- **Key actions** – KeyDown runs the chosen action (cut, auto, program input, preview input, cut to black, fade to black) on the configured ATEM.

## Crates

- [streamdeck-rs](https://github.com/mdonoughe/streamdeck-rs) – Stream Deck plugin SDK (WebSocket, registration, messages).
- [necromancer](https://github.com/micolous/necromancer) – Blackmagic ATEM control protocol (UDP BURP on port 9910).

## Build

```bash
cargo build --release
```

Output: `target/release/streamdeck-atem.exe` (Windows) or `target/release/streamdeck-atem` (macOS/Linux).

## Install the plugin

1. **Build** the binary (see above).

2. **Plugin folder**  
   Use the folder `com.my.atem.sdPlugin` in this repo. It contains:
   - `manifest.json`
   - `PropertyInspector/index.html`
   - `Images/` (see below)

3. **Copy the binary** into the plugin folder:
   - **Windows:** copy `target/release/streamdeck-atem.exe` into `com.my.atem.sdPlugin/`.
   - **macOS/Linux:** copy `target/release/streamdeck-atem` into `com.my.atem.sdPlugin/` and ensure it is executable (`chmod +x streamdeck-atem`).

4. **Icons**  
   Add PNG icons in `com.my.atem.sdPlugin/Images/`:
   - `action-icon.png` – key icon (e.g. 72×72 or 144×144).
   - `plugin-icon.png` – plugin icon in the actions list.  
   If these are missing, Stream Deck may use a default or show an error; see `com.my.atem.sdPlugin/Images/README.md`.

5. **Install into Stream Deck**  
   Copy the whole `com.my.atem.sdPlugin` folder into the Stream Deck plugins directory:
   - **Windows:** `%APPDATA%\Elgato\StreamDeck\Plugins\`
   - **macOS:** `~/Library/Application Support/com.elgato.StreamDeck/Plugins/`

6. **Restart Stream Deck** (or reload plugins if supported). The action **“ATEM Action”** should appear; add it to a key and open the Property Inspector to set ATEM IP and action type.

## Usage

1. Add an **ATEM Action** key to your Stream Deck profile.
2. Open the **Property Inspector** for that key.
3. Set **ATEM IP** (e.g. `192.168.1.100`), **Action type** (Cut, Auto, Program input, Preview input, etc.), **ME** index, and **Video source** (for program/preview).
4. The status line shows whether the plugin considers the ATEM connected; state updates are sent to the PI by context.
5. Press the key to run the chosen action on that ATEM.

## License

Apache-2.0.
