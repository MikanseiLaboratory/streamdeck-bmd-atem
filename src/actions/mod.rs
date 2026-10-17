//! Stream Deck actions. Each UUID is registered through `#[streamdeck_action]`.

use streamdeck_plugin::{ActionContext, ActionPayload, DialRotatePayload, Result};

use crate::contracts::ActionSettings;
use crate::runtime::{AppState, DskKind, TransitionKind};

macro_rules! define_action {
    ($name:ident, $uuid:literal, $press:path) => {
        define_action!($name, $uuid, $press, ignore_dial);
    };
    ($name:ident, $uuid:literal, $press:path, $rotate:path) => {
        #[derive(Default)]
        pub struct $name;

        #[streamdeck_plugin::streamdeck_action(
                                            uuid = $uuid,
                                            settings = crate::contracts::ActionSettings,
                                            state = crate::runtime::AppState,
                                        )]
        impl streamdeck_plugin::Action for $name {
            async fn on_will_appear(
                &mut self,
                _payload: &ActionPayload,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                ctx.state()
                    .on_will_appear(
                        &ctx.identity().action_id,
                        &ctx.identity().context,
                        &ctx.settings(),
                    )
                    .await;
                Ok(())
            }

            async fn on_will_disappear(
                &mut self,
                _payload: &ActionPayload,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                ctx.state().on_will_disappear(&ctx.identity().context).await;
                Ok(())
            }

            async fn on_settings_changed(
                &mut self,
                _previous: &ActionSettings,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                ctx.state()
                    .on_settings_changed(
                        &ctx.identity().action_id,
                        &ctx.identity().context,
                        &ctx.settings(),
                    )
                    .await;
                Ok(())
            }

            async fn on_property_inspector_did_appear(
                &mut self,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                ctx.state()
                    .on_property_inspector_did_appear(
                        &ctx.identity().action_id,
                        &ctx.identity().context,
                        &ctx.settings(),
                    )
                    .await;
                Ok(())
            }

            async fn on_property_inspector_did_disappear(
                &mut self,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                ctx.state()
                    .on_property_inspector_did_disappear(&ctx.identity().context)
                    .await;
                Ok(())
            }

            async fn on_property_inspector_message(
                &mut self,
                payload: &serde_json::Value,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                ctx.state()
                    .on_property_inspector_message(&ctx.identity().context, payload)
                    .await;
                Ok(())
            }

            async fn on_key_down(
                &mut self,
                _payload: &ActionPayload,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                $press(ctx).await
            }

            async fn on_dial_down(
                &mut self,
                _payload: &ActionPayload,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                $press(ctx).await
            }

            async fn on_dial_rotate(
                &mut self,
                payload: &DialRotatePayload,
                ctx: &ActionContext<'_, ActionSettings, AppState>,
            ) -> Result<()> {
                $rotate(ctx, payload).await
            }
        }
    };
}

define_action!(CutAction, "dev.miseinul.atem.cut", press_cut);
define_action!(AutoAction, "dev.miseinul.atem.auto", press_auto);
define_action!(
    ProgramInputAction,
    "dev.miseinul.atem.programinput",
    press_program
);
define_action!(
    PreviewInputAction,
    "dev.miseinul.atem.previewinput",
    press_preview
);
define_action!(
    CutBlackAction,
    "dev.miseinul.atem.cuttoblack",
    press_cut_black
);
define_action!(
    FadeToBlackAction,
    "dev.miseinul.atem.fadetoblack",
    press_fade_to_black
);
define_action!(
    AuxOutputAction,
    "dev.miseinul.atem.auxoutput",
    press_aux,
    rotate_aux
);
define_action!(DskAutoAction, "dev.miseinul.atem.dskauto", press_dsk_auto);
define_action!(
    DskOnAirAction,
    "dev.miseinul.atem.dskonair",
    press_dsk_on_air
);
define_action!(DskCutAction, "dev.miseinul.atem.dskcut", press_dsk_cut);
define_action!(DskFillAction, "dev.miseinul.atem.dskfill", press_dsk_fill);

/// Keep action modules linked so `inventory` registrations are not stripped.
pub fn force_link() {
    let _ = std::any::type_name::<CutAction>();
    let _ = std::any::type_name::<AutoAction>();
    let _ = std::any::type_name::<ProgramInputAction>();
    let _ = std::any::type_name::<PreviewInputAction>();
    let _ = std::any::type_name::<CutBlackAction>();
    let _ = std::any::type_name::<FadeToBlackAction>();
    let _ = std::any::type_name::<AuxOutputAction>();
    let _ = std::any::type_name::<DskAutoAction>();
    let _ = std::any::type_name::<DskOnAirAction>();
    let _ = std::any::type_name::<DskCutAction>();
    let _ = std::any::type_name::<DskFillAction>();
}

async fn ignore_dial(
    _ctx: &ActionContext<'_, ActionSettings, AppState>,
    _payload: &DialRotatePayload,
) -> Result<()> {
    Ok(())
}

async fn press_cut(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_transition(
            &ctx.identity().context,
            &ctx.settings(),
            TransitionKind::Cut,
        )
        .await;
    Ok(())
}

async fn press_auto(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_transition(
            &ctx.identity().context,
            &ctx.settings(),
            TransitionKind::Auto,
        )
        .await;
    Ok(())
}

async fn press_cut_black(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_transition(
            &ctx.identity().context,
            &ctx.settings(),
            TransitionKind::CutBlack,
        )
        .await;
    Ok(())
}

async fn press_fade_to_black(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_transition(
            &ctx.identity().context,
            &ctx.settings(),
            TransitionKind::FadeToBlack,
        )
        .await;
    Ok(())
}

async fn press_program(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_bus(&ctx.identity().context, &ctx.settings(), true)
        .await;
    Ok(())
}

async fn press_preview(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_bus(&ctx.identity().context, &ctx.settings(), false)
        .await;
    Ok(())
}

async fn press_aux(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_aux(&ctx.identity().context, &ctx.settings())
        .await;
    Ok(())
}

async fn rotate_aux(
    ctx: &ActionContext<'_, ActionSettings, AppState>,
    payload: &DialRotatePayload,
) -> Result<()> {
    if let Some(updated) = ctx
        .state()
        .rotate_aux(
            &ctx.identity().context,
            &ctx.settings(),
            i64::from(payload.ticks),
        )
        .await
    {
        ctx.update_settings(|settings| *settings = updated)?;
    }
    Ok(())
}

async fn press_dsk_auto(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_dsk(&ctx.identity().context, &ctx.settings(), DskKind::Auto)
        .await;
    Ok(())
}

async fn press_dsk_on_air(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_dsk(&ctx.identity().context, &ctx.settings(), DskKind::OnAir)
        .await;
    Ok(())
}

async fn press_dsk_cut(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_dsk(&ctx.identity().context, &ctx.settings(), DskKind::Cut)
        .await;
    Ok(())
}

async fn press_dsk_fill(ctx: &ActionContext<'_, ActionSettings, AppState>) -> Result<()> {
    ctx.state()
        .press_dsk(&ctx.identity().context, &ctx.settings(), DskKind::Fill)
        .await;
    Ok(())
}
