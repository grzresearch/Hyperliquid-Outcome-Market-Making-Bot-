use crate::AppConfig;
use anyhow::{Context, Result};

pub fn load() -> Result<AppConfig> {
    let path = std::env::var("HL_OMM_CONFIG").unwrap_or_else(|_| "config/default.toml".into());
    let cfg = config::Config::builder()
        .add_source(config::File::with_name(&path).required(true))
        .add_source(config::Environment::with_prefix("HL_OMM").separator("__"))
        .build()
        .context("config build")?;
    let app: AppConfig = cfg.try_deserialize().context("config deserialize")?;
    Ok(app)
}
