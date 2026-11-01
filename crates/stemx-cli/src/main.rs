//! `stemx`: headless CLI for Stem Extractor (testing and batch use).

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "stemx",
    version,
    about = "Stem Extractor command-line interface"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print version information for the stemx crates.
    Info,
}

/// (crate name, crate version) for every core crate.
fn crate_versions() -> [(&'static str, &'static str); 6] {
    [
        (env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        (stemx_dsp::NAME, stemx_dsp::VERSION),
        (stemx_io::NAME, stemx_io::VERSION),
        (stemx_separation::NAME, stemx_separation::VERSION),
        (stemx_audio::NAME, stemx_audio::VERSION),
        (stemx_library::NAME, stemx_library::VERSION),
    ]
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Info => {
            println!("Stem Extractor");
            for (name, version) in crate_versions() {
                println!("  {name:<18} {version}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn lists_all_crates() {
        let versions = crate_versions();
        assert!(versions.iter().all(|(_, v)| !v.is_empty()));
        assert!(versions.iter().any(|(n, _)| *n == "stemx-separation"));
    }
}
