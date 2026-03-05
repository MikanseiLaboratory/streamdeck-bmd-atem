# streamdeck-atem-rs

Cross-platform Stream Deck plugin for **Blackmagic ATEM** control. Built with Rust.

- **ATEM connection cache by IP** – one connection per switcher IP, reused across keys.
- **Property Inspector** – set ATEM IP, action type (cut, auto, program/preview), ME index, and video source.
- **Live state in PI** – ATEM connection status (and state updates) are pushed to the Property Inspector by context.
- **Key actions** – KeyDown runs the chosen action (cut, auto, program input, preview input, cut to black, fade to black) on the configured ATEM.


## Usage

1. Add an **ATEM Action** key to your Stream Deck profile.
2. Open the **Property Inspector** for that key.
3. Set **ATEM IP** (e.g. `192.168.1.100`), **Action type** (Cut, Auto, Program input, Preview input, etc.), **ME** index, and **Video source** (for program/preview).
4. The status line shows whether the plugin considers the ATEM connected; state updates are sent to the PI by context.
5. Press the key to run the chosen action on that ATEM.

## License

Apache-2.0.
