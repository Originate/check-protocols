extern crate yaml_rust;

mod argument_parser;
pub mod command;
pub mod command_matcher;
mod executable_path;
pub mod yaml;

use self::argument_parser::Parser;
pub use self::executable_path::compare_executables;
use crate::test_spec::yaml::*;
use crate::utils::{path_to_string, with_has_more};
use crate::R;
pub use command::Command;
pub use command_matcher::{AnchoredRegex, CommandMatcher};
use linked_hash_map::LinkedHashMap;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use yaml_rust::{yaml::Hash, Yaml, YamlLoader};

#[derive(PartialEq, Eq, Debug, Clone)]
pub struct Step {
    pub command_matcher: CommandMatcher,
    pub stdout: Vec<u8>,
    pub exitcode: i32,
}

impl Step {
    pub fn new(command_matcher: CommandMatcher) -> Step {
        Step {
            command_matcher,
            stdout: vec![],
            exitcode: 0,
        }
    }

    fn from_string(string: &str) -> R<Step> {
        Ok(Step::new(CommandMatcher::ExactMatch(Command::new(string)?)))
    }

    fn add_exitcode(&mut self, object: &Hash) -> R<()> {
        if let Ok(exitcode) = object.expect_field("exitcode") {
            self.exitcode = exitcode.expect_integer()?;
        }
        Ok(())
    }

    fn add_stdout(&mut self, object: &Hash) -> R<()> {
        if let Ok(stdout) = object.expect_field("stdout") {
            self.stdout = stdout.expect_bytes()?;
        }
        Ok(())
    }

    fn parse(yaml: &Yaml) -> R<Step> {
        match yaml {
            Yaml::String(string) => Step::from_string(string),
            Yaml::Hash(object) => {
                check_keys(&["command", "stdout", "exitcode", "regex"], object)?;
                let mut step = match (object.expect_field("command"), object.expect_field("regex"))
                {
                    (Ok(command_field), Err(_)) => Step::from_string(command_field.expect_str()?)?,
                    (Err(_), Ok(regex_field)) => Step::new(CommandMatcher::RegexMatch(
                        AnchoredRegex::new(regex_field.expect_str()?)?,
                    )),
                    _ => Err("please provide either a 'command' or 'regex' field but not both")?,
                };
                step.add_stdout(object)?;
                step.add_exitcode(object)?;
                Ok(step)
            }
            _ => Err(format!("expected: string or array, got: {:?}", yaml))?,
        }
    }

    fn serialize(&self) -> Yaml {
        let command = Yaml::String(self.command_matcher.format());
        if self.exitcode == 0 {
            command
        } else {
            let mut step = LinkedHashMap::new();
            step.insert(Yaml::from_str("command"), command);
            step.insert(
                Yaml::from_str("exitcode"),
                Yaml::Integer(i64::from(self.exitcode)),
            );
            Yaml::Hash(step)
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
            Step::new(CommandMatcher::ExactMatch(Command {
                executable: PathBuf::from("foo"),
                arguments: vec![],
            })),
        );
        Ok(())
    }

