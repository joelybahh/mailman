use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) fn read_json_or_default<T>(path: &Path) -> io::Result<Option<T>>
where
    T: DeserializeOwned,
{
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)?;
    let value = serde_json::from_str::<T>(&raw).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid JSON at {}: {err}", path.display()),
        )
    })?;

    Ok(Some(value))
}

pub(crate) fn write_json_pretty<T>(path: &Path, value: &T) -> io::Result<()>
where
    T: Serialize + ?Sized,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let raw = serde_json::to_string_pretty(value).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to serialize {}: {err}", path.display()),
        )
    })?;

    fs::write(path, raw)?;
    Ok(())
}
