use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use karaoke_player::mkc;
use karaoke_player::text::EncodingChoice;

#[derive(Parser)]
#[command(
    name = "mkc",
    version,
    about = "Convert karaoke songs to MKC (MIDI Karaoke Container)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Convert karaoke files, or every karaoke song in a folder, to .mkc.
    ///
    /// Outputs are always placed under an MKC prefix folder. For example,
    /// NCN\Song\S\SONG0001.mid becomes <output>\MKC\S\SONG0001.mkc.
    Pack {
        /// Karaoke files or folders.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Base output folder; MKC\... is created inside it.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Include subfolders when an input is a folder.
        #[arg(short, long)]
        recursive: bool,
        /// Replace existing .mkc files.
        #[arg(short = 'f', long)]
        overwrite: bool,
        /// Text encoding of the source lyrics (default: UTF-8, else auto).
        #[arg(long, value_enum, default_value_t = EncodingChoice::Auto)]
        encoding: EncodingChoice,
    },
    /// Print the manifest and lyric count of an MKC file.
    Info { file: PathBuf },
    /// Extract every entry from an MKC file.
    Unpack {
        file: PathBuf,
        /// Destination folder (default: the file stem next to the archive).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Pack {
            inputs,
            output,
            recursive,
            overwrite,
            encoding,
        } => pack(&inputs, output, recursive, overwrite, encoding),
        Command::Info { file } => info(&file),
        Command::Unpack { file, output } => unpack(&file, output),
    }
}

fn pack(
    inputs: &[PathBuf],
    output: Option<PathBuf>,
    recursive: bool,
    overwrite: bool,
    encoding: EncodingChoice,
) -> Result<()> {
    let mut songs = Vec::new();
    for input in inputs {
        let found = mkc::find_karaoke_songs(input, recursive)?;
        songs.extend(found);
    }
    songs.sort();
    songs.dedup();
    if songs.is_empty() {
        bail!("no karaoke songs found");
    }

    let output_root = match output {
        Some(path) => path,
        None if inputs.len() == 1 => mkc::default_output_root(&inputs[0]),
        None => bail!("--output is required when converting more than one path"),
    };

    let mut failures = 0usize;
    let mut converted = 0usize;
    let options = karaoke_player::LoadOptions {
        encoding,
        ..Default::default()
    };
    for song in &songs {
        let target = mkc::output_path(song, &output_root);
        match mkc::pack_song_with(song, &target, overwrite, &options) {
            Ok(result) => {
                converted += 1;
                println!(
                    "{} -> {} ({} bytes, {} lyric lines)",
                    song.display(),
                    result.output.display(),
                    result.bytes,
                    result.lyric_lines
                );
            }
            Err(error) => {
                failures += 1;
                eprintln!("{}: {error:#}", song.display());
            }
        }
    }
    println!("converted {converted} of {} song(s)", songs.len());
    if failures > 0 {
        bail!("{failures} song(s) could not be converted");
    }
    Ok(())
}

fn info(file: &Path) -> Result<()> {
    let package = mkc::read_package(file)?;
    let manifest = package.manifest()?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    if let Ok(lyrics) = package.lyrics() {
        println!("lyric lines: {}", lyrics.lines.len());
    }
    Ok(())
}

fn unpack(file: &Path, output: Option<PathBuf>) -> Result<()> {
    let directory = output.unwrap_or_else(|| file.with_extension(""));
    let written = mkc::unpack_to(file, &directory)
        .with_context(|| format!("unpacking {}", file.display()))?;
    for path in written {
        println!("{}", path.display());
    }
    Ok(())
}
