use crate::config::Config;

/// Uploads a snapshot to S3 and enforces the retention policy.
///
/// This is a stub implementation — see task 9 for the full implementation.
pub fn run_s3_upload(_config: &Config, _snapshot_name: &str) -> anyhow::Result<()> {
    anyhow::bail!("S3 upload not yet implemented")
}
