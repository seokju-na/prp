pub mod change;
pub mod init;
pub mod prepare;
pub mod release;
pub mod status;

use std::process::ExitCode;

/// How a command that succeeded ended, for the process exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
  Done,
  /// There was nothing to do, so nothing was changed.
  NoChanges,
}

impl From<Outcome> for ExitCode {
  fn from(outcome: Outcome) -> Self {
    match outcome {
      Outcome::Done => ExitCode::SUCCESS,
      Outcome::NoChanges => ExitCode::from(2),
    }
  }
}
