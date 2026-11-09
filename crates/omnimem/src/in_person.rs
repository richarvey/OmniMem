//! The terminal check on the CLI commands only a person should run (#37):
//! `omnimem trust confirm`, `reject`, `restore` and `confirm-source`, and
//! `omnimem contradictions resolve` and `accept`. Like a click in the
//! settings panel, these settle what an agent may only propose, so an agent
//! with a shell on the same machine shouldn't be able to run them as part of
//! a script.
//!
//! The check needs stdin and stdout to be a terminal and asks the person to
//! type the last six characters of the key. It's a speed bump, not a wall:
//! a program can fake a terminal (`script`, `expect`) and read the key it
//! was given. What it stops is the casual case, an agent piping a command
//! it thought of, and it makes the step a deliberate one for a person.

use std::io::{BufRead, IsTerminal, Write};

use anyhow::{Result, bail};

/// How many characters of the key the person types.
const TAIL: usize = 6;

/// The last [`TAIL`] characters of `key`, which the person types back.
fn tail(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    chars[chars.len().saturating_sub(TAIL)..].iter().collect()
}

/// Go on only if a person at a terminal confirms `what`, by typing the end
/// of `key`.
pub(crate) fn confirm(what: &str, key: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!(
            "{what} is a person's decision, so it runs only from a terminal, where you'll be \
             asked to type the end of the key. Run it yourself, or use the settings panel."
        );
    }
    let expected = tail(key);
    print!("To {what}, type the last {TAIL} characters of {key}: ");
    std::io::stdout().flush()?;
    let mut typed = String::new();
    std::io::stdin().lock().read_line(&mut typed)?;
    if typed.trim() != expected {
        bail!("that didn't match the end of {key}, so nothing was changed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tail_is_the_last_six_characters() {
        assert_eq!(tail("mem:knowledge:01FA2B3C"), "FA2B3C");
        assert_eq!(tail("abc"), "abc");
    }
}
