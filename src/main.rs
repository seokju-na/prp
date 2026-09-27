mod action;
mod artifacts;
mod changelog;
mod cli;
mod commands;
mod config;
mod dependency;
mod git;
mod github;
mod http_util;
mod manifest;
mod package;
mod registry;
mod version;

use clap::Parser;

fn main() -> std::process::ExitCode {
  match commands::run(cli::Cli::parse()) {
    Ok(()) => std::process::ExitCode::SUCCESS,
    Err(error) => {
      eprintln!("error: {error:#}");
      std::process::ExitCode::FAILURE
    }
  }
}