    #[test]
    fn parses_arguments() -> R<()> {
        assert_eq!(
            test_parse_step(r#""foo bar""#)?.command_matcher,
            CommandMatcher::ExactMatch(Command {
                executable: PathBuf::from("foo"),
                arguments: vec![OsString::from("bar")],
            }),
        );
        Ok(())
    }

    #[test]
    fn parses_objects_to_steps() -> R<()> {
        assert_eq!(
            test_parse_step(r#"{command: "foo"}"#)?,
            Step::new(CommandMatcher::ExactMatch(Command {
                executable: PathBuf::from("foo"),
                arguments: vec![],
            })),
        );
        Ok(())
    }

    #[test]
    fn allows_to_put_arguments_in_the_command_field() -> R<()> {
        assert_eq!(
            test_parse_step(r#"{command: "foo bar"}"#)?.command_matcher,
            CommandMatcher::ExactMatch(Command {
                executable: PathBuf::from("foo"),
                arguments: vec![OsString::from("bar")],
            }),
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

#[derive(Debug, PartialEq, Clone)]
pub struct Test {
    pub steps: VecDeque<Step>,
    pub ends_with_hole: bool,
    pub arguments: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub stdout: Option<Vec<u8>>,
    pub stderr: Option<Vec<u8>>,
    pub exitcode: Option<i32>,
    pub mocked_files: Vec<PathBuf>,
}

impl Test {
    #[allow(dead_code)]
    pub fn empty() -> Test {
        Test::new(vec![])
    }

    pub fn new(steps: Vec<Step>) -> Test {
        Test {
            steps: steps.into(),
            ends_with_hole: false,
            arguments: vec![],
            env: HashMap::new(),
            cwd: None,
            stdout: None,
            stderr: None,
            exitcode: None,
            mocked_files: vec![],
        }
    }

    fn from_array(array: &[Yaml]) -> R<Test> {
        enum StepOrHole {
            Step(Step),
            Hole,
        }
        fn parse_step_or_hole(yaml: &Yaml) -> R<StepOrHole> {
            Ok(match yaml {
                Yaml::String(step) if step == "_" => StepOrHole::Hole,
                yaml => StepOrHole::Step(Step::parse(yaml)?),
            })
        }
        let mut test = Test::empty();
        for (yaml, has_more) in with_has_more(array) {
            match parse_step_or_hole(yaml)? {
                StepOrHole::Step(step) => {
                    test.steps.push_back(step);
                }
                StepOrHole::Hole => {
                    test.ends_with_hole = true;
                    if has_more {
                        Err("holes ('_') are only allowed as the last step")?;
                    }
                }
            }
        }
        Ok(test)
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
            self.cwd = Some(PathBuf::from(cwd));
        }
        Ok(())
    }

    fn add_stdout(&mut self, object: &Hash) -> R<()> {
        if let Ok(stdout) = object.expect_field("stdout") {
            self.stdout = Some(stdout.expect_bytes()?);
        }
        Ok(())
    }

    fn add_stderr(&mut self, object: &Hash) -> R<()> {
        if let Ok(stderr) = object.expect_field("stderr") {
            self.stderr = Some(stderr.expect_bytes()?);
        }
        Ok(())
    }

    fn add_exitcode(&mut self, object: &Hash) -> R<()> {
        if let Ok(exitcode) = object.expect_field("exitcode") {
            self.exitcode = Some(exitcode.expect_integer()?);
        }
        Ok(())
    }

    fn add_mocked_files(&mut self, object: &Hash) -> R<()> {
        if let Ok(paths) = object.expect_field("mockedFiles") {
            for path in paths.expect_array()?.iter() {
                self.mocked_files.push(PathBuf::from(path.expect_str()?));
            }
        }
        Ok(())
    }

    fn from_object(object: &Hash) -> R<Test> {
        check_keys(
            &[
                "steps",
                "mockedFiles",
                "arguments",
                "env",
                "exitcode",
                "stdout",
                "stderr",
                "cwd",
            ],
            object,
        )?;
        let mut test = Test::from_array(object.expect_field("steps")?.expect_array()?)?;
        test.add_arguments(&object)?;
        test.add_env(&object)?;
        test.add_cwd(&object)?;
        test.add_stdout(&object)?;
        test.add_stderr(&object)?;
        test.add_exitcode(&object)?;
        test.add_mocked_files(&object)?;
        Ok(test)
    }

    fn serialize_env(&self, object: &mut Hash) {
        if !self.env.is_empty() {
            let mut env = LinkedHashMap::new();
            for (key, value) in &self.env {
                env.insert(Yaml::from_str(key), Yaml::from_str(value));
            }
            object.insert(Yaml::from_str("env"), Yaml::Hash(env));
        }
    }

    fn serialize(&self) -> Yaml {
        let mut test = LinkedHashMap::new();
        if !self.arguments.is_empty() {
            let arguments = self.arguments.iter().map(OsString::from).collect();
            test.insert(
                Yaml::from_str("arguments"),
                Yaml::String(Command::format_arguments(arguments)),
            );
        }
        self.serialize_env(&mut test);
        {
            let mut steps = vec![];
            for step in &self.steps {
                steps.push(step.serialize());
            }
            test.insert(Yaml::from_str("steps"), Yaml::Array(steps));
        }
        if let Some(exitcode) = self.exitcode {
            test.insert(
                Yaml::from_str("exitcode"),
                Yaml::Integer(i64::from(exitcode)),
            );
        }
        Yaml::Hash(test)
    }
}

#[derive(Debug, PartialEq)]
pub struct Tests {
    pub tests: Vec<Test>,
    pub unmocked_commands: Vec<PathBuf>,
    pub interpreter: Option<PathBuf>,
}

impl Tests {
    pub fn new(tests: Vec<Test>) -> Tests {
        Tests {
            tests,
            unmocked_commands: vec![],
            interpreter: None,
        }
    }

    fn from_array(array: &[Yaml]) -> R<Tests> {
        let mut result = vec![];
        for element in array.iter() {
            result.push(Test::from_object(element.expect_object()?)?);
        }
        Ok(Tests::new(result))
    }

    fn add_interpreter(&mut self, object: &Hash) -> R<()> {
        if let Ok(interpreter) = object.expect_field("interpreter") {
            self.interpreter = Some(PathBuf::from(interpreter.expect_str()?));
        }
        Ok(())
    }

    fn add_unmocked_commands(&mut self, object: &Hash) -> R<()> {
        if let Ok(unmocked_commands) = object.expect_field("unmockedCommands") {
            for unmocked_command in unmocked_commands.expect_array()? {
                self.unmocked_commands
                    .push(PathBuf::from(unmocked_command.expect_str()?));
            }
        }
        Ok(())
    }

    fn parse(yaml: Yaml) -> R<Tests> {
        Ok(match &yaml {
            Yaml::Array(array) => Tests::from_array(&array)?,
            Yaml::Hash(object) => {
                match (object.expect_field("tests"), object.expect_field("steps")) {
                    (Ok(tests), _) => {
                        check_keys(&["tests", "interpreter", "unmockedCommands"], object)?;
                        let mut tests = Tests::from_array(tests.expect_array()?)?;
                        tests.add_unmocked_commands(object)?;
                        tests.add_interpreter(object)?;
                        tests
                    }
                    (Err(_), Ok(_)) => Tests::new(vec![Test::from_object(&object)?]),
                    (Err(_), Err(_)) => Err(format!(
                        "expected top-level field \"steps\" or \"tests\", got: {:?}",
                        &yaml
                    ))?,
                }
            }
            _ => Err(format!("expected: array or object, got: {:?}", &yaml))?,
        })
    }

    pub fn load(executable_path: &Path) -> R<(PathBuf, Tests)> {
        let test_file = find_test_file(executable_path);
        let file_contents = read_test_file(&test_file)?;
        let yaml: Vec<Yaml> = YamlLoader::load_from_str(&file_contents).map_err(|error| {
            format!("invalid YAML in {}: {}", test_file.to_string_lossy(), error)
        })?;
        let yaml: Yaml = {
            if yaml.len() > 1 {
                Err(format!(
                    "multiple YAML documents not allowed (in {})",
                    test_file.to_string_lossy()
                ))?;
            }
            yaml.into_iter()
                .next()
                .ok_or_else(|| format!("no YAML documents (in {})", test_file.to_string_lossy()))?
        };
        Ok((
            test_file.clone(),
            Tests::parse(yaml)
                .map_err(|error| format!("error in {}: {}", test_file.to_string_lossy(), error))?,
        ))
    }

    fn serialize_unmocked_commands(&self, object: &mut Hash) -> R<()> {
        if !self.unmocked_commands.is_empty() {
            object.insert(
                Yaml::from_str("unmockedCommands"),
                Yaml::Array(
                    self.unmocked_commands
                        .iter()
                        .map(|unmocked_command| {
                            Ok(Yaml::String(path_to_string(unmocked_command)?.to_string()))
                        })
                        .collect::<R<Vec<Yaml>>>()?,
                ),
            );
        }
        Ok(())
    }

    pub fn serialize(&self) -> R<Yaml> {
        let mut object = LinkedHashMap::new();
        self.serialize_unmocked_commands(&mut object)?;
        {
            let mut tests = vec![];
            for test in self.tests.iter() {
                tests.push(test.serialize());
            }
            object.insert(Yaml::from_str("tests"), Yaml::Array(tests));
        }
        Ok(Yaml::Hash(object))
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

    fn test_parse(tempfile: &TempFile, tests_string: &str) -> R<Tests> {
        let test_file = tempfile.path().with_extension("test.yaml");
        fs::write(&test_file, trim_margin(tests_string)?)?;
        Ok(Tests::load(&tempfile.path())?.1)
    }

    fn test_parse_one(tests_string: &str) -> R<Test> {
        let tempfile = TempFile::new()?;
        let result = test_parse(&tempfile, tests_string)?.tests;
        assert_eq!(result.len(), 1);
        Ok(result.into_iter().next().unwrap())
    }

    #[test]
    fn reads_a_test_from_a_sibling_yaml_file() -> R<()> {
        assert_eq!(
            test_parse_one(
                r"
                    |steps:
                    |  - /bin/true
                ",
            )?,
            Test::new(vec![Step::new(CommandMatcher::ExactMatch(Command {
                executable: PathBuf::from("/bin/true"),
                arguments: vec![],
            }))]),
        );
        Ok(())
    }

    #[test]
    fn returns_an_informative_error_when_the_test_file_is_missing() {
        assert_error!(
            Tests::load(&PathBuf::from("./does-not-exist")),
            "test file not found: ./does-not-exist.test.yaml"
        );
    }

    mod invalid_fields {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn disallows_unknown_top_level_keys() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_error!(
                test_parse(
                    &tempfile,
                    "
                        |foo: 42
                        |tests:
                        |  - steps:
                        |      - foo
                    "
                ),
                format!(
                    "error in {}.test.yaml: \
                     unexpected field 'foo', \
                     possible values: 'tests', 'interpreter', 'unmockedCommands'",
                    path_to_string(&tempfile.path())?
                )
            );
            Ok(())
        }

        #[test]
        fn disallows_unknown_test_keys() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_error!(
                test_parse(
                    &tempfile,
                    "
                        |tests:
                        |  - steps:
                        |      - foo
                        |    foo: 42
                    "
                ),
                format!(
                    "error in {}.test.yaml: \
                     unexpected field 'foo', \
                     possible values: \
                     'steps', 'mockedFiles', 'arguments', 'env', \
                     'exitcode', 'stdout', 'stderr', 'cwd'",
                    path_to_string(&tempfile.path())?
                )
            );
            Ok(())
        }

        #[test]
        fn disallows_unknown_step_keys() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_error!(
                test_parse(
                    &tempfile,
                    "
                        |tests:
                        |  - steps:
                        |      - command: foo
                        |        foo: 42
                    "
                ),
                format!(
                    "error in {}.test.yaml: \
                     unexpected field 'foo', \
                     possible values: 'command', 'stdout', 'exitcode', 'regex'",
                    path_to_string(&tempfile.path())?
                )
            );
            Ok(())
        }

        #[test]
        fn multiple_unknown_fields() {}
    }

    fn get_exact(step: Step) -> Command {
        match step.command_matcher {
            CommandMatcher::ExactMatch(command) => command,
            CommandMatcher::RegexMatch(_) => panic!("expected Exact"),
        }
    }

    #[test]
    fn works_for_multiple_commands() -> R<()> {
        assert_eq!(
            test_parse_one(
                r"
                    |steps:
                    |  - /bin/true
                    |  - /bin/false
                "
            )?
            .steps
            .map(|step| get_exact(step).executable.clone()),
            vec![PathBuf::from("/bin/true"), PathBuf::from("/bin/false")],
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_arguments() -> R<()> {
        assert_eq!(
            test_parse_one(
                r"
                    |steps:
                    |  - /bin/true foo bar
                "
            )?
            .steps
            .map(|step| get_exact(step).arguments.clone()),
            vec![vec![OsString::from("foo"), OsString::from("bar")]],
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_the_test_as_an_object() -> R<()> {
        assert_eq!(
            test_parse_one(
                r"
                    |steps:
                    |  - /bin/true
                "
            )?,
            Test::new(vec![Step::new(CommandMatcher::ExactMatch(Command {
                executable: PathBuf::from("/bin/true"),
                arguments: vec![],
            }))]),
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_the_script_environment() -> R<()> {
        assert_eq!(
            test_parse_one(
                r"
                    |steps:
                    |  - /bin/true
                    |env:
                    |  foo: bar
                "
            )?
            .env
            .into_iter()
            .collect::<Vec<_>>(),
            vec![("foo".to_string(), "bar".to_string())]
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_multiple_tests() -> R<()> {
        let tempfile = TempFile::new()?;
        assert_eq!(
            test_parse(
                &tempfile,
                r"
                    |- arguments: foo
                    |  steps: []
                    |- arguments: bar
                    |  steps: []
                ",
            )?
            .tests
            .map(|test| test.arguments),
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
                r"
                    |steps: []
                    |---
                    |steps: []
                ",
            ),
            format!(
                "multiple YAML documents not allowed (in {}.test.yaml)",
                path_to_string(&tempfile.path())?
            )
        );
        Ok(())
    }

    #[test]
    fn allows_to_specify_multiple_tests_as_an_object() -> R<()> {
        let tempfile = TempFile::new()?;
        assert_eq!(
            test_parse(
                &tempfile,
                r"
                    |tests:
                    |  - arguments: foo
                    |    steps: []
                    |  - arguments: bar
                    |    steps: []
                ",
            )?
            .tests
            .map(|test| test.arguments),
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
                "error in {}.test.yaml: \
                 expected top-level field \"steps\" or \"tests\", \
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
                    r"
                        |steps:
                        |  - /bin/true
                        |arguments: foo bar
                    "
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
                    r#"
                        |steps:
                        |  - /bin/true
                        |arguments: foo "bar baz"
                    "#
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
                    r"
                        |steps:
                        |  - /bin/true
                        |arguments: 42
                    "
                ),
                format!(
                    "error in {}.test.yaml: \
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
                    r"
                        |steps:
                        |  - /bin/true
                        |cwd: /foo
                    "
                )?
                .cwd,
                Some(PathBuf::from("/foo"))
            );
            Ok(())
        }

        #[test]
        fn none_is_the_default() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r"
                        |steps:
                        |  - /bin/true
                    "
                )?
                .cwd,
                None
            );
            Ok(())
        }

        #[test]
        fn disallows_relative_paths() -> R<()> {
            let yaml = YamlLoader::load_from_str(&trim_margin(
                r"
                    |steps:
                    |  - /bin/true
                    |cwd: foo
                ",
            )?)?;
            assert_error!(
                Tests::parse(yaml[0].clone()),
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
                    r"
                        |steps:
                        |  - /bin/true
                        |exitcode: 42
                    "
                )?
                .exitcode,
                Some(42)
            );
            Ok(())
        }

        #[test]
        fn uses_none_as_the_default() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r"
                        |steps:
                        |  - /bin/true
                    "
                )?
                .exitcode,
                None
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
                    r"
                        |tests:
                        |  - steps: []
                        |unmockedCommands:
                        |  - foo
                    "
                )?
                .unmocked_commands
                .map(|path| path.to_string_lossy().to_string()),
                vec!["foo"]
            );
            Ok(())
        }
    }

