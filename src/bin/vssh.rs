use std::ffi::CString;
use std::io::{self, Write};
use nix::unistd::{fork, ForkResult, execvp, dup2, pipe, close};
use nix::sys::wait::waitpid;
use nix::fcntl::{open, OFlag};
use nix::sys::stat::Mode;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use anyhow::Result;

/// Represents the status of processing a line.
#[derive(Debug)]
enum Status {
    Continue,
    Exit,
}

fn main() {
    loop {
        let current_dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("unknown"));
        print!("{}$ ", current_dir.display());
        io::stdout().flush().unwrap();

        let mut input_line = String::new();
        if io::stdin().read_line(&mut input_line).is_err() {
            eprintln!("Error reading the input");
            continue;
        }

        match process_next_line(&input_line) {
            Ok(Status::Continue) => continue,
            Ok(Status::Exit) => break,
            Err(e) => eprintln!("Error: {}", e),
        }
    }
}

/// Processes the next input line and returns the appropriate status.
fn process_next_line(input_line: &str) -> Result<Status> {
    let trimmed_line = input_line.trim();
    //if empty
    if trimmed_line.is_empty() {
        return Ok(Status::Continue);
    }
    //if exit
    if trimmed_line == "exit" {
        return Ok(Status::Exit);
    }
    //if cd 
    if trimmed_line.starts_with("cd ") {
        let parts: Vec<&str> = trimmed_line.split_whitespace().collect();
        if parts.len() < 2 {
            eprintln!("cd: missing argument");
        } else if let Err(e) = std::env::set_current_dir(parts[1]) {
            eprintln!("cd: {}: {}", parts[1], e);
        }
        return Ok(Status::Continue);
    }
    //pipeline
    if trimmed_line.contains('|') {
        if let Err(e) = execute_pipeline(trimmed_line) {
            eprintln!("Pipeline error: {}", e);
        }
        return Ok(Status::Continue);
    }
    //single command
    if let Err(e) = run_command(trimmed_line) {
        eprintln!("Command error: {}", e);
    }
    Ok(Status::Continue)
}


/// Run a single command with I/O redirection 
fn run_command(command_line: &str) -> Result<()> {
    let mut is_background = false;
    let mut command = command_line.trim().to_string();
    if command.ends_with('&') {
        is_background = true;
        command.pop(); 
        command = command.trim().to_string();
    }

    let (command, input_file, output_file) = parse_command(&command);

    match unsafe { fork()? } {
        ForkResult::Child => {
            //input file
            if let Some(ref input_path) = input_file {
                let input = open(input_path.as_str(), OFlag::O_RDONLY, Mode::empty())
                    .map_err(|e| anyhow::anyhow!("Error opening input file {}: {}", input_path, e))?
                    .into_raw_fd();
                dup2(input, 0)?;
                close(input)?;
            }
            //output file
            if let Some(ref output_path) = output_file {
                let output = open(
                    output_path.as_str(),
                    OFlag::O_CREAT | OFlag::O_WRONLY | OFlag::O_TRUNC,
                    Mode::from_bits(0o644).unwrap(),
                )
                .map_err(|e| anyhow::anyhow!("Error opening output file {}: {}", output_path, e))?
                .into_raw_fd();
                dup2(output, 1)?;
                close(output)?;
            }
            let command_execute = externalize(&command);
            if command_execute.is_empty() {
                std::process::exit(1);
            }
            execvp(&command_execute[0], &command_execute)?;
            unreachable!();
        },
        ForkResult::Parent { child } => {
            if is_background {
                println!("Starting background process {}", child);
            } else {
                let _ = waitpid(child, None)?;
            }
        }
    }
    Ok(())
}

/// Convert a command string into a vector of C-style strings
fn externalize(command: &str) -> Vec<CString> {
    command.split_whitespace()
        .map(|s| CString::new(s).unwrap())
        .collect()
}

/// Parse commands into tokens and check < and > 
fn parse_command(command: &str) -> (String, Option<String>, Option<String>) {
    let mut tokens = command.split_whitespace().peekable();
    let mut token_combine = Vec::new();
    let mut input = None;
    let mut output = None;

    while let Some(part) = tokens.next() {
        match part {
            "<" => {
                if let Some(file) = tokens.next() {
                    input = Some(file.to_string());
                }
            },
            ">" => {
                if let Some(file) = tokens.next() {
                    output = Some(file.to_string());
                }
            },
            _ => token_combine.push(part),
        }
    }
    (token_combine.join(" "), input, output)
}

/// Execute pipelines 
fn execute_pipeline(command_line: &str) -> Result<()> {
    let commands: Vec<&str> = command_line.split('|').map(|s| s.trim()).collect();
    let num_commands = commands.len();
    let mut child_process_ids = Vec::new();
    let mut pipe_ends = Vec::new();

    for _ in 0..(num_commands - 1) {
        pipe_ends.push(pipe()?);
    }
    for (i, segment) in commands.iter().enumerate() {
        let (command, input_file, output_file) = if i == 0 {
            parse_command(segment)
        } else if i == num_commands - 1 {
            parse_command(segment)
        } else {
            (segment.to_string(), None, None)
        };
        let mut command = command;
        if i == num_commands - 1 && command.ends_with('&') {
            command = command.trim_end_matches('&').trim().to_string();
        }
        match unsafe { fork()? } {
            ForkResult::Child => {
                // first command
                if i == 0 {
                    if let Some(ref input_path) = input_file {
                        let input = open(input_path.as_str(), OFlag::O_RDONLY, Mode::empty())
                            .map_err(|e| anyhow::anyhow!("Error opening input file {}: {}", input_path, e))?
                            .into_raw_fd();
                        dup2(input, 0)?;
                        close(input)?;
                    }
                }
                // last command
                if i == num_commands-1{
                    if let Some(ref output_path) = output_file {
                        let output = open(
                            output_path.as_str(),
                            OFlag::O_CREAT | OFlag::O_WRONLY | OFlag::O_TRUNC,
                            Mode::from_bits(0o644).unwrap(),
                        )
                        .map_err(|e| anyhow::anyhow!("Error opening output file {}: {}", output_path, e))?
                        .into_raw_fd();
                        dup2(output, 1)?;
                        close(output)?;
                    }
                }
                // If not first command, the input is previous pipe’s read end
                if i > 0 {
                    let (ref prev_read, _) = pipe_ends[i - 1];
                    dup2(prev_read.as_raw_fd(), 0)?;
                }
                // If not the last command, the output is current pipe’s write end.
                if i < num_commands - 1 {
                    let (_, ref next_write) = pipe_ends[i];
                    dup2(next_write.as_raw_fd(), 1)?;
                }
                for &(ref read, ref write) in &pipe_ends {
                    let _ = close(read.as_raw_fd());
                    let _ = close(write.as_raw_fd());
                }
                let command_execute = externalize(&command);
                if command_execute.is_empty() {
                    std::process::exit(1);
                }
                execvp(&command_execute[0], &command_execute)?;
                unreachable!();
            },
            ForkResult::Parent { child } => {
                child_process_ids.push(child);
            }
        }
    }
    for (read, write) in pipe_ends {
        let _ = close(read.as_raw_fd());
        let _ = close(write.as_raw_fd());
    }
    let is_background = commands[num_commands - 1].trim().ends_with('&');
    if !is_background {
        for processid in child_process_ids {
            let _ = waitpid(processid, None)?;
        }
    } else if let Some(last_pid) = child_process_ids.last() {
        println!("Starting background process {}", last_pid);
    }
    Ok(())
}