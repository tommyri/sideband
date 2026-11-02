//! Persisted preferences and local-only device checks. No PCM crosses IPC.
use crate::{state::AppState, types::ApiError};
use serde::{Deserialize, Serialize};
use ss_audio::{
    device::{DeviceManager, GraphConfig},
    processing::{Backend, NoiseSuppression, ProcessingConfig},
    vad,
};
use ss_client_core::{AudioConfig, TalkGate, voice::DeviceBackend};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{Emitter, State};
use tauri_plugin_store::StoreExt;
use ts_rs::TS;

fn error(e: impl std::fmt::Display) -> ApiError {
    ApiError::new("audio", e.to_string())
}
#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(default, rename_all = "camelCase")]
#[ts(export, export_to = "../../src/lib/generated/")]
pub struct AudioPreferences {
    pub version: u8,
    pub input: Option<String>,
    pub output: Option<String>,
    pub mode: String,
    pub binding: String,
    pub release_ms: u16,
    pub threshold: f32,
    pub hangover_ms: u16,
    pub echo: bool,
    pub noise: String,
    pub gain: bool,
    pub apple: bool,
    pub master: u16,
    pub cues: u16,
    pub attenuation: u16,
    pub jitter_min: u16,
    pub jitter_max: u16,
}
impl Default for AudioPreferences {
    fn default() -> Self {
        Self {
            version: 1,
            input: None,
            output: None,
            mode: "ptt".into(),
            binding: String::new(),
            release_ms: 150,
            threshold: -40.0,
            hangover_ms: 300,
            echo: false,
            noise: "off".into(),
            gain: false,
            apple: false,
            master: 100,
            cues: 100,
            attenuation: 0,
            jitter_min: 20,
            jitter_max: 200,
        }
    }
}
impl AudioPreferences {
    pub fn config(&self) -> Result<AudioConfig, ApiError> {
        if self.version != 1
            || self.release_ms > 2000
            || self.hangover_ms > 5000
            || !(-96.0..=0.0).contains(&self.threshold)
            || self.master > 200
            || self.cues > 100
            || self.attenuation > 50
            || self.jitter_min < 20
            || self.jitter_min > self.jitter_max
            || self.jitter_max > 200
        {
            return Err(error("Audio preferences are out of range"));
        }
        let gate = match self.mode.as_str() {
            "ptt" => TalkGate::PushToTalk,
            "vad" => TalkGate::VoiceActivation(vad::Config {
                threshold_dbfs: self.threshold,
                hangover: Duration::from_millis(self.hangover_ms.into()),
                ..Default::default()
            }),
            "open" => TalkGate::OpenMic,
            _ => return Err(error("Unknown transmission mode")),
        };
        let noise_suppression = match self.noise.as_str() {
            "off" => NoiseSuppression::Off,
            "sonora" if Backend::Sonora.is_available() => {
                NoiseSuppression::Sonora(Default::default())
            }
            "rnnoise" if Backend::RnNoise.is_available() => NoiseSuppression::RnNoise,
            _ => return Err(error("Noise suppression model unavailable in this build")),
        };
        if (self.echo || self.gain) && !Backend::Sonora.is_available() {
            return Err(error("Software processing unavailable in this build"));
        }
        if self.apple && !cfg!(target_os = "macos") {
            return Err(error("Apple processing requires macOS"));
        }
        if self.apple
            && (self.input.is_some()
                || self.output.is_some()
                || self.echo
                || self.gain
                || self.noise != "off")
        {
            return Err(error(
                "Apple processing requires system defaults and software processing off",
            ));
        }
        let devices = DeviceManager::available_devices();
        let resolve = |id: &Option<String>, input: bool| -> Result<_, ApiError> {
            id.as_ref().map(|id| devices.iter().find(|d| d.id.as_str() == id && if input { d.input } else { d.output }).map(|d| d.id.clone()).ok_or_else(|| error("Selected device is unavailable; choose System default or reconnect it"))).transpose()
        };
        Ok(AudioConfig {
            input_device: resolve(&self.input, true)?,
            output_device: resolve(&self.output, false)?,
            gate,
            release_delay: Duration::from_millis(self.release_ms.into()),
            device_backend: if self.apple {
                DeviceBackend::AppleVoiceProcessing {
                    bypass: false,
                    automatic_gain_control: true,
                }
            } else {
                DeviceBackend::Portable
            },
            processing: ProcessingConfig {
                echo_cancellation: self.echo,
                noise_suppression,
                gain_control: self.gain,
            },
            master_gain: f32::from(self.master) / 100.0,
            cue_gain: f32::from(self.cues) / 100.0,
            attenuation: f32::from(self.attenuation) / 100.0,
            jitter_min: Duration::from_millis(self.jitter_min.into()),
            jitter_max: Duration::from_millis(self.jitter_max.into()),
            ..Default::default()
        })
    }
}
pub fn load(app: &tauri::AppHandle) -> Result<AudioPreferences, ApiError> {
    let store = app
        .store_builder("audio.json")
        .disable_auto_save()
        .build()
        .map_err(error)?;
    store
        .get("preferences")
        .map(serde_json::from_value)
        .transpose()
        .map_err(error)
        .map(|p| p.unwrap_or_default())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioEnvironment {
    preferences: AudioPreferences,
    devices: Vec<serde_json::Value>,
    microphone: String,
    apple: bool,
    sonora: bool,
    rnnoise: bool,
    mouse: bool,
    system_binding: bool,
    permission: Option<String>,
    limitations: Vec<String>,
}
#[tauri::command]
pub async fn audio_environment(app: tauri::AppHandle) -> Result<AudioEnvironment, ApiError> {
    tauri::async_runtime::spawn_blocking(move || {
        let caps = ss_input::Backend::detect().capabilities();
        Ok(AudioEnvironment {
            preferences: load(&app)?,
            devices: DeviceManager::available_devices().into_iter().map(|d| serde_json::json!({"id":d.id.as_str(),"name":d.name,"input":d.input,"output":d.output})).collect(),
            microphone: format!("{:?}", ss_audio::device::microphone_access()), apple: cfg!(target_os="macos"),
            sonora: Backend::Sonora.is_available(), rnnoise: Backend::RnNoise.is_available(),
            mouse: caps.mouse_buttons, system_binding: caps.binding_set_by_system,
            permission: caps.needs_permission.map(|p| p.to_string()), limitations: caps.limitations,
        })
    }).await.map_err(error)?
}
#[tauri::command]
pub async fn save_audio(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    preferences: AudioPreferences,
) -> Result<(), ApiError> {
    let _op = state.operations.lock().await;
    if state.audio_test.lock().await.is_some() {
        return Err(error("Stop the local audio test before applying settings"));
    }
    let config = preferences.config()?;
    let session = state
        .voice
        .lock()
        .await
        .as_ref()
        .map(|v| Arc::clone(&v.session));
    let store = app
        .store_builder("audio.json")
        .disable_auto_save()
        .build()
        .map_err(error)?;
    let previous = store.get("preferences");
    store.set(
        "preferences",
        serde_json::to_value(&preferences).map_err(error)?,
    );
    let restore = || {
        if let Some(previous) = &previous {
            store.set("preferences", previous.clone());
        } else {
            store.delete("preferences");
        }
    };
    if let Err(e) = store.save() {
        restore();
        return Err(error(format!(
            "Could not save audio preferences; nothing applied: {e}"
        )));
    }
    if let Some(session) = &session
        && let Err(e) = session.configure(config).await
    {
        restore();
        store.save().map_err(|save| {
            error(format!(
                "Audio update failed: {e}; restoring saved preferences also failed: {save}"
            ))
        })?;
        return Err(error(e));
    }
    let _ = app.emit("ss://audio-preferences", preferences);
    Ok(())
}
#[tauri::command]
pub async fn audio_level(state: State<'_, AppState>) -> Result<f32, ApiError> {
    Ok(state
        .voice
        .lock()
        .await
        .as_ref()
        .map_or(-96.0, |v| v.session.input_level()))
}
#[tauri::command]
pub async fn stop_audio_test(state: State<'_, AppState>) -> Result<(), ApiError> {
    if let Some(stop) = state.audio_test.lock().await.as_ref() {
        stop.store(true, Ordering::Release);
    }
    Ok(())
}
#[tauri::command]
pub async fn test_audio(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    preferences: AudioPreferences,
    kind: String,
) -> Result<f32, ApiError> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let _op = state.operations.lock().await;
        if state.voice.lock().await.is_some() {
            return Err(error("Leave voice before running a local device test"));
        }
        let mut test = state.audio_test.lock().await;
        if test.is_some() {
            return Err(error("An audio test is already running"));
        }
        *test = Some(Arc::clone(&stop));
    }
    let target = app.clone();
    let result =
        tauri::async_runtime::spawn_blocking(move || preview(&target, &preferences, &kind, &stop))
            .await
            .map_err(error);
    *state.audio_test.lock().await = None;
    result?
}
fn preview(
    app: &tauri::AppHandle,
    preferences: &AudioPreferences,
    kind: &str,
    stop: &AtomicBool,
) -> Result<f32, ApiError> {
    if !["mic", "output", "calibrate"].contains(&kind) {
        return Err(error("Unknown audio test"));
    }
    let config = preferences.config()?;
    let manager = DeviceManager::new();
    let mut capture = if kind != "output" {
        Some(
            manager
                .open_capture(config.input_device.as_ref(), GraphConfig::MONO)
                .map_err(error)?,
        )
    } else {
        None
    };
    let mut playback = if kind != "calibrate" {
        Some(
            manager
                .open_playback(config.output_device.as_ref(), GraphConfig::STEREO)
                .map_err(error)?,
        )
    } else {
        None
    };
    let start = Instant::now();
    let mut delay = std::collections::VecDeque::from(vec![0.0_f32; 48_000]);
    let mut levels = Vec::new();
    let mut frame = Vec::with_capacity(480);
    let mut phase = 0.0_f32;
    let duration = Duration::from_secs(if kind == "calibrate" {
        2
    } else if kind == "output" {
        1
    } else {
        10
    });
    while start.elapsed() < duration && !stop.load(Ordering::Acquire) {
        if let Some((_, consumer)) = &mut capture {
            while frame.len() < 480 {
                match consumer.pop() {
                    Ok(s) => frame.push(s),
                    Err(_) => break,
                }
            }
            if frame.len() == 480 {
                let db = (10.0
                    * (frame.iter().map(|s| s * s).sum::<f32>() / 480.0)
                        .max(1e-10)
                        .log10())
                .clamp(-96.0, 0.0);
                levels.push(db);
                let _ = app.emit("ss://audio-level", db);
                if let Some((_, producer)) = &mut playback {
                    for sample in &frame {
                        delay.push_back(*sample);
                        let sample = delay.pop_front().unwrap_or_default() * config.master_gain;
                        let _ = producer.push(sample.clamp(-1.0, 1.0));
                        let _ = producer.push(sample.clamp(-1.0, 1.0));
                    }
                }
                frame.clear();
            }
        } else if let Some((_, producer)) = &mut playback {
            for _ in 0..480 {
                let sample = phase.sin() * 0.15 * config.master_gain * config.cue_gain;
                phase += 440.0 * std::f32::consts::TAU / 48_000.0;
                let _ = producer.push(sample);
                let _ = producer.push(sample);
            }
        }
        std::thread::sleep(Duration::from_millis(if kind == "output" { 10 } else { 2 }));
    }
    if stop.load(Ordering::Acquire) {
        return Err(error("Audio test stopped"));
    }
    if kind == "calibrate" && levels.len() < 100 {
        return Err(error("Not enough microphone samples to calibrate"));
    }
    levels.sort_by(f32::total_cmp);
    Ok((levels.get(levels.len() / 2).copied().unwrap_or(-52.0) + 12.0).clamp(-96.0, 0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_and_old_records_leave_processing_off() {
        let old: AudioPreferences =
            serde_json::from_value(serde_json::json!({"master":75})).unwrap();
        assert_eq!(old.master, 75);
        assert_eq!(old.mode, "ptt");
        assert!(!old.echo && !old.gain && !old.apple);
        assert_eq!(old.noise, "off");
        assert_eq!(old.release_ms, 150);
    }
    #[test]
    fn invalid_ranges_and_unknown_versions_fail_before_devices_open() {
        let mut p = AudioPreferences {
            master: 201,
            ..Default::default()
        };
        assert!(p.config().is_err());
        p.master = 100;
        p.threshold = f32::NAN;
        assert!(p.config().is_err());
        p.threshold = -40.0;
        p.jitter_min = 190;
        p.jitter_max = 30;
        assert!(p.config().is_err());
        p.jitter_min = 20;
        p.version = 99;
        assert!(p.config().is_err());
    }
}
