//! Cross-platform entrypoint argument and exit-status helpers.

use std::process::ExitStatus;

pub(crate) fn parse_args_json(value: Option<&str>) -> Result<Vec<String>, String> {
    match value {
        None | Some("") => Ok(Vec::new()),
        Some(json) => serde_json::from_str(json)
            .map_err(|e| format!("DIMENSION_AGENT_ARGS_JSON is invalid: {e}")),
    }
}

pub(crate) fn entrypoint_argv(binary: &str, args: &[String]) -> Vec<String> {
    std::iter::once(binary.to_string())
        .chain(args.iter().cloned())
        .collect()
}

pub(crate) fn exit_outcome(status: &ExitStatus) -> (i32, bool) {
    use std::os::unix::process::ExitStatusExt;

    let exit_code = status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    (exit_code, exit_code == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_json_preserves_spaces_empty_strings_and_unicode() {
        let args = parse_args_json(Some(
            r#"["argument with spaces","","こんにちは","--flag=value"]"#,
        ))
        .expect("parse args JSON");

        assert_eq!(
            args,
            vec!["argument with spaces", "", "こんにちは", "--flag=value"]
        );
        assert_eq!(parse_args_json(None).unwrap(), Vec::<String>::new());
        assert_eq!(parse_args_json(Some("")).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn args_json_rejects_malformed_or_non_string_arrays() {
        assert!(parse_args_json(Some("not JSON")).is_err());
        assert!(parse_args_json(Some(r#"{"arg":"value"}"#)).is_err());
        assert!(parse_args_json(Some(r#"["valid",3]"#)).is_err());
    }

    #[test]
    fn real_process_receives_exact_entrypoint_argv() {
        let args = vec![
            "-c".to_string(),
            r#"for a in "$@"; do printf "[%s]" "$a"; done"#.to_string(),
            "sh".to_string(),
            "argument with spaces".to_string(),
            "".to_string(),
            "こんにちは".to_string(),
        ];
        let argv = entrypoint_argv("/bin/sh", &args);
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .expect("spawn /bin/sh");

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "[argument with spaces][][こんにちは]"
        );
    }

    #[test]
    fn exit_outcome_reports_zero_and_nonzero_codes() {
        let success = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .status()
            .expect("run successful child");
        let failure = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 3"])
            .status()
            .expect("run failing child");

        assert_eq!(exit_outcome(&success), (0, true));
        assert_eq!(exit_outcome(&failure), (3, false));
    }
}
