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
    let output = Command::new(program)
        .args(args)
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
        let code = output.status.code().unwrap_or(-1);
        anyhow::bail!("'{program}' exited with status {code}: {}", stderr.trim())
    }
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
    if steps.len() < 2 {
        anyhow::bail!("pipe requires at least two steps, got {}", steps.len());
    }

    // Spawn all processes, wiring stdout[n] -> stdin[n+1].
    // Stores (step_index, program_name_owned, child).
    let mut children: Vec<(usize, String, Child)> = Vec::with_capacity(steps.len());

    for (index, (program, args)) in steps.iter().enumerate() {
        debug!(program, step = index, "spawning pipeline step");

        let stdin = if index == 0 {
            // First step reads from /dev/null — no interactive input.
            Stdio::null()
        } else {
            // Take stdout from the previous child as our stdin.
            let prev_stdout = children
                .last_mut()
                .and_then(|(_, _, child)| child.stdout.take())
                .with_context(|| format!("failed to obtain stdout from step {}", index - 1))?;
            Stdio::from(prev_stdout)
        };

        let stdout = if index == steps.len() - 1 {
            // Last step: inherit stdout so output goes to the terminal / caller.
            Stdio::inherit()
        } else {
            Stdio::piped()
        };

        let child = Command::new(program)
            .args(*args)
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                // On spawn failure: kill and reap all already-spawned children
                // to avoid leaving zombie processes.
                for (_, _, ref mut c) in &mut children {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                format!("failed to spawn '{program}' at pipeline step {index}")
            })?;

        children.push((index, program.to_string(), child));
    }

    // Wait in reverse order (last consumer first) to prevent deadlock:
    // if we waited in spawn order, a middle step blocked on a full stdout
    // pipe would prevent its producer from making progress, causing a
    // circular wait.
    //
    // Collect (index, program, result) so we can report the first failure
    // by step index after all children are reaped.
    let mut results: Vec<(usize, String, anyhow::Result<()>)> = Vec::with_capacity(children.len());

    for (index, program, child) in children.into_iter().rev() {
        let outcome = child
            .wait_with_output()
            .with_context(|| format!("failed to wait for '{program}' at pipeline step {index}"))
            .and_then(|output| {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.trim().is_empty() {
                    warn!(program, step = index, stderr = %stderr.trim(), "pipeline step stderr");
                }

                if output.status.success() {
                    Ok(())
                } else {
                    let code = output.status.code().unwrap_or(-1);
                    Err(anyhow::anyhow!(
                        "pipeline step {index} ('{program}') exited with status {code}: {}",
                        stderr.trim()
                    ))
                }
            });

        results.push((index, program, outcome));
    }

    // Find the first failing step by lowest index.
    results.sort_by_key(|(index, _, _)| *index);
    let first_error = results.into_iter().find_map(|(_, _, result)| result.err());

    match first_error {
        Some(err) => Err(err),
        None => Ok(()),
    }
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
}