    mod specified_interpreter {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn allows_a_specified_interpreter() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_eq!(
                test_parse(
                    &tempfile,
                    r"
                        |tests:
                        |  - steps: []
                        |interpreter: /bin/bash
                    ",
                )?
                .interpreter
                .unwrap()
                .to_string_lossy(),
                "/bin/bash",
            );
            Ok(())
        }

        #[test]
        fn disallows_an_interpreter_with_an_invalid_type() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_error!(
                test_parse(
                    &tempfile,
                    r"
                        |tests:
                        |  - steps: []
                        |interpreter: 42
                    ",
                ),
                format!(
                    "error in {}.test.yaml: \
                     expected: string, got: Integer(42)",
                    path_to_string(&tempfile.path())?
                )
            );
            Ok(())
        }
    }

    #[test]
    fn allows_to_specify_mocked_files() -> R<()> {
        assert_eq!(
            test_parse_one(
                r"
                    |steps: []
                    |mockedFiles:
                    |  - /foo
                "
            )?
            .mocked_files
            .map(|path| path.to_string_lossy().to_string()),
            vec![("/foo")]
        );
        Ok(())
    }

    mod expected_stdout {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn allows_to_specify_the_expected_stdout() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r"
                        |- steps: []
                        |  stdout: foo
                    "
                )?
                .stdout
                .map(|s| String::from_utf8(s).unwrap()),
                Some("foo".to_string())
            );
            Ok(())
        }

        #[test]
        fn none_is_the_default() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r"
                        |- steps: []
                    "
                )?
                .stdout,
                None
            );
            Ok(())
        }
    }

    mod expected_stderr {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn allows_to_specify_the_expected_stderr() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r"
                        |- steps: []
                        |  stderr: foo
                    "
                )?
                .stderr
                .map(|s| String::from_utf8(s).unwrap()),
                Some("foo".to_string())
            );
            Ok(())
        }

        #[test]
        fn none_is_the_default() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r"
                        |- steps: []
                    "
                )?
                .stderr,
                None
            );
            Ok(())
        }
    }

    mod regex_matching {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn parses_a_regex_command_matcher() -> R<()> {
            let step = test_parse_one(
                r"
                    |tests:
                    |  - steps:
                    |      - regex: \d
                ",
            )?
            .steps[0]
                .clone();
            match step.command_matcher {
                CommandMatcher::RegexMatch(regex) => assert_eq!(regex, AnchoredRegex::new("\\d")?),
                _ => panic!("expected regex match, got: {:?}", step.command_matcher),
            }
            Ok(())
        }

        #[test]
        fn disallows_a_regex_matcher_and_command() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_error!(
                test_parse(
                    &tempfile,
                    r"
                        |tests:
                        |  - steps:
                        |      - command: foo
                        |        regex: \d
                    ",
                ),
                format!(
                    "error in {}.test.yaml: \
                     please provide either a 'command' or 'regex' field but not both",
                    path_to_string(&tempfile.path())?
                )
            );
            Ok(())
        }

        #[test]
        fn fails_to_parse_with_bad_regex() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_error!(
                test_parse(
                    &tempfile,
                    r"
                        |tests:
                        |  - steps:
                        |      - regex: \x
                    ",
                ),
                format!(
                    "error in {}.test.yaml: \
                     regex parse error:\n    ^\\x$\n       ^\nerror: invalid hexadecimal digit",
                    path_to_string(&tempfile.path())?
                )
            );
            Ok(())
        }
    }

    mod holes {
        use super::*;
        use pretty_assertions::assert_eq;

        #[test]
        fn parses_underscores_as_holes() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r"
                        |tests:
                        |  - steps:
                        |      - _
                    "
                )?
                .ends_with_hole,
                true
            );
            Ok(())
        }

        #[test]
        fn false_is_the_default() -> R<()> {
            assert_eq!(
                test_parse_one(
                    r"
                        |tests:
                        |  - steps:
                        |      - /bin/foo
                    "
                )?
                .ends_with_hole,
                false
            );
            Ok(())
        }

        #[test]
        fn disallows_underscores_followed_by_more_steps() -> R<()> {
            let tempfile = TempFile::new()?;
            assert_error!(
                test_parse(
                    &tempfile,
                    r"
                        |tests:
                        |  - steps:
                        |      - _
                        |      - /bin/foo
                    "
                ),
                format!(
                    "error in {}.test.yaml: holes ('_') are only allowed as the last step",
                    path_to_string(&tempfile.path())?
                )
            );
            Ok(())
        }

    }
}

