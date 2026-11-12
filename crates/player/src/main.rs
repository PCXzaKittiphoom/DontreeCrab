use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use karaoke_player::audio::wav;
use karaoke_player::audio::{self, Command, Player};
use karaoke_player::model::Song;
use karaoke_player::text::EncodingChoice;
use karaoke_player::{EventKind, LoadOptions, SourceFormat, load, soundfont};

mod assoc;
mod gui;
mod icons;

#[derive(Parser)]
#[command(
    name = "DontreeCrab",
    version,
    about = "DontreeCrab - play and inspect MIDI karaoke songs: .kar, NCN (.mid/.lyr/.cur) and KMID"
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Show a one-screen summary of a song
    Info(Common),
    /// Dump the parsed MIDI document (debugging aid)
    Dump(DumpArgs),
    /// Print timed lyrics
    Lyrics(LyricArgs),
    /// Play the song through the default audio device
    Play(PlayArgs),
    /// Render the song to a WAV file
    Render(RenderArgs),
    /// Open the graphical player
    Gui {
        /// Songs to open at startup (the first one plays)
        #[arg(value_name = "FILE")]
        files: Vec<PathBuf>,
    },
    /// Associate .kar files with this player (Windows)
    Associate {
        /// Also take over .mid/.midi files
        #[arg(long)]
        mid: bool,
    },
    /// Remove the file associations created by `associate`
    Unassociate,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum FormatArg {
    Kar,
    Ncn,
    Kmid,
    Midi,
}

#[derive(Args)]
struct Common {
    /// Song file (.kar, .mid or .midi)
    file: PathBuf,

    /// Text encoding for lyric data
    #[arg(long, value_enum, default_value_t = EncodingChoice::Auto)]
    encoding: EncodingChoice,

    /// Force the container instead of sniffing it
    #[arg(long, value_enum)]
    format: Option<FormatArg>,

    /// Explicit .lyr file (NCN)
    #[arg(long, value_name = "PATH")]
    lyrics: Option<PathBuf>,

    /// Explicit .cur file (NCN)
    #[arg(long, value_name = "PATH")]
    cursor: Option<PathBuf>,
}

impl Common {
    fn song(&self) -> Result<Song> {
        let options = LoadOptions {
            encoding: self.encoding,
            format: self.format.map(|f| match f {
                FormatArg::Kar => SourceFormat::Kar,
                FormatArg::Ncn => SourceFormat::Ncn,
                FormatArg::Kmid => SourceFormat::Kmid,
                FormatArg::Midi => SourceFormat::Midi,
            }),
            lyrics: self.lyrics.clone(),
            cursor: self.cursor.clone(),
        };
        load(&self.file, &options)
    }
}

#[derive(Args)]
struct DumpArgs {
    #[command(flatten)]
    common: Common,

    /// Emit the whole song as JSON
    #[arg(long)]
    json: bool,

    /// Also list every event (can be thousands of lines)
    #[arg(long)]
    events: bool,
}

#[derive(Args)]
struct LyricArgs {
    #[command(flatten)]
    common: Common,

    /// Emit the lyric timeline as JSON
    #[arg(long)]
    json: bool,

    /// Print every syllable with its own timestamp
    #[arg(long)]
    chars: bool,
}

#[derive(Args)]
struct PlayArgs {
    #[command(flatten)]
    common: Common,

    /// SoundFont (.sf2). Defaults to $KARAOKE_SF2 or ./soundfonts/*.sf2
    #[arg(long, value_name = "PATH")]
    sf2: Option<PathBuf>,

    /// Master volume, 0.0 - 2.0
    #[arg(long, default_value_t = 0.7)]
    volume: f32,

    /// Transpose in semitones
    #[arg(long, default_value_t = 0)]
    transpose: i32,

    /// Start position in seconds
    #[arg(long, default_value_t = 0.0)]
    start: f64,

    /// Do not print lyric lines while playing
    #[arg(long)]
    no_lyrics: bool,
}

#[derive(Args)]
struct RenderArgs {
    #[command(flatten)]
    common: Common,

    /// Output WAV file
    #[arg(short, long, default_value = "out.wav")]
    output: PathBuf,

    /// SoundFont (.sf2). Defaults to $KARAOKE_SF2 or ./soundfonts/*.sf2
    #[arg(long, value_name = "PATH")]
    sf2: Option<PathBuf>,

    /// Sample rate (Hz)
    #[arg(long, default_value_t = 44_100)]
    rate: u32,

    /// Transpose in semitones
    #[arg(long, default_value_t = 0)]
    transpose: i32,

    /// Master volume, 0.0 - 2.0
    #[arg(long, default_value_t = 0.7)]
    volume: f32,

    /// Start position in seconds
    #[arg(long, default_value_t = 0.0)]
    start: f64,

