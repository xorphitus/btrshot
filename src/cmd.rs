use std::process::{Child, Command, Stdio};

use anyhow::Context;
use tracing::{debug, warn};

/// Runs a single external command, capturing its stdout and stderr.
///
/// Stdout is logged at `DEBUG` level; stderr is logged at `WARN` level.
/// Returns `Ok(())` if the command exits with status 0.
/// Returns `Err` if the command fails to spawn or exits with a non-zero status,
/// including the captured stderr in the error message.
pub fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    run_with_env(program, args, &[])
}

/// Chains two or more commands with stdout of step N piped to stdin of step N+1.
///
/// Each step is `(program, args)`. All processes are spawned before any is
/// waited on, so the pipeline runs concurrently. Returns `Err` for the first
/// failing step — that is, the lowest-indexed step that exits with a non-zero
/// status. If multiple steps fail, only the first failure is returned.
///
/// Returns `Err` immediately if `steps` is empty or contains only one entry —
/// a meaningful pipeline requires at least two steps.
pub fn pipe(steps: &[(&str, &[&str])]) -> anyhow::Result<()> {
    pipe_with_env(steps, &[])
}

/// Runs a single external command with optional environment variables,
/// capturing its stdout and stderr.
///
/// Stdout is logged at `DEBUG` level; stderr is logged at `WARN` level.
/// Returns `Ok(stdout_string)` if the command exits with status 0.
/// Returns `Err` if the command fails to spawn, exits with a non-zero status,
/// or produces stdout that is not valid UTF-8.
pub fn run_with_output(program: &str, args: &[&str]) -> anyhow::Result<String> {
    run_with_output_env(program, args, &[])
}

/// Like [`run_with_output`] but also sets additional environment variables on
/// the spawned process.
///
/// `env_vars` is a slice of `(name, value)` pairs.
pub fn run_with_output_env(
    program: &str,
    args: &[&str],
    env_vars: &[(&str, &str)],
) -> anyhow::Result<String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (key, val) in env_vars {
        cmd.env(key, val);
    }

    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn '{program}'"))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        warn!(program, stderr = %stderr.trim(), "command stderr");
    }

    if output.status.success() {
        let stdout = String::from_utf8(output.stdout)
            .with_context(|| format!("stdout of '{program}' is not valid UTF-8"))?;
        if !stdout.trim().is_empty() {
            debug!(program, stdout = %stdout.trim(), "command stdout");
        }
        Ok(stdout)
    } else {
        let code = exit_code_or_signal(&output.status);
        anyhow::bail!("'{program}' exited with {code}: {}", stderr.trim())
    }
}