#[cfg(test)]
mod serialize {
    use super::*;
    use pretty_assertions::assert_eq;

    fn roundtrip(tests: Tests) -> R<()> {
        let yaml = tests.serialize()?;
        let result = Tests::parse(yaml)?;
        assert_eq!(result, tests);
        Ok(())
    }

    #[test]
    fn outputs_an_empty_tests_object() -> R<()> {
        roundtrip(Tests::new(vec![]))
    }

    #[test]
    fn outputs_a_single_test_with_no_steps() -> R<()> {
        roundtrip(Tests::new(vec![Test::empty()]))
    }

    #[test]
    fn outputs_a_single_test_with_a_single_step() -> R<()> {
        roundtrip(Tests::new(vec![Test::new(vec![Step::from_string("cp")?])]))
    }

    mod arguments {
        use super::*;

        #[test]
        fn outputs_the_test_arguments() -> R<()> {
            let mut test = Test::new(vec![Step::from_string("cp")?]);
            test.arguments = vec!["foo".to_string()];
            roundtrip(Tests::new(vec![test]))
        }

        #[test]
        fn works_for_arguments_with_special_characters() -> R<()> {
            let mut test = Test::new(vec![Step::from_string("cp")?]);
            test.arguments = vec!["foo bar".to_string()];
            roundtrip(Tests::new(vec![test]))
        }
    }

