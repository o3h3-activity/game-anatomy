use std::path::PathBuf;
use std::process::ExitCode;

use game_anatomy::generate_hash;

const USAGE: &str = "Usage: game-anatomy <path>\n\nAuto-detects PlayStation, PlayStation 2, or PSP media and prints only the\nRetroAchievements-compatible MD5.";

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().ok_or(USAGE)?);
    if args.next().is_some() {
        return Err("expected exactly one input path".into());
    }
    let md5 = generate_hash(&path).map_err(|error| error.to_string())?;
    println!("{md5}");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("game-anatomy: {error}");
            ExitCode::FAILURE
        }
    }
}
