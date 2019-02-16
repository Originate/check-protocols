use crate::protocol;
use crate::protocol::Protocol;
use crate::syscall_mocking::{Syscall, SyscallStop, Tracer};
use crate::tracee_memory;
use crate::R;
use libc::user_regs_struct;
use nix::unistd::Pid;
use std::fs::copy;
use std::path::Path;

#[derive(Debug)]
pub struct SyscallMock {
    tracee_pid: Pid,
    expected: Protocol,
    errors: Option<String>,
}

impl SyscallMock {
    pub fn new(tracee_pid: Pid, expected: Protocol) -> SyscallMock {
        SyscallMock {
            tracee_pid,
            expected,
            errors: None,
        }
    }

    pub fn handle_syscall(
        &mut self,
        pid: Pid,
        syscall_stop: SyscallStop,
        syscall: Syscall,
        registers: user_regs_struct,
    ) -> R<()> {
        if let (Syscall::Execve, SyscallStop::Enter) = (&syscall, syscall_stop) {
            if self.tracee_pid != pid {
                let command = tracee_memory::data_to_string(tracee_memory::peekdata_iter(
                    pid,
                    registers.rdi,
                ))?;
                let arguments = tracee_memory::peek_string_array(pid, registers.rsi)?;
                self.handle_step(&command, arguments);
                copy("/bin/true", "/tmp/a")?;
                tracee_memory::pokedata(
                    pid,
                    registers.rdi,
                    tracee_memory::string_to_data("/tmp/a")?,
                )?;
            }
        }
        Ok(())
    }

    fn handle_step(&mut self, received_command: &str, received_arguments: Vec<String>) {
        match self.expected.pop_front() {
            Some(next_expected_step) => {
                match next_expected_step.compare(received_command, received_arguments) {
                    Ok(()) => {}
                    Err(error) => self.push_error(error),
                }
            }
            None => self.push_error(protocol::Step::format_error(
                "<protocol end>",
                &protocol::format_command(&received_command, received_arguments),
            )),
        }
    }

    fn handle_end(&mut self) {
        if let Some(expected_step) = self.expected.pop_front() {
            self.push_error(protocol::Step::format_error(
                &protocol::format_command(&expected_step.command, expected_step.arguments),
                "<script terminated>",
            ));
        }
    }

    fn push_error(&mut self, error: String) {
        if self.errors.is_none() {
            self.errors = Some(error);
        }
    }
}

pub fn run_against_protocol(executable: &Path, expected: Protocol) -> R<Option<String>> {
    let mut syscall_mock = Tracer::run_against_mock(executable, |tracee_pid| {
        SyscallMock::new(tracee_pid, expected)
    })?;
    syscall_mock.handle_end();
    Ok(syscall_mock.errors)
}

#[cfg(test)]
mod run_against_protocol {
    extern crate map_in_place;

    use super::*;
    use std::collections::vec_deque::VecDeque;
    use test_utils::TempFile;

    #[test]
    fn works_for_longer_file_names() -> R<()> {
        let long_command = TempFile::new()?;
        copy("/bin/true", long_command.path())?;
        let script = TempFile::write_temp_script(&format!(
            r##"
                #!/usr/bin/env bash

                {}
            "##,
            long_command.path().to_string_lossy()
        ))?;
        assert_eq!(
            run_against_protocol(
                &script.path(),
                vec![protocol::Step {
                    command: long_command.path().to_string_lossy().into_owned(),
                    arguments: vec![]
                }]
                .into()
            )?,
            None
        );
        Ok(())
    }

    #[test]
    fn complains_when_the_file_does_not_exist() {
        assert_eq!(
            format!(
                "{}",
                run_against_protocol(Path::new("./does_not_exist"), VecDeque::new()).unwrap_err()
            ),
            "ENOENT: No such file or directory"
        );
    }

    #[test]
    fn does_not_execute_the_commands() -> R<()> {
        let testfile = TempFile::new()?;
        let script = TempFile::write_temp_script(&format!(
            r##"
                #!/usr/bin/env bash

                touch {}
            "##,
            testfile.path().to_string_lossy()
        ))?;
        run_against_protocol(&script.path(), VecDeque::new())?;
        assert!(!testfile.path().exists(), "touch was executed");
        Ok(())
    }
}