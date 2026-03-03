use anyhow::Result;
use clap::{Parser, Subcommand};
use hearkit_core::config::AppConfig;
use hearkit_core::pipeline::MeetingPipeline;
use hearkit_core::storage::Storage;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "hearkit", about = "Local-first meeting recorder")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Record audio from the microphone
    Record,
    /// List all recordings
    List,
    /// Transcribe a recording
    Transcribe {
        /// Meeting ID to transcribe
        id: String,
    },
    /// Show a meeting's details
    Show {
        /// Meeting ID
        id: String,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("hearkit=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    let config = AppConfig::default();
    let data_dir = config.data_dir();
    let storage = Storage::new(data_dir)?;
    let mut pipeline = MeetingPipeline::new(config, storage);

    match cli.command {
        Commands::Record => cmd_record(&mut pipeline)?,
        Commands::List => cmd_list(&pipeline)?,
        Commands::Transcribe { id } => cmd_transcribe(&pipeline, &id)?,
        Commands::Show { id } => cmd_show(&pipeline, &id)?,
    }

    Ok(())
}

fn cmd_record(pipeline: &mut MeetingPipeline) -> Result<()> {
    println!("Starting recording... (press Enter to stop)");

    let handle = pipeline.start_recording()?;
    let id = handle.id.clone();
    println!("Recording ID: {id}");

    // Wait for Enter key (with Ctrl+C support)
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc_handler(r);

    // Wait for Enter or Ctrl+C
    let mut input = String::new();
    let stdin = io::stdin();

    // Use a thread to read stdin so we can also check the running flag
    let read_thread = std::thread::spawn(move || {
        let _ = stdin.read_line(&mut input);
    });

    // Spin until enter is pressed or ctrl+c
    while running.load(Ordering::SeqCst) {
        if read_thread.is_finished() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    println!("\nStopping recording...");
    let meeting = pipeline.stop_recording(handle)?;

    println!("Saved: {}", meeting.audio_path.display());
    println!("Duration: {:.1}s", meeting.duration_secs);
    println!("Meeting ID: {}", meeting.id);
    println!("\nTo transcribe: hearkit transcribe {}", meeting.id);

    Ok(())
}

fn cmd_list(pipeline: &MeetingPipeline) -> Result<()> {
    let meetings = pipeline.storage().list_meetings()?;

    if meetings.is_empty() {
        println!("No recordings yet. Run: hearkit record");
        return Ok(());
    }

    println!("{:<40} {:<22} {:>8}  {}", "ID", "DATE", "DURATION", "STATUS");
    println!("{}", "-".repeat(80));

    for m in &meetings {
        let status = match (&m.transcript, &m.analysis) {
            (Some(_), Some(_)) => "transcribed + analyzed",
            (Some(_), None) => "transcribed",
            _ => "recorded",
        };
        println!(
            "{:<40} {:<22} {:>6.0}s  {}",
            m.id,
            m.started_at.format("%Y-%m-%d %H:%M:%S"),
            m.duration_secs,
            status
        );
    }

    Ok(())
}

fn cmd_transcribe(pipeline: &MeetingPipeline, id: &str) -> Result<()> {
    let mut meeting = pipeline.storage().load_meeting(id)?;

    if meeting.transcript.is_some() {
        println!("Already transcribed.");
    } else {
        println!("Transcribing {}...", meeting.audio_path.display());
        pipeline.transcribe(&mut meeting)?;
        println!("Done!");
    }

    if let Some(ref t) = meeting.transcript {
        println!("\n--- Transcript ({:.1}s) ---\n", t.duration);
        for seg in &t.segments {
            println!("[{:>6.1}s] {}", seg.start, seg.text);
        }
        println!("\n--- Full text ---\n{}", t.full_text);
    }

    Ok(())
}

fn cmd_show(pipeline: &MeetingPipeline, id: &str) -> Result<()> {
    let meeting = pipeline.storage().load_meeting(id)?;

    println!("Meeting: {}", meeting.title);
    println!("ID:      {}", meeting.id);
    println!("Date:    {}", meeting.started_at.format("%Y-%m-%d %H:%M:%S"));
    println!("Duration: {:.1}s", meeting.duration_secs);
    println!("Audio:   {}", meeting.audio_path.display());

    if let Some(ref t) = meeting.transcript {
        println!("\n--- Transcript ({} segments, {:.1}s) ---\n", t.segments.len(), t.duration);
        println!("{}", t.full_text);
    } else {
        println!("\nNo transcript yet. Run: hearkit transcribe {}", meeting.id);
    }

    if let Some(ref a) = meeting.analysis {
        println!("\n--- Summary ---\n{}", a.summary);
        if !a.action_items.is_empty() {
            println!("\n--- Action Items ---");
            for item in &a.action_items {
                let assignee = item.assignee.as_deref().unwrap_or("unassigned");
                println!("  - {} ({})", item.description, assignee);
            }
        }
    }

    Ok(())
}

fn ctrlc_handler(running: Arc<AtomicBool>) {
    let _ = ctrlc::set_handler(move || {
        running.store(false, Ordering::SeqCst);
    });
}
