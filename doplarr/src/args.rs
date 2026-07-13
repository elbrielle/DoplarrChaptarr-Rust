use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
pub struct Cli {
    /// Validate configuration and every backend, print a sanitized report, and
    /// exit before connecting to Discord.
    #[arg(long)]
    pub check: bool,

    #[arg(value_name = "FILE", default_value = "config.toml")]
    pub config_file: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_mode_accepts_an_explicit_config_path() {
        let cli = Cli::try_parse_from(["doplarr", "--check", "/tmp/config.toml"]).unwrap();

        assert!(cli.check);
        assert_eq!(cli.config_file, PathBuf::from("/tmp/config.toml"));
    }

    #[test]
    fn run_mode_keeps_the_default_config_path() {
        let cli = Cli::try_parse_from(["doplarr"]).unwrap();

        assert!(!cli.check);
        assert_eq!(cli.config_file, PathBuf::from("config.toml"));
    }
}
