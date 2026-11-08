use std::io::{self, Write};

use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

fn main() -> io::Result<()> {
    let mut signals = Signals::new([SIGINT, SIGTERM])?;

    writeln!(io::stdout().lock(), "Hello world")?;

    let _ = signals.forever().next();

    Ok(())
}