    /// Render at most this many seconds (preview clip)
    #[arg(long, value_name = "SECONDS")]
    duration: Option<f64>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CliCommand::Info(args) => cmd_info(args),
        CliCommand::Dump(args) => cmd_dump(args),
        CliCommand::Lyrics(args) => cmd_lyrics(args),
        CliCommand::Play(args) => cmd_play(args),
        CliCommand::Render(args) => cmd_render(args),
        CliCommand::Gui { files } => gui::run(files).map_err(|e| anyhow::anyhow!("gui: {e}")),
        CliCommand::Associate { mid } => {
            let exe = std::env::current_exe()?;
            assoc::associate(&exe, mid)?;
            println!(
                "associated .kar{} with {}",
                if mid { " and .mid/.midi" } else { "" },
                exe.display()
            );
            Ok(())
        }
        CliCommand::Unassociate => {
            assoc::unassociate()?;
            println!("file associations removed");
            Ok(())
        }
    }
}

fn cmd_info(args: Common) -> Result<()> {
    let song = args.song()?;

    println!("File      : {}", args.file.display());
    println!("Format    : {}", format_name(song.format));
    println!("Title     : {}", opt(&song.title));
    println!("Artist    : {}", opt(&song.artist));
    println!("Key       : {}", opt(&song.key));
    println!("Language  : {}", opt(&song.language));
    println!("Tempo     : {:.2} BPM", song.bpm);
    println!("Division  : {} ticks / quarter", song.ticks_per_quarter);
    println!("Tracks    : {}", song.track_count);
    println!("Duration  : {}", time(song.duration_us));

    let notes = song
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::NoteOn { .. }))
        .count();
    println!("Events    : {} ({} note-on)", song.events.len(), notes);
    println!(
        "Lyrics    : {} syllables in {} lines",
        song.syllable_count(),
        song.lyric_lines.len()
    );

    let tempos = &song.tempo_map.segments;
    if tempos.len() > 1 {
        println!("Tempo map :");
        for segment in tempos.iter().take(8) {
            println!(
                "            tick {:>7}  {:>7.2} BPM  at {}",
                segment.tick,
                60_000_000.0 / segment.us_per_quarter as f64,
                time(song.tempo_map.tick_to_us(segment.tick))
            );
        }
        if tempos.len() > 8 {
            println!("            ... {} more", tempos.len() - 8);
        }
    }

    for line in &song.info {
        println!("Info      : {line}");
    }

    Ok(())
}

fn cmd_dump(args: DumpArgs) -> Result<()> {
    let song = args.common.song()?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&song)?);
        return Ok(());
    }

    println!(
        "{} / {}  [{}]",
        opt(&song.title),
        opt(&song.artist),
        format_name(song.format)
    );
    println!(
        "division={} tracks={} duration={} bpm={:.2} events={} lyrics={}/{}",
        song.ticks_per_quarter,
        song.track_count,
        time(song.duration_us),
        song.bpm,
        song.events.len(),
        song.syllable_count(),
        song.lyric_lines.len()
    );

    let mut note_on = 0usize;
    let mut note_off = 0usize;
    let mut cc = 0usize;
    let mut program = 0usize;
    let mut text = 0usize;
    let mut tempo = 0usize;
    for event in &song.events {
        match event.kind {
            EventKind::NoteOn { .. } => note_on += 1,
            EventKind::NoteOff { .. } => note_off += 1,
            EventKind::ControlChange { .. } => cc += 1,
            EventKind::ProgramChange { .. } => program += 1,
            EventKind::Text { .. } => text += 1,
            EventKind::Tempo { .. } => tempo += 1,
            _ => {}
        }
    }
    println!(
        "note_on={note_on} note_off={note_off} cc={cc} program={program} text={text} tempo={tempo}"
    );

    println!("tempo segments:");
    for segment in &song.tempo_map.segments {
        println!(
            "  tick {:>7}  {:>8.3} BPM  at {}",
            segment.tick,
            60_000_000.0 / segment.us_per_quarter as f64,
            time(song.tempo_map.tick_to_us(segment.tick))
        );
    }

    if args.events {
        for event in &song.events {
            println!(
                "{} tick {:>7} tr{:>2} {}",
                time(event.at_us),
                event.tick,
                event.track,
                describe(&event.kind)
            );
        }
    }

    Ok(())
}

fn cmd_lyrics(args: LyricArgs) -> Result<()> {
    let song = args.common.song()?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&song.lyric_lines)?);
        return Ok(());
    }

    if song.lyric_lines.is_empty() {
        println!(
            "no lyrics found in {} (plain MIDI?)",
            args.common.file.display()
        );
        return Ok(());
    }

    for line in &song.lyric_lines {
        println!("[{}] {}", time(line.start_us), line.text);
        if args.chars {
            for syllable in &line.syllables {
                println!("    {}  {}", time(syllable.start_us), syllable.text);
            }
        }
    }

    Ok(())
}

