use crate::app::APP_ID;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct AppConfig {
    pub primary_monitor_id: Option<String>,
    pub monitor_settings: Vec<MonitorSettings>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct MonitorSettings {
    pub monitor_id: String,
    pub display_name: String,
    pub include_in_quick_switch: bool,
    pub laptop_input: Option<String>,
    pub toggle_input: Option<String>,
    pub saved_position_x: Option<i32>,
    pub saved_position_y: Option<i32>,
    pub saved_width: Option<u32>,
    pub saved_height: Option<u32>,
}

#[derive(Clone)]
pub struct ConfigStore {
    path: PathBuf,
    state: Arc<Mutex<AppConfig>>,
}

impl ConfigStore {
    pub fn load() -> Self {
        let path = config_file_path();
        let config = fs::read_to_string(&path)
            .ok()
            .and_then(|text| toml::from_str::<AppConfig>(&text).ok())
            .unwrap_or_default();

        Self {
            path,
            state: Arc::new(Mutex::new(config)),
        }
    }

    pub fn current(&self) -> AppConfig {
        self.state.lock().unwrap().clone()
    }

    pub fn update<T, F>(&self, update: F) -> Result<T, String>
    where
        F: FnOnce(&mut AppConfig) -> T,
    {
        let mut config = self.state.lock().unwrap();
        let result = update(&mut config);
        self.save_locked(&config)?;
        Ok(result)
    }

    fn save_locked(&self, config: &AppConfig) -> Result<(), String> {
        let serialized = toml::to_string_pretty(config)
            .map_err(|err| format!("Could not serialize settings: {err}"))?;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("Could not create config directory: {err}"))?;
        }

        fs::write(&self.path, serialized)
            .map_err(|err| format!("Could not write config file {}: {err}", self.path.display()))
    }
}

impl AppConfig {
    pub fn settings(&self, monitor_id: &str) -> Option<&MonitorSettings> {
        self.monitor_settings
            .iter()
            .find(|settings| settings.monitor_id == monitor_id)
    }

    pub fn settings_mut_or_insert(
        &mut self,
        monitor_id: &str,
        display_name: &str,
    ) -> &mut MonitorSettings {
        if let Some(index) = self
            .monitor_settings
            .iter()
            .position(|settings| settings.monitor_id == monitor_id)
        {
            let settings = &mut self.monitor_settings[index];
            settings.display_name = display_name.into();
            return settings;
        }

        self.monitor_settings.push(MonitorSettings {
            monitor_id: monitor_id.into(),
            display_name: display_name.into(),
            ..Default::default()
        });
        self.monitor_settings.last_mut().unwrap()
    }
}

fn config_home() -> PathBuf {
    let base = env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
        let home = env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.config")
    });

    PathBuf::from(base)
}

fn config_file_path() -> PathBuf {
    config_home().join(APP_ID).join("settings.toml")
}
