#![cfg_attr(
    feature = "dev",
    allow(dead_code, unused_variables, unused_imports, unreachable_code)
)]
#![cfg_attr(feature = "ci", deny(warnings))]
#![deny(clippy::all)]

use check_protocols::context::Context;
use check_protocols::{cli, run_main, R};
use pretty_assertions::assert_eq;
use test_utils::{trim_margin, TempFile};
use yaml_rust::YamlLoader;

mod yaml_formatting {
    use super::*;

    #[test]
    fn output_contains_trailing_newline() -> R<()> {
        let context = Context::new_mock();
        run_main(
            &context,
            &cli::Args::CheckProtocols {
                script_path: TempFile::write_temp_script(b"#!/usr/bin/env bash")?.path(),
                record: true,
            },
        )?;
        assert!(context.get_captured_stdout().ends_with('\n'));
        Ok(())
    }

    #[test]
    fn does_not_output_three_leading_dashes() -> R<()> {
        let context = Context::new_mock();
        run_main(
            &context,
            &cli::Args::CheckProtocols {
                script_path: TempFile::write_temp_script(b"#!/usr/bin/env bash")?.path(),
                record: true,
            },
        )?;
        assert!(!context.get_captured_stdout().starts_with("---"));
        Ok(())
    }
}

fn assert_eq_yaml(result: &str, expected: &str) -> R<()> {
    let result =
        YamlLoader::load_from_str(result).map_err(|error| format!("{}\n({})", error, result))?;
    let expected = YamlLoader::load_from_str(expected)
        .map_err(|error| format!("{}\n({})", error, expected))?;
    assert_eq!(result, expected);
    Ok(())
}

fn test_recording(script: &str, expected: &str) -> R<()> {
    let script = TempFile::write_temp_script(trim_margin(script)?.as_bytes())?;
    let context = Context::new_mock();
    run_main(
        &context,
        &cli::Args::CheckProtocols {
            script_path: script.path(),
            record: true,
        },
    )?;
    let output = context.get_captured_stdout();
    assert_eq_yaml(&output, &trim_margin(expected)?)?;
    Ok(())
}

#[test]
fn records_an_empty_protocol() -> R<()> {
    test_recording(
        "
            |#!/usr/bin/env bash
        ",
        "
            |protocols:
            |  - protocol: []
        ",
    )
}

#[test]
fn records_protocol_steps() -> R<()> {
    test_recording(
        "
            |#!/usr/bin/env bash
            |ls >/dev/null
        ",
        "
            |protocols:
            |  - protocol:
            |      - ls
        ",
    )
}

#[test]
fn records_multiple_steps() -> R<()> {
    test_recording(
        "
            |#!/usr/bin/env bash
            |date > /dev/null
            |ls > /dev/null
        ",
        "
            |protocols:
            |  - protocol:
            |      - date
            |      - ls
        ",
    )
}

#[test]
fn records_command_arguments() -> R<()> {
    test_recording(
        "
            |#!/usr/bin/env bash
            |mkdir -p foo
        ",
        "
            |protocols:
            |  - protocol:
            |      - mkdir -p foo
        ",
    )
}

#[test]
fn records_script_exitcode() -> R<()> {
    test_recording(
        "
            |#!/usr/bin/env bash
            |exit 42
        ",
        "
            |protocols:
            |  - protocol: []
            |    exitcode: 42
        ",
    )
}

#[test]
fn records_command_exitcodes() -> R<()> {
    test_recording(
        r#"
            |#!/usr/bin/env bash
            |bash -c "exit 42"
            |true
        "#,
        r#"
            |protocols:
            |  - protocol:
            |      - command: bash -c "exit 42"
            |        exitcode: 42
        "#,
    )
}
