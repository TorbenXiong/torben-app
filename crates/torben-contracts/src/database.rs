use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::{ExactVersion, TorbenError, TorbenResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseEngine {
    Mysql,
    Redis,
    Postgresql,
}

impl DatabaseEngine {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mysql => "mysql",
            Self::Redis => "redis",
            Self::Postgresql => "postgresql",
        }
    }

    pub const fn default_port(self) -> u16 {
        match self {
            Self::Mysql => 3306,
            Self::Redis => 6379,
            Self::Postgresql => 5432,
        }
    }
}

impl fmt::Display for DatabaseEngine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for DatabaseEngine {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mysql" => Ok(Self::Mysql),
            "redis" => Ok(Self::Redis),
            "postgresql" | "postgres" | "pg" => Ok(Self::Postgresql),
            _ => Err(TorbenError::new(
                "database_engine_unsupported",
                "Expected mysql, redis, or postgresql.",
            )
            .with_detail("engine", value)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DatabaseInstanceName(String);

impl DatabaseInstanceName {
    /// Creates a validated instance name that is safe to use as one path component.
    ///
    /// # Errors
    ///
    /// Returns an error when the name is empty, too long, contains unsupported characters, or is
    /// reserved by Windows.
    pub fn new(value: impl Into<String>) -> TorbenResult<Self> {
        let value = value.into();
        let normalized = value.to_ascii_lowercase();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value == normalized
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
            && !is_windows_reserved_name(&value);
        if valid {
            Ok(Self(value))
        } else {
            Err(TorbenError::new(
                "database_instance_name_invalid",
                "Instance names must use 1-64 lowercase ASCII letters, digits, '-' or '_'.",
            )
            .with_detail("name", value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DatabaseInstanceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for DatabaseInstanceName {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

fn is_windows_reserved_name(value: &str) -> bool {
    matches!(
        value,
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseInstanceState {
    Stopped,
    Running,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseInstance {
    pub engine: DatabaseEngine,
    pub name: DatabaseInstanceName,
    pub runtime_version: ExactVersion,
    pub port: u16,
    pub data_path: String,
    pub created_at: String,
    pub state: DatabaseInstanceState,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateDatabaseInstanceRequest {
    pub engine: DatabaseEngine,
    pub name: DatabaseInstanceName,
    pub runtime_version: Option<ExactVersion>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatabaseInstanceTarget {
    pub engine: DatabaseEngine,
    pub name: DatabaseInstanceName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupDatabaseInstanceRequest {
    pub engine: DatabaseEngine,
    pub name: DatabaseInstanceName,
    pub destination: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreDatabaseInstanceRequest {
    pub engine: DatabaseEngine,
    pub name: DatabaseInstanceName,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeleteDatabaseInstanceRequest {
    pub engine: DatabaseEngine,
    pub name: DatabaseInstanceName,
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseBackup {
    pub engine: DatabaseEngine,
    pub instance_name: DatabaseInstanceName,
    pub path: String,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{DatabaseEngine, DatabaseInstanceName};

    #[test]
    fn instance_names_are_safe_path_components() {
        assert!(DatabaseInstanceName::new("local_dev").is_ok());
        for invalid in ["", ".", "..", "Local", "a/b", "a\\b", "con", "COM1"] {
            assert!(DatabaseInstanceName::new(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn database_engine_accepts_postgresql_aliases() {
        assert_eq!(
            DatabaseEngine::from_str("pg").unwrap(),
            DatabaseEngine::Postgresql
        );
        assert!(DatabaseEngine::from_str("sqlite").is_err());
    }
}
