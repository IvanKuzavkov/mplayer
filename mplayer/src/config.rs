use std::path::PathBuf;

#[cfg(not(debug_assertions))]
use directories::ProjectDirs;
use mplayer_core::Result;

pub fn data_directory() -> Result<PathBuf> {
    resolve_data_directory(
        || std::env::var_os("MPLAYER_DATA_DIR").map(PathBuf::from),
        default_data_directory,
    )
}

#[cfg(debug_assertions)]
fn default_data_directory() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|path| path.join("data"))
}

#[cfg(not(debug_assertions))]
fn default_data_directory() -> Option<PathBuf> {
    ProjectDirs::from("org", "mplayer", "mplayer").map(|dirs| dirs.data_dir().to_owned())
}

fn resolve_data_directory(
    debug_override: impl FnOnce() -> Option<PathBuf>,
    fallback_directory: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(path) = debug_override().filter(|path| !path.as_os_str().is_empty()) {
        return Ok(path);
    }
    // Release builds do not consult the application-specific environment override.
    #[cfg(not(debug_assertions))]
    let _ = debug_override;

    fallback_directory()
        .ok_or_else(|| anyhow::anyhow!("Cannot locate the application data directory"))
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
