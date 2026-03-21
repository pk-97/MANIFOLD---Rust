use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

pub trait ExternalProcessRunner {
    fn run_async(command: &str, arguments: &[&str]) -> ProcessHandle
    where
        Self: Sized;
}

#[derive(Debug, Clone)]
pub struct ProcessOutputLine {
    pub line: String,
    pub is_stderr: bool,
}

impl ProcessOutputLine {
    pub fn new(line: String, is_stderr: bool) -> Self {
        Self { line, is_stderr }
    }
}

/// A handle to a running external process.
/// Call `poll()` each frame to drain buffered output lines.
pub struct ProcessHandle {
    receiver: Option<Receiver<ProcessOutputLine>>,
    finished_rx: Option<Receiver<i32>>,
    exit_code: Option<i32>,
    stdout_builder: String,
    stderr_builder: String,
}

impl ProcessHandle {
    /// Construct a handle that is already finished (failed-to-start case).
    fn failed() -> Self {
        Self {
            receiver: None,
            finished_rx: None,
            exit_code: Some(-1),
            stdout_builder: String::new(),
            stderr_builder: String::new(),
        }
    }

    fn running(receiver: Receiver<ProcessOutputLine>, finished_rx: Receiver<i32>) -> Self {
        Self {
            receiver: Some(receiver),
            finished_rx: Some(finished_rx),
            exit_code: None,
            stdout_builder: String::with_capacity(1024),
            stderr_builder: String::with_capacity(1024),
        }
    }

    /// Poll for new output lines. Non-blocking.
    /// Equivalent to `DrainProcessOutputQueue` in Unity.
    pub fn poll(&mut self) -> Vec<ProcessOutputLine> {
        let mut lines = Vec::new();

        // Drain output lines from the channel.
        if let Some(rx) = &self.receiver {
            loop {
                match rx.try_recv() {
                    Ok(output_line) => {
                        if output_line.is_stderr {
                            self.stderr_builder.push_str(&output_line.line);
                            self.stderr_builder.push('\n');
                        } else {
                            self.stdout_builder.push_str(&output_line.line);
                            self.stdout_builder.push('\n');
                        }
                        lines.push(output_line);
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        }

        // Check if the process has finished (exit code sent).
        if self.exit_code.is_none()
            && let Some(rx) = &self.finished_rx {
                match rx.try_recv() {
                    Ok(code) => {
                        self.exit_code = Some(code);
                        // Drop the channels — process is done.
                        self.receiver = None;
                        self.finished_rx = None;
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {
                        // Worker thread exited without sending exit code (crash path).
                        self.exit_code = Some(-1);
                        self.receiver = None;
                        self.finished_rx = None;
                    }
                }
            }

        lines
    }

    /// Check if the process has finished.
    pub fn is_finished(&self) -> bool {
        self.exit_code.is_some()
    }

    /// Get exit code. `None` if still running.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Get accumulated stdout.
    pub fn stdout(&self) -> &str {
        &self.stdout_builder
    }

    /// Get accumulated stderr.
    pub fn stderr(&self) -> &str {
        &self.stderr_builder
    }
}

pub struct ProcessRunnerImpl;

impl ExternalProcessRunner for ProcessRunnerImpl {
    /// Spawn the process asynchronously, returning a handle that can be polled each frame.
    /// Equivalent to `RunAsync` / `RunExternalProcessAsync` in Unity.
    fn run_async(command: &str, arguments: &[&str]) -> ProcessHandle {
        let mut child = match Command::new(command)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return ProcessHandle::failed(),
        };

        let (line_tx, line_rx) = mpsc::channel::<ProcessOutputLine>();
        let (exit_tx, exit_rx) = mpsc::channel::<i32>();

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let line_tx_stdout = line_tx.clone();
        let line_tx_stderr = line_tx;

        // Stdout reader thread.
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        let _ = line_tx_stdout.send(ProcessOutputLine::new(l, false));
                    }
                    Err(_) => break,
                }
            }
        });

        // Stderr reader thread.
        thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        let _ = line_tx_stderr.send(ProcessOutputLine::new(l, true));
                    }
                    Err(_) => break,
                }
            }
        });

        // Waiter thread — blocks on WaitForExit, then sends the exit code.
        thread::spawn(move || {
            let exit_code = match child.wait() {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            };
            let _ = exit_tx.send(exit_code);
        });

        ProcessHandle::running(line_rx, exit_rx)
    }
}