    #[test]
    fn outputs_the_test_exitcode() -> R<()> {
        let mut test = Test::new(vec![Step::from_string("cp")?]);
        test.exitcode = Some(42);
        roundtrip(Tests::new(vec![test]))
    }

    #[test]
    fn includes_the_step_exitcodes() -> R<()> {
        let test = Test::new(vec![Step {
            command_matcher: CommandMatcher::ExactMatch(Command::new("cp")?),
            stdout: vec![],
            exitcode: 42,
        }]);
        roundtrip(Tests::new(vec![test]))
    }

    #[test]
    fn includes_the_environment() -> R<()> {
        let mut test = Test::empty();
        test.env.insert("FOO".to_string(), "bar".to_string());
        roundtrip(Tests::new(vec![test]))
    }

    #[test]
    fn includes_unmocked_commands() -> R<()> {
        let mut tests = Tests::new(vec![Test::new(vec![])]);
        tests.unmocked_commands = vec![PathBuf::from("sed")];
        roundtrip(tests)
    }
}

fn find_test_file(executable: &Path) -> PathBuf {
    let mut result = executable.to_path_buf().into_os_string();
    result.push(".");
    result.push("test.yaml");
    PathBuf::from(result)
}

fn read_test_file(test_file: &Path) -> R<String> {
    if !test_file.exists() {
        Err(format!(
            "test file not found: {}",
            test_file.to_string_lossy()
        ))?;
    }
    Ok(match fs::read(&test_file) {
        Err(error) => Err(format!(
            "error reading {}: {}",
            path_to_string(&test_file)?,
            error
        ))?,
        Ok(file_contents) => String::from_utf8(file_contents)?,
    })
}

#[cfg(test)]
mod find_test_file {
    use super::*;

    #[test]
    fn adds_the_test_file_extension() {
        assert_eq!(
            find_test_file(&PathBuf::from("foo")),
            PathBuf::from("foo.test.yaml")
        );
    }

    #[test]
    fn works_for_files_with_extensions() {
        assert_eq!(
            find_test_file(&PathBuf::from("foo.ext")),
            PathBuf::from("foo.ext.test.yaml")
        );
    }
}
