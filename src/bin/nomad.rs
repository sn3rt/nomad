use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use nomad_env::{load_config, parse_ssh_args, Nomad, OpenSshTransport};

fn usage() -> &'static str {
    "usage: nomad [--waypipe|-wp] [ssh options] host\n       nomad clean [ssh options] host"
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match run(args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("nomad: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: Vec<String>) -> Result<ExitCode> {
    let mut config_path: Option<PathBuf> = None;
    if let Some(pos) = args.iter().position(|a| a == "--config") {
        if pos + 1 >= args.len() {
            eprintln!("{}", usage());
            return Ok(ExitCode::FAILURE);
        }
        config_path = Some(PathBuf::from(args[pos + 1].clone()));
        args.drain(pos..=pos + 1);
    }

    let is_clean = args.first().map(|a| a == "clean").unwrap_or(false);
    if is_clean {
        args.remove(0);
    }

    if args.is_empty() {
        eprintln!("{}", usage());
        return Ok(ExitCode::FAILURE);
    }

    let dest = match parse_ssh_args(&args) {
        Ok(d) => d,
        Err(err) => {
            eprintln!("nomad: {err}");
            if matches!(err, nomad_env::destination::ParseError::MissingHost) {
                eprintln!("{}", usage());
            }
            return Ok(ExitCode::FAILURE);
        }
    };

    let (config, config_source) = load_config(config_path.as_deref())
        .with_context(|| "failed to load nomad configuration")?;
    let root = config
        .profile
        .resolve_root()
        .with_context(|| format!("using config {}", config_source.display()))?;

    if dest.use_waypipe && which("waypipe").is_none() {
        eprintln!("nomad: waypipe mode requires local waypipe");
        return Ok(ExitCode::FAILURE);
    }

    let transport = OpenSshTransport;
    let nomad = Nomad::new(&transport, &config.profile);

    if is_clean {
        nomad.clean(&dest, &root)?;
        eprintln!("nomad: remote temp directory removed");
        return Ok(ExitCode::SUCCESS);
    }

    eprintln!("nomad: using profile root {}", root.display());
    eprintln!("nomad: opening control connection to {}", dest.host);
    let session = nomad.prepare(&dest, &root)?;
    eprintln!(
        "nomad: starting remote {} with temporary dotfiles",
        session.remote_shell_name
    );
    let status = nomad.enter(&dest, &session, dest.use_waypipe)?;

    match status.code() {
        Some(code) => Ok(ExitCode::from(code as u8)),
        None => Ok(ExitCode::FAILURE),
    }
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(bin))
        .find(|candidate| candidate.is_file())
}
