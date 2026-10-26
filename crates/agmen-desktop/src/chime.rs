//! The urgent-alert chime (ADR-0013): macOS's own Glass sound played by
//! `afplay`, so agmen ships no sound file and adds no audio dependency.

use std::cell::RefCell;
use std::process::{Child, Command, Stdio};

/// The sound: one of the system sounds every macOS install has in
/// `/System/Library/Sounds`.
pub(crate) const CHIME_SOUND: &str = "/System/Library/Sounds/Glass.aiff";

/// macOS's command line audio player.
const AFPLAY: &str = "/usr/bin/afplay";

/// What [`Chime::play`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Played {
    Started,
    /// The last chime is still sounding; alerts that arrive together chime
    /// once.
    StillPlaying,
    /// The player could not start.
    Failed,
}

/// Plays the chime without blocking the UI thread. At most one player runs
/// at a time.
#[derive(Debug)]
pub(crate) struct Chime {
    program: String,
    args: Vec<String>,
    /// The player last started; reaped on the next play or on drop.
    playing: RefCell<Option<Child>>,
}

impl Default for Chime {
    fn default() -> Self {
        Self::with_command(AFPLAY, &[CHIME_SOUND])
    }
}

impl Chime {
    /// A chime that runs `program` with `args`.
    #[must_use]
    pub(crate) fn with_command(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            playing: RefCell::new(None),
        }
    }

    /// Starts the chime unless one is still sounding.
    pub(crate) fn play(&self) -> Played {
        let mut playing = self.playing.borrow_mut();
        if let Some(child) = playing.as_mut() {
            match child.try_wait() {
                // Still sounding.
                Ok(None) => return Played::StillPlaying,
                // It finished (and is reaped now), or its status cannot be
                // read; either way a new chime may start.
                Ok(Some(_)) | Err(_) => {}
            }
        }
        let started = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match started {
            Ok(child) => {
                *playing = Some(child);
                Played::Started
            }
            Err(err) => {
                eprintln!("agmen-desktop: the chime did not play: {err}");
                *playing = None;
                Played::Failed
            }
        }
    }
}

impl Drop for Chime {
    fn drop(&mut self) {
        if let Some(mut child) = self.playing.get_mut().take()
            && matches!(child.try_wait(), Ok(None))
        {
            // The app is closing; a chime cut short does not matter, and
            // reaping it keeps no zombie behind.
            if child.kill().is_ok()
                && let Err(err) = child.wait()
            {
                eprintln!("agmen-desktop: the chime player was not reaped: {err}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn the_chime_is_a_system_sound() {
        assert!(std::path::Path::new(CHIME_SOUND).is_file());
        assert!(std::path::Path::new(AFPLAY).is_file());
    }

    #[test]
    fn a_chime_does_not_restart_while_it_sounds() {
        // `sleep` stands in for the player, so the test makes no sound.
        let chime = Chime::with_command("/bin/sleep", &["5"]);
        assert_eq!(chime.play(), Played::Started);
        assert_eq!(chime.play(), Played::StillPlaying);
    }

    #[test]
    fn a_finished_chime_plays_again() {
        let chime = Chime::with_command("/usr/bin/true", &[]);
        assert_eq!(chime.play(), Played::Started);
        // Wait for `true` to exit, then the next chime starts a new player.
        if let Some(child) = chime.playing.borrow_mut().as_mut() {
            child.wait().unwrap();
        }
        assert_eq!(chime.play(), Played::Started);
    }

    #[test]
    fn a_missing_player_fails_quietly() {
        let chime = Chime::with_command("/nonexistent/afplay", &[]);
        assert_eq!(chime.play(), Played::Failed);
    }
}