/// Build an argument string for `std::process::Command::arg`.
/// Equivalent to `BuildProcessArgumentsString` in Unity.
/// Unity quotes every argument; here we return individual args so the caller
/// passes them via `Command::args`, which handles quoting natively.
/// This helper exists for contexts that need a shell-style string.
#[allow(dead_code)]
fn build_process_arguments_string(arguments: &[&str]) -> String {
    if arguments.is_empty() {
        return String::new();
    }

    let mut builder = String::with_capacity(arguments.len() * 8);
    for (i, arg) in arguments.iter().enumerate() {
        if i > 0 {
            builder.push(' ');
        }
        builder.push_str(&quote_argument(arg));
    }
    builder
}

/// Build a full shell command line (command + arguments, all shell-quoted).
/// Equivalent to `BuildShellCommandLine` in Unity.
#[allow(dead_code)]
fn build_shell_command_line(command: &str, arguments: &[&str]) -> String {
    let mut builder = String::with_capacity(256);
    builder.push_str(&quote_shell_argument(command));

    for arg in arguments {
        builder.push(' ');
        builder.push_str(&quote_shell_argument(arg));
    }

    builder
}

/// Quote an argument for Windows-style process invocation.
/// Equivalent to `QuoteArgument` in Unity.
#[allow(dead_code)]
fn quote_argument(value: &str) -> String {
    if value.is_empty() {
        return "\"\"".to_string();
    }

    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped)
}

/// Quote an argument for POSIX shell invocation using single quotes.
/// Equivalent to `QuoteShellArgument` in Unity.
#[allow(dead_code)]
fn quote_shell_argument(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_argument_empty() {
        assert_eq!(quote_argument(""), "\"\"");
    }

    #[test]
    fn test_quote_argument_plain() {
        assert_eq!(quote_argument("hello"), "\"hello\"");
    }

    #[test]
    fn test_quote_argument_backslash_and_quote() {
        assert_eq!(quote_argument("a\\b\"c"), "\"a\\\\b\\\"c\"");
    }

    #[test]
    fn test_quote_shell_argument_empty() {
        assert_eq!(quote_shell_argument(""), "''");
    }

    #[test]
    fn test_quote_shell_argument_single_quote() {
        assert_eq!(quote_shell_argument("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_build_process_arguments_string_empty() {
        assert_eq!(build_process_arguments_string(&[]), "");
    }

    #[test]
    fn test_build_process_arguments_string_multiple() {
        let result = build_process_arguments_string(&["foo", "bar baz"]);
        assert_eq!(result, "\"foo\" \"bar baz\"");
    }

    #[test]
    fn test_build_shell_command_line() {
        let result = build_shell_command_line("/usr/bin/ffmpeg", &["-i", "input file.mp4"]);
        assert_eq!(result, "'/usr/bin/ffmpeg' '-i' 'input file.mp4'");
    }

    #[test]
    fn test_process_handle_failed_is_finished() {
        let handle = ProcessHandle::failed();
        assert!(handle.is_finished());
        assert_eq!(handle.exit_code(), Some(-1));
        assert_eq!(handle.stdout(), "");
        assert_eq!(handle.stderr(), "");
    }

    #[test]
    fn test_run_async_echo() {
        let mut handle = ProcessRunnerImpl::run_async("echo", &["hello"]);
        // Give the thread time to run.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let lines = handle.poll();
        assert!(!lines.is_empty());
        assert_eq!(lines[0].line, "hello");
        assert!(!lines[0].is_stderr);
        assert!(handle.is_finished());
        assert_eq!(handle.exit_code(), Some(0));
    }

    #[test]
    fn test_run_async_bad_command() {
        let handle = ProcessRunnerImpl::run_async("/no/such/binary/xyz", &[]);
        assert!(handle.is_finished());
        assert_eq!(handle.exit_code(), Some(-1));
    }
}
