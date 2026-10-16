// SPDX-License-Identifier: MIT
use std::path::PathBuf;
use winreg::{RegKey, enums::*};

/// Stored per Windows user; portable binaries need neither installation nor elevation.
#[derive(Clone, Debug)]
pub struct Settings {
    pub case_sensitive: bool,
    pub order: usize,
    pub windows_eol: bool,
    pub temporary_directory: PathBuf,
    pub source_a: String,
    pub source_b: String,
    pub target: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            case_sensitive: true,
            order: 1,
            windows_eol: true,
            temporary_directory: std::env::temp_dir().join("TextMerge"),
            source_a: String::new(),
            source_b: String::new(),
            target: String::new(),
        }
    }
}

impl Settings {
    pub const REGISTRY_PATH: &'static str = "Software\\TextMerge";

    pub fn load() -> anyhow::Result<Self> {
        let root = RegKey::predef(HKEY_CURRENT_USER);
        let key = match root.open_subkey(Self::REGISTRY_PATH) {
            Ok(key) => key,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.into()),
        };
        let defaults = Self::default();
        Ok(Self {
            case_sensitive: key.get_value::<u32, _>("CaseSensitive").unwrap_or(1) != 0,
            order: (key.get_value::<u32, _>("MergeOrder").unwrap_or(1) as usize).min(3),
            windows_eol: key.get_value::<u32, _>("WindowsEol").unwrap_or(1) != 0,
            temporary_directory: PathBuf::from(
                key.get_value::<String, _>("TemporaryDirectory")
                    .unwrap_or_else(|_| {
                        defaults.temporary_directory.to_string_lossy().into_owned()
                    }),
            ),
            source_a: key.get_value("SourceA").unwrap_or_default(),
            source_b: key.get_value("SourceB").unwrap_or_default(),
            target: key.get_value("Target").unwrap_or_default(),
        })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(Self::REGISTRY_PATH)?;
        key.set_value("CaseSensitive", &u32::from(self.case_sensitive))?;
        key.set_value("MergeOrder", &(self.order as u32))?;
        key.set_value("WindowsEol", &u32::from(self.windows_eol))?;
        key.set_value(
            "TemporaryDirectory",
            &self.temporary_directory.to_string_lossy().as_ref(),
        )?;
        key.set_value("SourceA", &self.source_a)?;
        key.set_value("SourceB", &self.source_b)?;
        key.set_value("Target", &self.target)?;
        Ok(())
    }
}
