//! Stream Deck plugin for Blackmagic ATEM control.

use streamdeck_plugin::Plugin;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    streamdeck_atem::actions::force_link();
    let state = streamdeck_atem::runtime::AppState::default();
    Plugin::builder(std::env::args())
        .state(state.clone())
        .lifecycle(streamdeck_atem::runtime::GlobalHook { state })
        .add_registered_actions()
        .run()
        .await?;
    Ok(())
}