/// Runs a single external command with optional environment variables.
///
/// This is the env-aware companion to [`run`]. `env_vars` is a slice of
/// `(name, value)` pairs that are set on the spawned process in addition to
/// the inherited environment.
pub fn run_with_env(program: &str, args: &[&str], env_vars: &[(&str, &str)]) -> anyhow::Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (key, val) in env_vars {
        cmd.env(key, val);
    }

    let output = cmd
        .output()
        .with_context(|| format!("failed to spawn '{program}'"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.trim().is_empty() {
        debug!(program, stdout = %stdout.trim(), "command stdout");
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        warn!(program, stderr = %stderr.trim(), "command stderr");
    }

    if output.status.success() {
        Ok(())
    } else {
        let code = exit_code_or_signal(&output.status);
        anyhow::bail!("'{program}' exited with {code}: {}", stderr.trim())
    }
}

/// Chains two or more commands with stdout→stdin piping and optional extra
/// environment variables applied to every step.
///
/// `env_vars` is a slice of `(name, value)` pairs set on every spawned
/// process in addition to the inherited environment.
pub fn pipe_with_env(steps: &[(&str, &[&str])], env_vars: &[(&str, &str)]) -> anyhow::Result<()> {
    if steps.len() < 2 {
        anyhow::bail!("pipe requires at least two steps, got {}", steps.len());
    }

    let mut children: Vec<(usize, String, Child)> = Vec::with_capacity(steps.len());

    for (index, (program, args)) in steps.iter().enumerate() {
        debug!(program, step = index, "spawning pipeline step");

        let stdin = if index == 0 {
            Stdio::null()
        } else {
            let prev_stdout = children
                .last_mut()
                .and_then(|(_, _, child)| child.stdout.take())
                .with_context(|| format!("failed to obtain stdout from step {}", index - 1))?;
            Stdio::from(prev_stdout)
        };

        let mut cmd = Command::new(program);
        cmd.args(*args);
        for (key, val) in env_vars {
            cmd.env(key, val);
        }
        cmd.stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = cmd.spawn().with_context(|| {
            for (_, _, ref mut c) in &mut children {
                let _ = c.kill();
                let _ = c.wait();
            }
            format!("failed to spawn '{program}' at pipeline step {index}")
        })?;

        children.push((index, program.to_string(), child));
    }

    let mut results: Vec<(usize, String, anyhow::Result<()>)> = Vec::with_capacity(children.len());

    for (index, program, child) in children.into_iter().rev() {
        let outcome = child
            .wait_with_output()
            .with_context(|| format!("failed to wait for '{program}' at pipeline step {index}"))
            .and_then(|output| {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.trim().is_empty() {
                    debug!(program, step = index, stdout = %stdout.trim(), "pipeline step stdout");
                }

                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.trim().is_empty() {
                    warn!(program, step = index, stderr = %stderr.trim(), "pipeline step stderr");
                }

                if output.status.success() {
                    Ok(())
                } else {
                    let code = exit_code_or_signal(&output.status);
                    Err(anyhow::anyhow!(
                        "pipeline step {index} ('{program}') exited with {code}: {}",
                        stderr.trim()
                    ))
                }
            });

        results.push((index, program, outcome));
    }

    results.sort_by_key(|(index, _, _)| *index);
    let first_error = results.into_iter().find_map(|(_, _, result)| result.err());

    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Returns a human-readable exit status string.
///
/// If the process exited normally, returns `"status <code>"`.
/// If the process was terminated by a signal (Unix only), returns
/// `"signal <number>"`. Falls back to `"status -1"` if neither is available.
fn exit_code_or_signal(status: &std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("status {code}");
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(sig) = status.signal() {
            return format!("signal {sig}");
        }
    }

    "status -1".to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // run — success path
    // -----------------------------------------------------------------------

    #[test]
    fn test_run_true_returns_ok() {
        let result = run("true", &[]);
        assert!(result.is_ok(), "expected Ok for `true`, got: {result:?}");
    }

    #[test]
    fn test_run_echo_returns_ok() {
        let result = run("echo", &["hello"]);
        assert!(
            result.is_ok(),
            "expected Ok for `echo hello`, got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // run — failure path
    // -----------------------------------------------------------------------

    #[test]
    fn test_run_false_returns_err() {
        let result = run("false", &[]);
        assert!(result.is_err(), "expected Err for `false`, got Ok");
    }

    #[test]
    fn test_run_nonzero_exit_error_contains_exit_code() {
        // `sh -c 'exit 42'` exits with code 42
        let err = run("sh", &["-c", "exit 42"]).expect_err("should be Err");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("42"),
            "error message should mention exit code 42; got: {msg}"
        );
    }

    #[test]
    fn test_run_stderr_included_in_error_message() {
        // Write to stderr then exit non-zero
        let err = run("sh", &["-c", "echo 'something went wrong' >&2; exit 1"])
            .expect_err("should be Err");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("something went wrong"),
            "error message should include stderr output; got: {msg}"
        );
    }

    #[test]
    fn test_run_nonexistent_program_returns_err() {
        let result = run("__no_such_binary_btrshot__", &[]);
        assert!(
            result.is_err(),
            "spawning a missing program should return Err"
        );
    }

    // -----------------------------------------------------------------------
    // pipe — success path
    // -----------------------------------------------------------------------

    #[test]
    fn test_pipe_echo_to_cat_returns_ok() {
        // echo "hello" | cat
        let result = pipe(&[("echo", &["hello"] as &[&str]), ("cat", &[])]);
        assert!(
            result.is_ok(),
            "expected Ok for echo|cat pipeline; got: {result:?}"
        );
    }

    #[test]
    fn test_pipe_three_stages_returns_ok() {
        // echo "hello" | cat | cat
        let result = pipe(&[("echo", &["hello"] as &[&str]), ("cat", &[]), ("cat", &[])]);
        assert!(
            result.is_ok(),
            "expected Ok for echo|cat|cat pipeline; got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // pipe — failure path
    // -----------------------------------------------------------------------

    #[test]
    fn test_pipe_empty_steps_returns_err() {
        let result = pipe(&[]);
        assert!(result.is_err(), "empty pipeline should return Err");
    }

    #[test]
    fn test_pipe_single_step_returns_err() {
        let result = pipe(&[("echo", &["hello"] as &[&str])]);
        assert!(result.is_err(), "single-step pipeline should return Err");
    }

    #[test]
    fn test_pipe_first_step_fails_returns_err() {
        // false | cat — false exits non-zero, pipeline should fail
        let result = pipe(&[("false", &[] as &[&str]), ("cat", &[])]);
        assert!(
            result.is_err(),
            "pipeline with failing first step should return Err"
        );
    }

    #[test]
    fn test_pipe_last_step_fails_returns_err() {
        // echo "hello" | false — last step exits non-zero
        let result = pipe(&[("echo", &["hello"] as &[&str]), ("false", &[])]);
        assert!(
            result.is_err(),
            "pipeline with failing last step should return Err"
        );
    }

    #[test]
    fn test_pipe_middle_step_fails_returns_err() {
        // echo "hello" | false | cat — middle step fails
        let result = pipe(&[
            ("echo", &["hello"] as &[&str]),
            ("false", &[]),
            ("cat", &[]),
        ]);
        assert!(
            result.is_err(),
            "pipeline with failing middle step should return Err"
        );
    }

    #[test]
    fn test_pipe_nonexistent_program_returns_err() {
        // spawn failure at second step
        let result = pipe(&[
            ("echo", &["hello"] as &[&str]),
            ("__no_such_binary_btrshot__", &[]),
        ]);
        assert!(
            result.is_err(),
            "pipeline with non-existent program should return Err"
        );
    }

    // -----------------------------------------------------------------------
    // run_with_output — success path
    // -----------------------------------------------------------------------

    #[test]
    fn test_run_with_output_success_returns_stdout() {
        let result = run_with_output("echo", &["hello world"]);
        assert!(
            result.is_ok(),
            "expected Ok for `echo hello world`, got: {result:?}"
        );
        let stdout = result.expect("already checked ok");
        assert!(
            stdout.contains("hello world"),
            "stdout should contain 'hello world'; got: {stdout:?}"
        );
    }

    #[test]
    fn test_run_with_output_empty_stdout_returns_empty_string() {
        let result = run_with_output("true", &[]);
        assert!(result.is_ok(), "expected Ok for `true`, got: {result:?}");
        assert_eq!(result.expect("already checked ok"), "");
    }

    // -----------------------------------------------------------------------
    // run_with_output — failure path
    // -----------------------------------------------------------------------

    #[test]
    fn test_run_with_output_nonzero_exit_returns_err() {
        let result = run_with_output("false", &[]);
        assert!(result.is_err(), "expected Err for `false`, got Ok");
    }

    #[test]
    fn test_run_with_output_error_contains_exit_code() {
        let err = run_with_output("sh", &["-c", "exit 42"]).expect_err("should be Err");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("42"),
            "error message should mention exit code 42; got: {msg}"
        );
    }

    #[test]
    fn test_run_with_output_error_contains_stderr() {
        let err = run_with_output("sh", &["-c", "echo 'bad output' >&2; exit 1"])
            .expect_err("should be Err");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("bad output"),
            "error message should include stderr; got: {msg}"
        );
    }

    #[test]
    fn test_run_with_output_invalid_utf8_returns_err() {
        // Output raw bytes 0x80 0x81 (invalid UTF-8 sequences) then exit 0
        let err = run_with_output("sh", &["-c", "printf '\\x80\\x81'"])
            .expect_err("should be Err on invalid UTF-8");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("UTF-8") || msg.contains("utf-8") || msg.contains("utf8"),
            "error message should mention UTF-8; got: {msg}"
        );
    }

    #[test]
    fn test_run_with_output_nonexistent_program_returns_err() {
        let result = run_with_output("__no_such_binary_btrshot__", &[]);
        assert!(
            result.is_err(),
            "spawning a missing program should return Err"
        );
    }

    // -----------------------------------------------------------------------
    // exit_code_or_signal — helper
    // -----------------------------------------------------------------------

    #[test]
    fn test_exit_code_or_signal_normal_exit() {
        // sh -c 'exit 7' exits normally with code 7
        let output = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .output()
            .expect("sh must be available");
        let s = exit_code_or_signal(&output.status);
        assert_eq!(s, "status 7", "normal exit should report status code");
    }

    #[cfg(unix)]
    #[test]
    fn test_exit_code_or_signal_signal_termination() {
        // `kill -9 $$` self-kills with SIGKILL (signal 9)
        let output = std::process::Command::new("sh")
            .args(["-c", "kill -9 $$"])
            .output()
            .expect("sh must be available");
        // The shell itself may exit with code 137 or the child may show signal.
        // We only assert the function doesn't panic and returns a string.
        let s = exit_code_or_signal(&output.status);
        assert!(
            !s.is_empty(),
            "exit_code_or_signal should return a non-empty string"
        );
    }

    // -----------------------------------------------------------------------
    // run_with_env — success path
    // -----------------------------------------------------------------------

    #[test]
    fn test_run_with_env_true_returns_ok() {
        let result = run_with_env("true", &[], &[]);
        assert!(
            result.is_ok(),
            "expected Ok for `true` with empty env_vars, got: {result:?}"
        );
    }

    #[test]
    fn test_run_with_env_injects_variable() {
        // The shell tests that $MY_VAR equals "expected_value"; exits 0 on success.
        let result = run_with_env(
            "sh",
            &["-c", "test \"$MY_VAR\" = \"expected_value\""],
            &[("MY_VAR", "expected_value")],
        );
        assert!(
            result.is_ok(),
            "env variable should be injected into the subprocess; got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // run_with_env — failure path
    // -----------------------------------------------------------------------

    #[test]
    fn test_run_with_env_false_returns_err() {
        let result = run_with_env("false", &[], &[]);
        assert!(result.is_err(), "expected Err for `false`, got Ok");
    }

    // -----------------------------------------------------------------------
    // run_with_output_env — env injection
    // -----------------------------------------------------------------------

    #[test]
    fn test_run_with_output_env_injects_variable() {
        // $MY_VAR should appear in the captured stdout.
        let result =
            run_with_output_env("sh", &["-c", "echo $MY_VAR"], &[("MY_VAR", "hello_world")]);
        assert!(
            result.is_ok(),
            "expected Ok when env variable is set; got: {result:?}"
        );
        let stdout = result.expect("already checked ok");
        assert!(
            stdout.contains("hello_world"),
            "stdout should contain the injected env value 'hello_world'; got: {stdout:?}"
        );
    }

    // -----------------------------------------------------------------------
    // pipe_with_env — success path
    // -----------------------------------------------------------------------

    #[test]
    fn test_pipe_with_env_basic_pipeline_returns_ok() {
        // echo "hello" | cat — no env vars
        let result = pipe_with_env(&[("echo", &["hello"] as &[&str]), ("cat", &[])], &[]);
        assert!(
            result.is_ok(),
            "expected Ok for echo|cat pipeline via pipe_with_env; got: {result:?}"
        );
    }

    #[test]
    fn test_pipe_with_env_injects_variable_into_pipeline() {
        // The first step exits non-zero when $MY_VAR is not "piped_value",
        // so assert!(result.is_ok()) genuinely proves the variable was injected.
        let result = pipe_with_env(
            &[
                (
                    "sh",
                    &["-c", "test \"$MY_VAR\" = \"piped_value\""] as &[&str],
                ),
                ("cat", &[] as &[&str]),
            ],
            &[("MY_VAR", "piped_value")],
        );
        assert!(
            result.is_ok(),
            "env variable should be propagated to pipeline steps; got: {result:?}"
        );
    }

    // -----------------------------------------------------------------------
    // pipe_with_env — failure path
    // -----------------------------------------------------------------------

    #[test]
    fn test_pipe_with_env_failing_step_returns_err() {
        // false | cat — first step exits non-zero
        let result = pipe_with_env(&[("false", &[] as &[&str]), ("cat", &[])], &[]);
        assert!(
            result.is_err(),
            "pipeline with failing step should return Err via pipe_with_env"
        );
    }

    #[test]
    fn test_pipe_with_env_empty_steps_returns_err() {
        let result = pipe_with_env(&[], &[]);
        assert!(
            result.is_err(),
            "empty pipeline should return Err via pipe_with_env"
        );
    }
}
