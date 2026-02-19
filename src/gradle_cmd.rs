use crate::tracking;
use anyhow::{Context, Result};
use std::process::Command;

#[derive(Debug, Clone)]
pub enum GradleCommand {
    Build,
    Test,
    Check,
    Run,
    Other,
}

pub fn run(cmd: GradleCommand, args: &[String], verbose: u8) -> Result<()> {
    match cmd {
        GradleCommand::Build => run_gradle_filtered("build", args, verbose, filter_gradle_build),
        GradleCommand::Test => run_gradle_filtered("test", args, verbose, filter_gradle_test),
        GradleCommand::Check => run_gradle_filtered("check", args, verbose, filter_gradle_build),
        GradleCommand::Run => run_gradle_filtered("run", args, verbose, filter_gradle_run),
        GradleCommand::Other => run_passthrough(args, verbose),
    }
}

/// Generic gradle command runner with filtering
fn run_gradle_filtered<F>(
    subcommand: &str,
    args: &[String],
    verbose: u8,
    filter_fn: F,
) -> Result<()>
where
    F: Fn(&str) -> String,
{
    let timer = tracking::TimedExecution::start();

    // Check if ./gradlew exists, otherwise use gradle
    let gradle_cmd = if std::path::Path::new("./gradlew").exists() {
        "./gradlew"
    } else {
        "gradle"
    };

    let mut cmd = Command::new(gradle_cmd);
    cmd.arg(subcommand);
    for arg in args {
        cmd.arg(arg);
    }

    if verbose > 0 {
        eprintln!("Running: {} {} {}", gradle_cmd, subcommand, args.join(" "));
    }

    let output = cmd
        .output()
        .with_context(|| format!("Failed to run {} {}", gradle_cmd, subcommand))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw = format!("{}\n{}", stdout, stderr);

    let exit_code = output
        .status
        .code()
        .unwrap_or(if output.status.success() { 0 } else { 1 });
    let filtered = filter_fn(&raw);

    if let Some(hint) = crate::tee::tee_and_hint(&raw, &format!("gradle_{}", subcommand), exit_code)
    {
        println!("{}\n{}", filtered, hint);
    } else {
        println!("{}", filtered);
    }

    timer.track(
        &format!("gradle {} {}", subcommand, args.join(" ")),
        &format!("rtk gradle {} {}", subcommand, args.join(" ")),
        &raw,
        &filtered,
    );

    if !output.status.success() {
        std::process::exit(exit_code);
    }

    Ok(())
}

fn filter_gradle_build(output: &str) -> String {
    let mut build_successful = false;
    let mut task_count = 0;
    let mut errors = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with("> Task :") {
            task_count += 1;
            continue;
        }

        if trimmed.contains("BUILD SUCCESSFUL") {
            build_successful = true;
            continue;
        }

        if trimmed.contains("BUILD FAILED") {
            continue;
        }

        // Keep everything else as potential errors/warnings or relevant output
        // Strip time info usually at the end of lines
        if !trimmed.starts_with("deprecated")
            && !trimmed.starts_with("Configuration on demand is an incubating feature")
        {
            errors.push(trimmed);
        }
    }

    if build_successful && errors.is_empty() {
        return format!("✓ gradle build ({} tasks)", task_count);
    }

    let mut result = String::new();
    if !build_successful {
        result.push_str("gradle build failed:\n");
        result.push_str("═══════════════════════════════════════\n");
    } else {
        result.push_str(&format!(
            "✓ gradle build ({} tasks) with output:\n",
            task_count
        ));
    }

    for (i, err) in errors.iter().enumerate().take(20) {
        result.push_str(err);
        result.push('\n');
    }

    if errors.len() > 20 {
        result.push_str(&format!("\n... +{} more lines\n", errors.len() - 20));
    }

    result.trim().to_string()
}

