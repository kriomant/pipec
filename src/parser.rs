use peginator::{ParseError, PegParser};

// Include the generated parser code
include!(concat!(env!("OUT_DIR"), "/pipeline_ebnf.rs"));

/// Splits pipeline into individual commands.
pub fn split_pipeline(input: &str) -> Result<Vec<&str>, ParseError> {
    let pipeline = Pipeline::parse(input)?;
    let mut commands = Vec::with_capacity(pipeline.rest_commands.len() + 1);
    commands.push(&input[pipeline.first_command.position]);
    commands.extend(pipeline.rest_commands.into_iter().map(|c| &input[c.position]));
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::split_pipeline;

    #[track_caller]
    fn assert_parse(pipeline: &str, expected: &[&str]) {
        assert_eq!(
            split_pipeline(pipeline).unwrap().as_slice(),
            expected,
        );
    }

    #[test]
    fn test_single_command() {
        assert_parse(
            "echo a",
            &[
                "echo a",
            ]
        );
    }

    #[test]
    fn test_simple_pipeline() {
        assert_parse(
            "echo a | cat",
            &[
                "echo a",
                "cat",
            ]
        );

        assert_parse(
            "echo a|cat",
            &[
                "echo a",
                "cat",
            ]
        );
    }

    #[test]
    fn test_group_pipeline() {
        assert_parse(
            "{ echo a; echo b }|cat",
            &[
                "{ echo a; echo b }",
                "cat",
            ]
        );
    }

    #[test]
    fn test_subshell_pipeline() {
        assert_parse(
            "( echo a; echo b )|cat",
            &[
                "( echo a; echo b )",
                "cat",
            ]
        );
    }

    #[test]
    fn test_double_quoted_strings() {
        assert_parse(
            r#"cat file | grep "a|b""#,
            &[
                "cat file",
                r#"grep "a|b""#,
            ]
        );

        assert_parse(
            r#"cat file | grep "a\"|b""#,
            &[
                "cat file",
                r#"grep "a\"|b""#,
            ]
        );
    }

    #[test]
    fn test_single_quoted_strings() {
        assert_parse(
            "cat file | grep 'a|b'",
            &[
                "cat file",
                "grep 'a|b'",
            ]
        );
    }
}