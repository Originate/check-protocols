extern crate yaml_rust;

mod argument_parser;
pub mod command;
mod yaml;

use self::argument_parser::Parser;
use crate::protocol::yaml::*;
use crate::utils::path_to_string;
use crate::R;
pub use command::Command;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use yaml_rust::{yaml::Hash, Yaml, YamlLoader};

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct Step {
    pub command: Command,
    pub stdout: Vec<u8>,
    pub exitcode: i32,
}

impl Step {
    fn from_string(string: &str) -> R<Step> {
        Ok(Step {
            command: Command::new(string)?,
            stdout: vec![],
            exitcode: 0,
        })
    }

    fn add_exitcode(&mut self, object: &Hash) -> R<()> {
        if let Ok(exitcode) = object.expect_field("exitcode") {
            self.exitcode = exitcode.expect_integer()?;
        }
        Ok(())
    }

    fn add_stdout(&mut self, object: &Hash) -> R<()> {
        if let Ok(stdout) = object.expect_field("stdout") {
            self.stdout = stdout.expect_str()?.as_bytes().to_vec();
        }
        Ok(())
    }

    fn parse(yaml: &Yaml) -> R<Step> {
        match yaml {
            Yaml::String(string) => Step::from_string(string),
            Yaml::Hash(object) => {
                let mut step = Step::from_string(object.expect_field("command")?.expect_str()?)?;
                step.add_stdout(object)?;
                step.add_exitcode(object)?;
                Ok(step)
            }
            _ => Err(format!("expected: string or array, got: {:?}", yaml))?,
        }
    }
}

#[cfg(test)]
mod parse_step {
    use super::*;
    use test_utils::assert_error;
    use yaml_rust::Yaml;

    fn test_parse_step(yaml: &str) -> R<Step> {
        let yaml = YamlLoader::load_from_str(yaml)?;
        assert_eq!(yaml.len(), 1);
        let yaml = &yaml[0];
        Step::parse(yaml)
    }

