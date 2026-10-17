//! Stream Deck plugin for Blackmagic ATEM control.

use streamdeck_plugin::Plugin;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    streamdeck_atem::actions::force_link();
    Plugin::builder(std::env::args())
        .state(streamdeck_atem::runtime::AppState::default())
        .add_registered_actions()
        .run()
        .await?;
    Ok(())
}