fn cmd_play(args: PlayArgs) -> Result<()> {
    let song = args.common.song()?;
    let sound_font = soundfont::load(&soundfont::resolve(args.sf2.as_deref())?)?;

    let position = Arc::new(AtomicU64::new(0));
    let volume = args.volume;
    let transpose = args.transpose;

    let handle = audio::start(
        |rate| {
            let mut player = Player::new(&song, sound_font.clone(), rate, transpose)?;
            player.set_volume(volume);
            Ok(player)
        },
        position.clone(),
        0,
    )?;
    let sender = handle.commands.clone();

    eprintln!(
        "audio: {} Hz, {} ch, {:?}",
        handle.sample_rate, handle.channels, handle.sample_format
    );
    eprintln!(
        "song : {} - {} ({})",
        opt(&song.title),
        opt(&song.artist),
        time(song.duration_us)
    );

    if args.start > 0.0 {
        let _ = sender.send(Command::Seek((args.start * 1_000_000.0) as u64));
    } else {
        let _ = sender.send(Command::Play);
    }

    let mut printed_line = usize::MAX;
    loop {
        std::thread::sleep(Duration::from_millis(120));
        let now = position.load(Ordering::Relaxed);

        if !args.no_lyrics {
            if let Some(index) = song.line_at(now) {
                if index != printed_line {
                    if let Some(line) = song.lyric_lines.get(index) {
                        println!("[{}] {}", time(line.start_us), line.text);
                    }
                    printed_line = index;
                }
            }
        }

        if now >= song.duration_us {
            break;
        }
    }

    println!("[{}] done", time(song.duration_us));
    Ok(())
}

fn cmd_render(args: RenderArgs) -> Result<()> {
    let song = args.common.song()?;
    let sound_font = soundfont::load(&soundfont::resolve(args.sf2.as_deref())?)?;

    let mut player = Player::new(&song, sound_font, args.rate, args.transpose)?;
    player.set_volume(args.volume);
    if args.start > 0.0 {
        player.seek((args.start * 1_000_000.0) as u64);
    }
    player.play();

    let max_frames = args
        .duration
        .map(|seconds| (seconds.max(0.0) * args.rate as f64) as u64);

    let stats = wav::render_to_wav(&mut player, &args.output, max_frames)?;
    let seconds = stats.frames as f64 / args.rate as f64;

    println!(
        "wrote {} ({:.2}s, {} Hz stereo, peak {:.3}, rms {:.4})",
        args.output.display(),
        seconds,
        args.rate,
        stats.peak,
        stats.rms
    );

    Ok(())
}

fn describe(kind: &EventKind) -> String {
    match kind {
        EventKind::NoteOn {
            channel,
            key,
            velocity,
        } => format!("note_on  ch{channel:<2} key={key:<3} vel={velocity}"),
        EventKind::NoteOff {
            channel,
            key,
            velocity,
        } => format!("note_off ch{channel:<2} key={key:<3} vel={velocity}"),
        EventKind::ControlChange {
            channel,
            controller,
            value,
        } => format!("cc       ch{channel:<2} {controller}={value}"),
        EventKind::ProgramChange { channel, program } => {
            format!("program  ch{channel:<2} {program}")
        }
        EventKind::PitchBend { channel, value } => {
            format!("bend     ch{channel:<2} {value:+}")
        }
        EventKind::ChannelPressure { channel, value } => {
            format!("pressure ch{channel:<2} {value}")
        }
        EventKind::KeyPressure {
            channel,
            key,
            value,
        } => format!("keypress ch{channel:<2} key={key} {value}"),
        EventKind::Text { text_kind, text } => format!("{text_kind:?} {text:?}"),
        EventKind::Tempo {
            us_per_quarter,
            bpm,
        } => format!("tempo    {us_per_quarter}us/q ({bpm:.2} BPM)"),
        EventKind::SysEx => "sysex".to_owned(),
    }
}

fn format_name(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Kar => "KAR (MIDI karaoke)",
        SourceFormat::Ncn => "NCN (Nick Karaoke)",
        SourceFormat::Kmid => "KMID (embedded lyrics)",
        SourceFormat::Midi => "MIDI (plain)",
    }
}

fn opt(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("-")
}

fn time(us: u64) -> String {
    let total_ms = us / 1_000;
    let ms = total_ms % 1_000;
    let seconds = total_ms / 1_000 % 60;
    let minutes = total_ms / 60_000;
    format!("{minutes:02}:{seconds:02}.{ms:03}")
}
