# streamdeck-atem-rs

Cross-platform Stream Deck plugin for **Blackmagic ATEM** control. Built with Rust.

- **ATEM connection cache by IP** – one connection per switcher IP, reused across keys.
- **Multiple dedicated actions** – separate Stream Deck actions for Cut, Auto, Program Input, Preview Input, Cut to Black, and Fade to Black.
- **Per-action Property Inspector** – each action has its own PI page where you set ATEM IP, ME index, and (for Program/Preview) the video source.
- **Live state in PI** – ATEM connection status is pushed to the Property Inspector by context using ATEM state updates.
- **Program/Preview key colours** – Program/Preview action keys change colour based on the *actual* ATEM bus state (Program vs Preview), using ATEM state rather than local plugin state.

## Usage

1. Add one of the **ATEM actions** (ATEM Cut, ATEM Auto, ATEM Program Input, ATEM Preview Input, ATEM Cut to Black, ATEM Fade to Black) to your Stream Deck profile.
2. Open the **Property Inspector** for that key.
3. Set **ATEM IP** (e.g. `192.168.1.100`) and **ME index**.
4. For **Program/Preview** actions, also set the **Video source** (e.g. `1` for Input 1). Use one key per input you care about.
5. The status line shows whether the plugin considers the ATEM connected; connection state is sent to the PI by context.
6. Press the key to run the configured action on that ATEM.
7. For Program/Preview actions, key colours follow the ATEM:
   - Program Input keys: active input = red, inactive = green.
   - Preview Input keys: active input = green, inactive = red.

## License

Apache-2.0.