fn filter_gradle_test(output: &str) -> String {
    let mut failures = Vec::new();
    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;

    // Simple heuristic parsing for Gradle test output
    // Gradle test output can vary plugins, but often looks like:
    // > Task :app:test
    //
    // com.example.MyTest > testSomething() FAILED
    //     java.lang.AssertionError at MyTest.java:10
    //
    // BUILD SUCCESSFUL in 1s
    // 3 actionable tasks: 1 executed, 2 up-to-date

    // Another format (Test Report):
    // Tests run: 5, Failures: 1, Errors: 0, Skipped: 0

    let mut in_failure = false;
    let mut current_failure = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("Tests run:") {
            // Parse summary line
            // Tests run: 5, Failures: 1, Errors: 0, Skipped: 0
            let parts: Vec<&str> = trimmed.split(',').collect();
            for part in parts {
                if let Some(val) = part.trim().split_whitespace().last() {
                    if let Ok(num) = val.parse::<usize>() {
                        if part.contains("Failures") {
                            failed += num;
                        } else if part.contains("Skipped") {
                            skipped += num;
                        } else if part.contains("Tests run") {
                            // This is total, passed = total - failed - skipped (roughly)
                            // We can't easily update passed here without complex logic,
                            // but we can try.
                        }
                    }
                }
            }
            // If we found a summary line, we might rely on it.
            // But usually we want to count passed/failed from individual lines if possible
            // or trust this summary.
            // For now let's just keep the summary line in the output if there are failures.
            if failed > 0 {
                current_failure.push(line.to_string());
            }
            continue;
        }

        if trimmed.ends_with("FAILED") {
            in_failure = true;
            failed += 1;
            current_failure.push(trimmed.to_string());
            continue;
        } else if trimmed.ends_with("PASSED") {
            passed += 1;
            in_failure = false;
            continue;
        } else if trimmed.ends_with("SKIPPED") {
            skipped += 1;
            in_failure = false;
            continue;
        }

        if in_failure {
            // Keep indented lines following a failure
            if line.starts_with("    ") || line.starts_with('\t') {
                current_failure.push(trimmed.to_string());
            } else if trimmed.is_empty() {
                if !current_failure.is_empty() {
                    failures.push(current_failure.join("\n"));
                    current_failure.clear();
                }
                in_failure = false;
            } else {
                // New block starting
                if !current_failure.is_empty() {
                    failures.push(current_failure.join("\n"));
                    current_failure.clear();
                }
                in_failure = false;
            }
        }
    }

    if !current_failure.is_empty() {
        failures.push(current_failure.join("\n"));
    }

    if failed == 0 && failures.is_empty() {
        if passed > 0 {
            return format!(
                "✓ gradle test: {} passed{}",
                passed,
                if skipped > 0 {
                    format!(", {} skipped", skipped)
                } else {
                    "".to_string()
                }
            );
        } else if output.contains("BUILD SUCCESSFUL") {
            // Fallback if we couldn't parse individual tests but build succeeded
            return "✓ gradle test: BUILD SUCCESSFUL".to_string();
        }
    }

    let mut result = String::new();
    result.push_str(&format!(
        "gradle test: {} failed, {} passed{}\n",
        failed,
        passed,
        if skipped > 0 {
            format!(", {} skipped", skipped)
        } else {
            "".to_string()
        }
    ));
    result.push_str("═══════════════════════════════════════\n");

    for (i, fail) in failures.iter().enumerate().take(10) {
        result.push_str(fail);
        result.push('\n');
        if i < failures.len() - 1 {
            result.push('\n');
        }
    }

    if failures.len() > 10 {
        result.push_str(&format!("\n... +{} more failures\n", failures.len() - 10));
    }

    if result.trim().is_empty() {
        // Fallback to showing errors from build output
        return filter_gradle_build(output);
    }

    result.trim().to_string()
}

fn filter_gradle_run(output: &str) -> String {
    // For run, mostly just strip the task execution log
    let mut lines = Vec::new();
    let mut start_capturing = false;

    for line in output.lines() {
        if line.contains("> Task :") && line.contains(":run") {
            start_capturing = true;
            continue;
        }
        if line.contains("BUILD SUCCESSFUL") || line.contains("BUILD FAILED") {
            start_capturing = false;
            continue;
        }

        if start_capturing || !line.trim().starts_with("> Task") {
            lines.push(line);
        }
    }

    if lines.is_empty() {
        return filter_gradle_build(output);
    }

    lines.join("\n").trim().to_string()
}

pub fn run_passthrough(args: &[String], verbose: u8) -> Result<()> {
    let timer = tracking::TimedExecution::start();

    // Check if ./gradlew exists, otherwise use gradle
    let gradle_cmd = if std::path::Path::new("./gradlew").exists() {
        "./gradlew"
    } else {
        "gradle"
    };

    if verbose > 0 {
        eprintln!("gradle passthrough: {} {}", gradle_cmd, args.join(" "));
    }

    let status = Command::new(gradle_cmd)
        .args(args)
        .status()
        .with_context(|| format!("Failed to run {}", gradle_cmd))?;

    let args_str = args.join(" ");
    timer.track_passthrough(
        &format!("gradle {}", args_str),
        &format!("rtk gradle {} (passthrough)", args_str),
    );

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_gradle_build_success() {
        let output = r#"
> Task :compileJava
> Task :processResources
> Task :classes
> Task :jar
> Task :assemble
> Task :compileTestJava
> Task :processTestResources
> Task :testClasses
> Task :test
> Task :check
> Task :build

BUILD SUCCESSFUL in 1s
3 actionable tasks: 3 executed
"#;
        let result = filter_gradle_build(output);
        assert!(result.contains("✓ gradle build"));
        assert!(result.contains("11 tasks"));
    }

    #[test]
    fn test_filter_gradle_build_failure() {
        let output = r#"
> Task :compileJava FAILED
/path/to/App.java:10: error: ';' expected
        System.out.println("Hello")
                                   ^
1 error

FAILURE: Build failed with an exception.

* What went wrong:
Execution failed for task ':compileJava'.
> Compilation failed; see the compiler error output for details.

BUILD FAILED in 1s
"#;
        let result = filter_gradle_build(output);
        assert!(result.contains("gradle build failed"));
        assert!(result.contains("';' expected"));
    }

    #[test]
    fn test_filter_gradle_test_success() {
        let output = r#"
> Task :test

com.example.AppTest > appHasAGreeting() PASSED

BUILD SUCCESSFUL in 1s
"#;
        let result = filter_gradle_test(output);
        assert!(result.contains("✓ gradle test: 1 passed"));
    }

    #[test]
    fn test_filter_gradle_test_failure() {
        let output = r#"
> Task :test

com.example.AppTest > appHasAGreeting() FAILED
    java.lang.AssertionError: expected:<Hello world.> but was:<null>
        at org.junit.Assert.fail(Assert.java:88)
        at org.junit.Assert.failNotEquals(Assert.java:834)
        at org.junit.Assert.assertEquals(Assert.java:118)
        at org.junit.Assert.assertEquals(Assert.java:144)
        at com.example.AppTest.appHasAGreeting(AppTest.java:13)

BUILD FAILED in 1s
"#;
        let result = filter_gradle_test(output);
        assert!(result.contains("gradle test: 1 failed, 0 passed"));
        assert!(result.contains("java.lang.AssertionError"));
    }
}
