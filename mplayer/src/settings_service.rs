use mplayer_core::*;
use std::time::Duration;
use tokio::sync::watch;
use tracing::Instrument;

pub(super) async fn restore(store: &Store) -> Result<SettingsSnapshot> {
    let volume = store
        .get_setting("volume")
        .await?
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    let device = store
        .get_setting("output_device")
        .await?
        .filter(|value| !value.is_empty());
    Ok(SettingsSnapshot {
        volume,
        device,
        error: None,
    })
}

pub(super) async fn run(
    store: Store,
    mut desired: watch::Receiver<SettingsUpdate>,
    output: watch::Sender<SettingsSnapshot>,
) -> Result<()> {
    let mut state = output.borrow().clone();
    let mut deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    let mut closing = false;
    loop {
        let update = desired.borrow_and_update().clone();
        let volume_dirty = state.volume != update.volume.payload;
        let device_dirty = state.device != update.device.payload;
        if device_dirty || (volume_dirty && (closing || tokio::time::Instant::now() >= deadline)) {
            let result: Result<()> = async {
                if device_dirty {
                    store
                        .set_setting(
                            "output_device",
                            update.device.payload.as_deref().unwrap_or(""),
                        )
                        .instrument(update.device.trace.span.clone())
                        .await
                        .inspect_err(|error| update.device.trace.failed("settings", error))?;
                    state.device = update.device.payload;
                    update.device.trace.event("settings", "persisted_device");
                }
                if volume_dirty && (closing || tokio::time::Instant::now() >= deadline) {
                    store
                        .set_setting("volume", &update.volume.payload.to_string())
                        .instrument(update.volume.trace.span.clone())
                        .await
                        .inspect_err(|error| update.volume.trace.failed("settings", error))?;
                    state.volume = update.volume.payload;
                    update.volume.trace.event("settings", "persisted_volume");
                }
                Ok(())
            }
            .await;
            state.error = result.as_ref().err().map(ToString::to_string);
            if let Err(error) = &result {
                tracing::warn!(%error, "could not save settings");
            }
            publish_changed(&output, &state);
            if closing {
                return result;
            }
            deadline = tokio::time::Instant::now() + Duration::from_millis(500);
        } else if closing {
            return Ok(());
        }
        tokio::select! {
            changed = desired.changed() => { closing = changed.is_err(); }
            _ = tokio::time::sleep_until(deadline), if state.volume != desired.borrow().volume.payload || state.device != desired.borrow().device.payload => {}
        }
    }
}
