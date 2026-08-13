use std::fmt;

/// SSH flags that consume a following value argument, mirroring the bash
/// script's `-[BbcDEeFIiJlmOopQRSWw]` case pattern.
const VALUE_FLAGS: &[char] = &[
    'B', 'b', 'c', 'D', 'E', 'e', 'F', 'I', 'i', 'J', 'l', 'm', 'O', 'o', 'p', 'Q', 'R', 'S', 'W',
    'w',
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub host: String,
    pub ssh_args: Vec<String>,
    pub use_waypipe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    MissingHost,
    RemoteCommandsUnsupported,
    MissingValueFor(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingHost => write!(f, "usage: nomad [--waypipe|-wp] [ssh options] host"),
            ParseError::RemoteCommandsUnsupported => write!(
                f,
                "remote commands are not supported; this always launches your dotfiles shell"
            ),
            ParseError::MissingValueFor(flag) => write!(f, "missing value for {flag}"),
        }
    }
}

impl std::error::Error for ParseError {}

fn is_value_flag(arg: &str) -> bool {
    let mut chars = arg.chars();
    matches!((chars.next(), chars.next(), chars.next()), (Some('-'), Some(c), None) if VALUE_FLAGS.contains(&c))
}

/// Parses `[--waypipe|-wp] [ssh options] host`, matching the bash script's
/// argument loop: known value-taking ssh flags consume the next argument,
/// other dashed flags are forwarded as-is, `--` and any trailing arguments
/// after the host are rejected since nomad never runs a remote command.
pub fn parse_ssh_args(args: &[String]) -> Result<Destination, ParseError> {
    let mut use_waypipe = false;
    let mut ssh_args = Vec::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "--waypipe" | "-wp" => {
                use_waypipe = true;
                i += 1;
            }
            "--" => return Err(ParseError::RemoteCommandsUnsupported),
            _ if is_value_flag(arg) => {
                let value = args
                    .get(i + 1)
                    .ok_or_else(|| ParseError::MissingValueFor(arg.clone()))?;
                ssh_args.push(arg.clone());
                ssh_args.push(value.clone());
                i += 2;
            }
            _ if arg.starts_with('-') => {
                ssh_args.push(arg.clone());
                i += 1;
            }
            _ => {
                let host = arg.clone();
                if i + 1 < args.len() {
                    return Err(ParseError::RemoteCommandsUnsupported);
                }
                return Ok(Destination {
                    host,
                    ssh_args,
                    use_waypipe,
                });
            }
        }
    }

    Err(ParseError::MissingHost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plain_host() {
        let dest = parse_ssh_args(&args(&["host"])).unwrap();
        assert_eq!(dest.host, "host");
        assert!(dest.ssh_args.is_empty());
        assert!(!dest.use_waypipe);
    }

    #[test]
    fn waypipe_flag_sets_flag_and_is_not_forwarded() {
        let dest = parse_ssh_args(&args(&["--waypipe", "host"])).unwrap();
        assert!(dest.use_waypipe);
        assert!(dest.ssh_args.is_empty());

        let dest = parse_ssh_args(&args(&["-wp", "host"])).unwrap();
        assert!(dest.use_waypipe);
    }

    #[test]
    fn value_flag_consumes_next_arg() {
        let dest = parse_ssh_args(&args(&["-p", "2222", "host"])).unwrap();
        assert_eq!(dest.ssh_args, vec!["-p".to_string(), "2222".to_string()]);
        assert_eq!(dest.host, "host");
    }

    #[test]
    fn boolean_flag_is_forwarded_alone() {
        let dest = parse_ssh_args(&args(&["-4", "-A", "host"])).unwrap();
        assert_eq!(dest.ssh_args, vec!["-4".to_string(), "-A".to_string()]);
    }

    #[test]
    fn value_flag_without_trailing_host_is_missing_host() {
        assert_eq!(
            parse_ssh_args(&args(&["-p", "22"])),
            Err(ParseError::MissingHost)
        );
    }

    #[test]
    fn value_flag_missing_its_value_is_an_error() {
        assert_eq!(
            parse_ssh_args(&args(&["-p"])),
            Err(ParseError::MissingValueFor("-p".into()))
        );
    }

    #[test]
    fn no_args_is_missing_host() {
        assert_eq!(parse_ssh_args(&args(&[])), Err(ParseError::MissingHost));
    }

    #[test]
    fn double_dash_rejected() {
        assert_eq!(
            parse_ssh_args(&args(&["--", "host"])),
            Err(ParseError::RemoteCommandsUnsupported)
        );
    }

    #[test]
    fn trailing_command_after_host_rejected() {
        assert_eq!(
            parse_ssh_args(&args(&["host", "echo", "hi"])),
            Err(ParseError::RemoteCommandsUnsupported)
        );
    }
}