    #[test]
    fn parses_strings_to_steps() -> R<()> {
        assert_eq!(
            test_parse_step(r#""foo""#)?,
            Step {
                command: Command {
                    executable: b"foo".to_vec(),
                    arguments: vec![],
                },
                stdout: vec![],
                exitcode: 0,
            },
        );
        Ok(())
    }

    #[test]
    fn parses_arguments() -> R<()> {
        assert_eq!(
            test_parse_step(r#""foo bar""#)?.command,
            Command {
                executable: b"foo".to_vec(),
                arguments: vec![b"bar".to_vec()],
            },
        );
        Ok(())
    }

    #[test]
    fn parses_objects_to_steps() -> R<()> {
        assert_eq!(
            test_parse_step(r#"{command: "foo"}"#)?,
            Step {
                command: Command {
                    executable: b"foo".to_vec(),
                    arguments: vec![],
                },
                stdout: vec![],
                exitcode: 0,
            },
        );
        Ok(())
    }

    #[test]
    fn allows_to_put_arguments_in_the_command_field() -> R<()> {
        assert_eq!(
            test_parse_step(r#"{command: "foo bar"}"#)?.command,
            Command {
                executable: b"foo".to_vec(),
                arguments: vec![b"bar".to_vec()],
            },
        );
        Ok(())
    }

    #[test]
    fn gives_nice_parse_errors() {
        assert_error!(
            Step::parse(&Yaml::Null),
            "expected: string or array, got: Null"
        )
    }

    #[test]
    fn allows_to_specify_stdout() -> R<()> {
        assert_eq!(
            test_parse_step(r#"{command: "foo", stdout: "bar"}"#)?.stdout,
            b"bar".to_vec(),
        );
        Ok(())
    }

    mod exitcode {
        use super::*;

        #[test]
        fn allows_to_specify_the_mocked_exit_code() -> R<()> {
            assert_eq!(
                test_parse_step(r#"{command: "foo", exitcode: 42}"#)?.exitcode,
                42
            );
            Ok(())
        }

        #[test]
        fn uses_zero_as_the_default() -> R<()> {
            assert_eq!(test_parse_step(r#"{command: "foo"}"#)?.exitcode, 0);
            Ok(())
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct Protocol {
    pub steps: VecDeque<Step>,
    pub arguments: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<Vec<u8>>,
    pub exitcode: i32,
    pub mocked_files: Vec<Vec<u8>>,
}

impl Protocol {
    #[allow(dead_code)]
    pub fn empty() -> Protocol {
        Protocol::new(vec![])
    }

    pub fn new(steps: Vec<Step>) -> Protocol {
        Protocol {
            steps: steps.into(),
            arguments: vec![],
            env: HashMap::new(),
            cwd: None,
            exitcode: 0,
            mocked_files: vec![],
        }
    }

    fn from_array(array: &[Yaml]) -> R<Protocol> {
        Ok(Protocol::new(
            array.iter().map(Step::parse).collect::<R<Vec<Step>>>()?,
        ))
    }

    fn add_arguments(&mut self, object: &Hash) -> R<()> {
        if let Ok(arguments) = object.expect_field("arguments") {
            self.arguments = Parser::parse_arguments(arguments.expect_str()?)?;
        }
        Ok(())
    }

    fn add_env(&mut self, object: &Hash) -> R<()> {
        if let Ok(env) = object.expect_field("env") {
            for (key, value) in env.expect_object()?.into_iter() {
                self.env.insert(
                    key.expect_str()?.to_string(),
                    value.expect_str()?.to_string(),
                );
            }
        }
        Ok(())
    }

    fn add_cwd(&mut self, object: &Hash) -> R<()> {
        if let Ok(cwd) = object.expect_field("cwd") {
            let cwd = cwd.expect_str()?;
            if !cwd.starts_with('/') {
                Err(format!(
                    "cwd has to be an absolute path starting with \"/\", got: {:?}",
                    cwd
                ))?;
            }
            self.cwd = Some(cwd.as_bytes().to_vec());
        }
        Ok(())
    }

    fn add_exitcode(&mut self, object: &Hash) -> R<()> {
        if let Ok(exitcode) = object.expect_field("exitcode") {
            self.exitcode = exitcode.expect_integer()?;
        }
        Ok(())
    }

    fn add_filetree(&mut self, object: &Hash) -> R<()> {
        if let Ok(filetree) = object.expect_field("filetree") {
            for entry in filetree.expect_array()?.iter() {
                self.mocked_files
                    .push(entry.expect_str()?.as_bytes().to_owned());
            }
        }
        Ok(())
    }

    fn from_object(object: &Hash) -> R<Protocol> {
        let mut protocol = Protocol::from_array(object.expect_field("protocol")?.expect_array()?)?;
        protocol.add_arguments(&object)?;
        protocol.add_env(&object)?;
        protocol.add_cwd(&object)?;
        protocol.add_exitcode(&object)?;
        protocol.add_filetree(&object)?;
        Ok(protocol)
    }
}

#[derive(Debug, PartialEq)]
pub struct Protocols {
    pub protocols: Vec<Protocol>,
    pub unmocked_commands: Vec<Vec<u8>>,
}

impl Protocols {
    fn from_array(array: &[Yaml]) -> R<Protocols> {
        let mut result = vec![];
        for element in array.iter() {
            result.push(Protocol::from_object(element.expect_object()?)?);
        }
        Ok(Protocols {
            protocols: result,
            unmocked_commands: vec![],
        })
    }

    fn add_unmocked_commands(&mut self, object: &Hash) -> R<()> {
        if let Ok(unmocked_commands) = object.expect_field("unmockedCommands") {
            for unmocked_command in unmocked_commands.expect_array()? {
                self.unmocked_commands
                    .push(unmocked_command.expect_str()?.as_bytes().to_vec());
            }
        }
        Ok(())
    }

    fn parse(yaml: Yaml) -> R<Protocols> {
        Ok(match &yaml {
            Yaml::Array(array) => Protocols::from_array(&array)?,
            Yaml::Hash(object) => match (
                object.expect_field("protocols"),
                object.expect_field("protocol"),
            ) {
                (Ok(protocols), _) => {
                    let mut protocols = Protocols::from_array(protocols.expect_array()?)?;
                    protocols.add_unmocked_commands(object)?;
                    protocols
                }
                (Err(_), Ok(_)) => Protocols {
                    protocols: vec![Protocol::from_object(&object)?],
                    unmocked_commands: vec![],
                },
                (Err(_), Err(_)) => Err(format!(
                    "expected field \"protocol\" or \"protocols\", got: {:?}",
                    &yaml
                ))?,
            },
            _ => Err(format!("expected: array or object, got: {:?}", &yaml))?,
        })
    }

    pub fn load(executable_path: &Path) -> R<Protocols> {
        let protocols_file = find_protocol_file(executable_path);
        let file_contents = read_protocols_file(&protocols_file)?;
        let yaml: Vec<Yaml> = YamlLoader::load_from_str(&file_contents).map_err(|error| {
            format!(
                "invalid YAML in {}: {}",
                protocols_file.to_string_lossy(),
                error
            )
        })?;
        let yaml: Yaml = {
            if yaml.len() > 1 {
                Err(format!(
                    "multiple YAML documents not allowed (in {})",
                    protocols_file.to_string_lossy()
                ))?;
            }
            yaml.into_iter().next().ok_or_else(|| {
                format!(
                    "no YAML documents (in {})",
                    protocols_file.to_string_lossy()
                )
            })?
        };
        Ok(Protocols::parse(yaml).map_err(|error| {
            format!(
                "unexpected type in {}: {}",
                protocols_file.to_string_lossy(),
                error
            )
        })?)
    }
}

#[cfg(test)]
mod load {
    use super::*;
    use crate::R;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use std::*;
    use test_utils::{assert_error, trim_margin, Mappable, TempFile};

    fn test_parse(tempfile: &TempFile, protocol_string: &str) -> R<Protocols> {
        let protocols_file = tempfile.path().with_extension("protocols.yaml");
        fs::write(&protocols_file, trim_margin(protocol_string)?)?;
        Protocols::load(&tempfile.path())
    }

    fn test_parse_one(protocol_string: &str) -> R<Protocol> {
        let tempfile = TempFile::new()?;
        let result = test_parse(&tempfile, protocol_string)?.protocols;
        assert_eq!(result.len(), 1);
        Ok(result.into_iter().next().unwrap())
    }

    #[test]
    fn reads_a_protocol_from_a_sibling_yaml_file() -> R<()> {
        assert_eq!(
            test_parse_one(
                r##"
                    |protocol:
                    |  - /bin/true
                "##,
            )?,
            Protocol::new(vec![Step {
                command: Command {
                    executable: b"/bin/true".to_vec(),
                    arguments: vec![],
                },
                stdout: vec![],
                exitcode: 0,
            }]),
        );
        Ok(())
    }

    #[test]
    fn returns_an_informative_error_when_the_protocol_file_is_missing() {
        assert_error!(
            Protocols::load(&PathBuf::from("./does-not-exist")),
            "protocol file not found: ./does-not-exist.protocols.yaml"
        );
    }

    #[test]
    fn works_for_multiple_commands() -> R<()> {
        assert_eq!(
            test_parse_one(
                r##"
                    |protocol:
                    |  - /bin/true
                    |  - /bin/false
                "##
            )?
            .steps
            .map(|step| step.command.executable),
            vec![b"/bin/true".to_vec(), b"/bin/false".to_vec()],
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_arguments() -> R<()> {
        assert_eq!(
            test_parse_one(
                r##"
                    |protocol:
                    |  - /bin/true foo bar
                "##
            )?
            .steps
            .map(|step| step.command.arguments),
            vec![vec![b"foo".to_vec(), b"bar".to_vec()]],
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_the_protocol_as_an_object() -> R<()> {
        assert_eq!(
            test_parse_one(
                r##"
                    |protocol:
                    |  - /bin/true
                "##
            )?,
            Protocol::new(vec![Step {
                command: Command {
                    executable: b"/bin/true".to_vec(),
                    arguments: vec![],
                },
                stdout: vec![],
                exitcode: 0,
            }]),
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_the_script_environment() -> R<()> {
        assert_eq!(
            test_parse_one(
                r##"
                    |protocol:
                    |  - /bin/true
                    |env:
                    |  foo: bar
                "##
            )?
            .env
            .into_iter()
            .collect::<Vec<_>>(),
            vec![("foo".to_string(), "bar".to_string())]
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_multiple_protocols() -> R<()> {
        let tempfile = TempFile::new()?;
        assert_eq!(
            test_parse(
                &tempfile,
                r##"
                    |- arguments: foo
                    |  protocol: []
                    |- arguments: bar
                    |  protocol: []
                "##,
            )?
            .protocols
            .map(|protocol| protocol.arguments),
            vec![vec!["foo"], vec!["bar"]]
        );
        Ok(())
    }

    #[test]
    fn disallows_multiple_yaml_documents() -> R<()> {
        let tempfile = TempFile::new()?;
        assert_error!(
            test_parse(
                &tempfile,
                r##"
                    |protocol: []
                    |---
                    |protocol: []
                "##,
            ),
            format!(
                "multiple YAML documents not allowed (in {}.protocols.yaml)",
                path_to_string(&tempfile.path())?
            )
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_multiple_protocols_as_an_object() -> R<()> {
        let tempfile = TempFile::new()?;
        assert_eq!(
            test_parse(
                &tempfile,
                r##"
                    |protocols:
                    |  - arguments: foo
                    |    protocol: []
                    |  - arguments: bar
                    |    protocol: []
                "##,
            )?
            .protocols
            .map(|protocol| protocol.arguments),
            vec![vec!["foo"], vec!["bar"]]
        );
        Ok(())
    }

    #[test]
    fn returns_a_nice_error_when_the_required_top_level_keys_are_missing() -> R<()> {
        let tempfile = TempFile::new()?;
        assert_error!(
            test_parse(&tempfile, "{}"),
            format!(
                "unexpected type in {}.protocols.yaml: \
                 expected field \"protocol\" or \"protocols\", \
                 got: Hash({{}})",
                path_to_string(&tempfile.path())?
            )
        );
        Ok(())
    }

    mod script_arguments {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn allows_arguments() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r##"
                        |protocol:
                        |  - /bin/true
                        |arguments: foo bar
                    "##
                )?
                .arguments,
                vec!["foo", "bar"]
            );
            Ok(())
        }

        #[test]
        fn allows_arguments_with_whitespace() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r##"
                        |protocol:
                        |  - /bin/true
                        |arguments: foo "bar baz"
                    "##
                )?
                .arguments,
                vec!["foo", "bar baz"]
            );
            Ok(())
        }

        #[test]
        fn disallows_arguments_of_invalid_type() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_error!(
                test_parse(
                    &tempfile,
                    r##"
                        |protocol:
                        |  - /bin/true
                        |arguments: 42
                    "##
                ),
                format!(
                    "unexpected type in {}.protocols.yaml: \
                     expected: string, got: Integer(42)",
                    path_to_string(&tempfile.path())?
                )
            );
            Ok(())
        }
    }

    mod working_directory {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn allows_to_specify_the_working_directory() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r##"
                        |protocol:
                        |  - /bin/true
                        |cwd: /foo
                    "##
                )?
                .cwd,
                Some(b"/foo".to_vec())
            );
            Ok(())
        }

        #[test]
        fn none_is_the_default() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r##"
                        |protocol:
                        |  - /bin/true
                    "##
                )?
                .cwd,
                None
            );
            Ok(())
        }

        #[test]
        fn disallows_relative_paths() -> R<()> {
            let yaml = YamlLoader::load_from_str(&trim_margin(
                r##"
                    |protocol:
                    |  - /bin/true
                    |cwd: foo
                "##,
            )?)?;
            assert_error!(
                Protocols::parse(yaml[0].clone()),
                "cwd has to be an absolute path starting with \"/\", got: \"foo\""
            );
            Ok(())
        }
    }

    mod exitcode {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn allows_to_specify_the_expected_exit_code() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r##"
                        |protocol:
                        |  - /bin/true
                        |exitcode: 42
                    "##
                )?
                .exitcode,
                42
            );
            Ok(())
        }

        #[test]
        fn uses_zero_as_the_default() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r##"
                        |protocol:
                        |  - /bin/true
                    "##
                )?
                .exitcode,
                0
            );
            Ok(())
        }
    }

    mod unmocked_commands {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn allows_to_specify_unmocked_commands() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_eq!(
                test_parse(
                    &tempfile,
                    r##"
                        |protocols:
                        |  - protocol: []
                        |unmockedCommands:
                        |  - foo
                    "##
                )?
                .unmocked_commands
                .map(|command| String::from_utf8(command).unwrap()),
                vec!["foo"]
            );
            Ok(())
        }
    }

    #[test]
    fn allows_to_specify_the_local_folder_tree() -> R<()> {
        assert_eq!(
            test_parse_one(
                r##"
                    |protocol:
                    |  - /bin/true
                    |filetree:
                    |  - /usr/local/bin/unrar
                "##
            )?
            .mocked_files
            .map(|mocked_file| String::from_utf8(mocked_file).unwrap()),
            vec![("/usr/local/bin/unrar")]
        );
        Ok(())
    }
}

fn find_protocol_file(executable: &Path) -> PathBuf {
    let mut result = executable.to_path_buf().into_os_string();
    result.push(".");
    result.push("protocols.yaml");
    PathBuf::from(result)
}

fn read_protocols_file(protocols_file: &Path) -> R<String> {
    if !protocols_file.exists() {
        Err(format!(
            "protocol file not found: {}",
            protocols_file.to_string_lossy()
        ))?;
    }
    Ok(match fs::read(&protocols_file) {
        Err(error) => Err(format!(
            "error reading {}: {}",
            path_to_string(&protocols_file)?,
            error
        ))?,
        Ok(file_contents) => String::from_utf8(file_contents)?,
    })
}

#[cfg(test)]
mod find_protocol_file {
    use super::*;

    #[test]
    fn adds_the_protocols_file_extension() {
        assert_eq!(
            find_protocol_file(&PathBuf::from("foo")),
            PathBuf::from("foo.protocols.yaml")
        );
    }

    #[test]
    fn works_for_files_with_extensions() {
        assert_eq!(
            find_protocol_file(&PathBuf::from("foo.ext")),
            PathBuf::from("foo.ext.protocols.yaml")
        );
    }
}
