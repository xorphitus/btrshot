use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Deserialize, Debug)]
pub struct PathsConfig {
    pub source_path: PathBuf,
    pub source_subvolume: String,
    pub backup_path: PathBuf,
}

#[derive(Deserialize, Debug)]
pub struct S3Config {
    pub bucket: String,
    pub retention_count: usize,
    pub aws_profile: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct GpgConfig {
    pub public_key_file: PathBuf,
}

#[derive(Deserialize, Debug)]
pub struct ScheduleConfig {
    pub check_interval: u64,
    pub full_backup_interval: u64,
    pub incremental_interval: u64,
}

#[derive(Deserialize, Debug)]
pub struct StateConfig {
    pub state_dir: PathBuf,
}

#[derive(Deserialize, Debug)]
pub struct Config {
    pub paths: PathsConfig,
    pub s3: S3Config,
    pub gpg: GpgConfig,
    pub schedule: ScheduleConfig,
    pub state: StateConfig,
}

impl Config {
    /// Reads and parses the TOML configuration file at `path`.
    ///
    /// Returns an error if the file cannot be read or does not conform to the
    /// expected schema.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;
        let config: Config = toml::from_str(&raw)
            .with_context(|| format!("failed to parse config file: {}", path.display()))?;
        Ok(config)
    }

    /// Returns the full path to the btrfs subvolume on the source disk,
    /// i.e. `source_path / source_subvolume`.
    pub fn source_subvolume_path(&self) -> PathBuf {
        self.paths.source_path.join(&self.paths.source_subvolume)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    const VALID_TOML: &str = r#"
[paths]
source_path = "/mnt/a"
source_subvolume = "data"
backup_path = "/mnt/b"

[s3]
bucket = "my-bucket"
retention_count = 10

[gpg]
public_key_file = "/etc/btrshot/key.pub"

[schedule]
check_interval = 7200
full_backup_interval = 604800
incremental_interval = 86400

[state]
state_dir = "/var/lib/btrshot"
"#;

    fn write_temp_toml(content: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("failed to create temp file");
        file.write_all(content.as_bytes())
            .expect("failed to write temp file");
        file
    }

    #[test]
    fn test_load_valid_config() {
        let file = write_temp_toml(VALID_TOML);
        let config = Config::load(file.path()).expect("should parse valid config");

        assert_eq!(config.paths.source_path, PathBuf::from("/mnt/a"));
        assert_eq!(config.paths.source_subvolume, "data");
        assert_eq!(config.paths.backup_path, PathBuf::from("/mnt/b"));
        assert_eq!(config.s3.bucket, "my-bucket");
        assert_eq!(config.s3.retention_count, 10);
        assert_eq!(
            config.gpg.public_key_file,
            PathBuf::from("/etc/btrshot/key.pub")
        );
        assert_eq!(config.schedule.check_interval, 7200);
        assert_eq!(config.schedule.full_backup_interval, 604800);
        assert_eq!(config.schedule.incremental_interval, 86400);
        assert_eq!(config.state.state_dir, PathBuf::from("/var/lib/btrshot"));
    }

    #[test]
    fn test_load_missing_file() {
        let result = Config::load(Path::new("/nonexistent/path/config.toml"));
        assert!(result.is_err(), "loading a missing file should return Err");
    }

    #[test]
    fn test_load_missing_field() {
        let toml_missing_bucket = r#"
[paths]
source_path = "/mnt/a"
source_subvolume = "data"
backup_path = "/mnt/b"

[s3]
retention_count = 10

[gpg]
public_key_file = "/etc/btrshot/key.pub"

[schedule]
check_interval = 7200
full_backup_interval = 604800
incremental_interval = 86400

[state]
state_dir = "/var/lib/btrshot"
"#;
        let file = write_temp_toml(toml_missing_bucket);
        let result = Config::load(file.path());
        assert!(result.is_err(), "missing required field should return Err");
    }

    #[test]
    fn test_source_subvolume_path() {
        let file = write_temp_toml(VALID_TOML);
        let config = Config::load(file.path()).expect("should parse valid config");
        assert_eq!(config.source_subvolume_path(), PathBuf::from("/mnt/a/data"));
    }

    #[test]
    fn test_optional_aws_profile_absent() {
        let file = write_temp_toml(VALID_TOML);
        let config = Config::load(file.path()).expect("should parse valid config");
        assert!(
            config.s3.aws_profile.is_none(),
            "aws_profile should be None when omitted"
        );
    }

    #[test]
    fn test_optional_aws_profile_present() {
        let toml_with_profile = r#"
[paths]
source_path = "/mnt/a"
source_subvolume = "data"
backup_path = "/mnt/b"

[s3]
bucket = "my-bucket"
retention_count = 10
aws_profile = "backup-profile"

[gpg]
public_key_file = "/etc/btrshot/key.pub"

[schedule]
check_interval = 7200
full_backup_interval = 604800
incremental_interval = 86400

[state]
state_dir = "/var/lib/btrshot"
"#;
        let file = write_temp_toml(toml_with_profile);
        let config = Config::load(file.path()).expect("should parse valid config");
        assert_eq!(config.s3.aws_profile, Some("backup-profile".to_string()));
    }

    #[test]
    fn test_load_malformed_toml() {
        let file = write_temp_toml("this is not valid toml ][[[");
        let result = Config::load(file.path());
        assert!(result.is_err(), "malformed TOML should return Err");
    }

    #[test]
    fn test_load_wrong_type() {
        let toml_wrong_type = r#"
[paths]
source_path = "/mnt/a"
source_subvolume = "data"
backup_path = "/mnt/b"

[s3]
bucket = "my-bucket"
retention_count = "not_a_number"

[gpg]
public_key_file = "/etc/btrshot/key.pub"

[schedule]
check_interval = 7200
full_backup_interval = 604800
incremental_interval = 86400

[state]
state_dir = "/var/lib/btrshot"
"#;
        let file = write_temp_toml(toml_wrong_type);
        let result = Config::load(file.path());
        assert!(result.is_err(), "wrong field type should return Err");
    }

    #[test]
    fn test_load_missing_file_error_contains_path() {
        let path = Path::new("/nonexistent/path/config.toml");
        let result = Config::load(path);
        let err_msg = format!("{:#}", result.expect_err("should be Err"));
        assert!(
            err_msg.contains("/nonexistent/path/config.toml"),
            "error message should contain the file path"
        );
    }
}
