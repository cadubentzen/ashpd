//! Manual test client for the `org.freedesktop.portal.ScreenCast` audio request.
//!
//! Mirrors `xdg-desktop-portal/screencast-audio.py`: it drives the portal
//! frontend end to end (`CreateSession`, `SelectSources` with the public
//! `audio` `a{sv}` option, `Start`), prints the returned streams grouped by
//! `mapping_id`, and then holds until Ctrl+C.
//!
//! It does not open the PipeWire remote yet.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example screen_cast_audio --features screencast -- --audio include-self
//! ```

use std::collections::HashMap;

use ashpd::{
    desktop::screencast::{
        AudioOptions, MediaType, Screencast, SelectSourcesOptions, SourceType, Stream,
    },
    enumflags2::BitFlags,
};
use clap::{Parser, ValueEnum};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AudioMode {
    /// Do not request audio at all.
    None,
    /// Request audio, excluding the requester's own audio.
    ExcludeSelf,
    /// Request audio, allowing the requester's own audio.
    IncludeSelf,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum SourceArg {
    Monitor,
    Window,
    Virtual,
}

impl From<SourceArg> for SourceType {
    fn from(arg: SourceArg) -> Self {
        match arg {
            SourceArg::Monitor => SourceType::Monitor,
            SourceArg::Window => SourceType::Window,
            SourceArg::Virtual => SourceType::Virtual,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    about = "Exercise the ScreenCast portal audio request and print the returned video/audio streams"
)]
struct Args {
    /// Audio request mode (`none` omits the option entirely).
    #[arg(long, value_enum, default_value_t = AudioMode::ExcludeSelf)]
    audio: AudioMode,

    /// Comma-separated source types to offer.
    #[arg(long, value_enum, value_delimiter = ',', default_value = "monitor,window")]
    types: Vec<SourceArg>,

    /// Allow selecting multiple sources.
    #[arg(long)]
    multiple: bool,

    /// Do not close the session on exit.
    #[arg(long)]
    keep_session: bool,
}

fn audio_option(mode: AudioMode) -> Option<AudioOptions> {
    match mode {
        AudioMode::None => None,
        AudioMode::ExcludeSelf => Some(AudioOptions::default()),
        AudioMode::IncludeSelf => Some(AudioOptions::default().set_include_self(true)),
    }
}

fn print_streams(streams: &[Stream]) {
    if streams.is_empty() {
        println!("streams: (none)");
        return;
    }

    println!("streams:");
    for (index, stream) in streams.iter().enumerate() {
        let mapping_id = stream.mapping_id().unwrap_or("(none)");
        let serial = stream
            .pipewire_serial()
            .map(|serial| serial.to_string())
            .unwrap_or_else(|| "(none)".to_string());
        println!(
            "  [{index}] node={} media_type={:?} mapping_id={} pipewire-serial={} source_type={:?}",
            stream.pipe_wire_node_id(),
            stream.media_type(),
            mapping_id,
            serial,
            stream.source_type(),
        );
    }
}

fn print_association(streams: &[Stream]) {
    let mut videos: HashMap<&str, u32> = HashMap::new();
    let mut audios: Vec<(u32, Option<&str>)> = Vec::new();

    for stream in streams {
        match stream.media_type() {
            MediaType::Audio => audios.push((stream.pipe_wire_node_id(), stream.mapping_id())),
            MediaType::Video => {
                if let Some(mapping_id) = stream.mapping_id() {
                    videos.entry(mapping_id).or_insert(stream.pipe_wire_node_id());
                }
            }
        }
    }

    if audios.is_empty() {
        println!("audio association: no audio streams returned");
        return;
    }

    println!("audio association:");
    for (node_id, mapping_id) in audios {
        match mapping_id {
            None => println!("  audio node {node_id}: no mapping_id (unassociated)"),
            Some(mapping_id) => match videos.get(mapping_id) {
                Some(video_node) => println!(
                    "  audio node {node_id} -> video node {video_node} (mapping_id={mapping_id})"
                ),
                None => println!(
                    "  audio node {node_id}: mapping_id={mapping_id} matches no video (unassociated)"
                ),
            },
        }
    }
}

#[tokio::main]
async fn main() -> ashpd::Result<()> {
    let args = Args::parse();

    let proxy = Screencast::new().await?;
    println!("version: {}", proxy.version());

    let sources = args
        .types
        .iter()
        .copied()
        .map(SourceType::from)
        .fold(BitFlags::empty(), |sources, source| sources | source);

    let session = proxy.create_session(Default::default()).await?;

    println!(
        "select-sources: audio={:?} types={:?} multiple={}",
        args.audio, args.types, args.multiple
    );
    proxy
        .select_sources(
            &session,
            SelectSourcesOptions::default()
                .set_sources(sources)
                .set_multiple(args.multiple)
                .set_audio(audio_option(args.audio)),
        )
        .await?
        .response()?;

    let response = proxy
        .start(&session, None, Default::default())
        .await?
        .response()?;

    let streams = response.streams();
    print_streams(streams);
    print_association(streams);

    println!("holding until Ctrl+C...");
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for Ctrl+C");
    println!("interrupted");

    if args.keep_session {
        println!("session left running (--keep-session)");
    } else {
        session.close().await?;
        println!("session closed");
    }

    Ok(())
}
