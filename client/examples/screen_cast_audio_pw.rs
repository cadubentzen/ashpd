//! Capture the ScreenCast portal's audio stream and write it to `capture.wav`.
//!
//! Rust/`ashpd` analogue of `examples/pw_webrtc_capture.c`: it drives the
//! portal (`CreateSession` -> `SelectSources` with the public audio `a{sv}` ->
//! `Start` -> `OpenPipeWireRemote`), connects to the returned PipeWire socket
//! with `pipewire-rs`, captures F32LE/48 kHz/stereo, re-chunks it into exact
//! 10 ms frames through an `rtrb` ring buffer, and writes 16-bit PCM to
//! `capture.wav`. The per-chunk WebRTC hand-off point is intentionally a stub.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example screen_cast_audio_pw --features screencast,pipewire -- --audio include-self
//! ```

use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    os::fd::OwnedFd,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::{self, Thread},
};

use ashpd::desktop::screencast::{
    AudioOptions, MediaType, Screencast, SelectSourcesOptions, SourceType, Stream,
};
use clap::{Parser, ValueEnum};
use pipewire as pw;
use pw::{properties::properties, spa};
use rtrb::{Consumer, Producer, RingBuffer};
use spa::{
    param::{
        ParamType,
        audio::{AudioFormat, AudioInfoRaw},
        format::{MediaSubtype, MediaType as SpaMediaType},
        format_utils,
    },
    pod::{Object, Pod, Value, serialize::PodSerializer},
    utils::Direction,
};

const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const CHUNK_MS: usize = 10;
const FRAMES_PER_CHUNK: usize = SAMPLE_RATE as usize * CHUNK_MS / 1000;
const QUANTUM_FRAMES: u32 = 1024;
const BYTES_PER_FRAME: usize = CHANNELS * size_of::<f32>();
const CHUNK_BYTES: usize = FRAMES_PER_CHUNK * BYTES_PER_FRAME;
const RING_BYTES: usize = 1 << 16;
const WAV_PATH: &str = "capture.wav";

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
#[command(about = "Capture the ScreenCast portal audio stream to capture.wav")]
struct Args {
    /// Audio request mode (`none` omits the option entirely).
    #[arg(long, value_enum, default_value_t = AudioMode::ExcludeSelf)]
    audio: AudioMode,

    /// Comma-separated source types to offer.
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "monitor,window"
    )]
    types: Vec<SourceArg>,

    /// Allow selecting multiple sources.
    #[arg(long)]
    multiple: bool,

    /// Do not close the session on exit.
    #[arg(long)]
    keep_session: bool,

    /// Index of the returned audio stream to capture.
    #[arg(long, default_value_t = 0)]
    stream_index: usize,
}

fn audio_option(mode: AudioMode) -> Option<AudioOptions> {
    match mode {
        AudioMode::None => None,
        AudioMode::ExcludeSelf => Some(AudioOptions::default()),
        AudioMode::IncludeSelf => Some(AudioOptions::default().set_include_self(true)),
    }
}

/// Counters shared between the PipeWire RT callback and the WAV writer thread.
#[derive(Default)]
struct Stats {
    running: AtomicBool,
    overruns: AtomicU32,
    last_in_frames: AtomicU32,
}

/// A minimal 16-bit PCM WAV writer that patches the header size on close.
struct WavWriter {
    file: File,
    frames: u32,
}

impl WavWriter {
    fn create(path: &str) -> io::Result<Self> {
        let mut file = File::create(path)?;
        write_wav_header(&mut file, 0)?;
        Ok(Self { file, frames: 0 })
    }

    fn write_samples(&mut self, samples: &[i16]) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        self.file.write_all(&bytes)?;
        self.frames += (samples.len() / CHANNELS) as u32;
        Ok(())
    }

    fn finalize(mut self) -> io::Result<()> {
        let data_bytes = self.frames * CHANNELS as u32 * 2;
        self.file.seek(SeekFrom::Start(0))?;
        write_wav_header(&mut self.file, data_bytes)?;
        self.file.flush()
    }
}

fn write_wav_header(file: &mut File, data_bytes: u32) -> io::Result<()> {
    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * 2;
    let block_align = CHANNELS as u16 * 2;

    file.write_all(b"RIFF")?;
    file.write_all(&(36 + data_bytes).to_le_bytes())?;
    file.write_all(b"WAVE")?;
    file.write_all(b"fmt ")?;
    file.write_all(&16u32.to_le_bytes())?;
    file.write_all(&1u16.to_le_bytes())?; // PCM
    file.write_all(&(CHANNELS as u16).to_le_bytes())?;
    file.write_all(&SAMPLE_RATE.to_le_bytes())?;
    file.write_all(&byte_rate.to_le_bytes())?;
    file.write_all(&block_align.to_le_bytes())?;
    file.write_all(&16u16.to_le_bytes())?;
    file.write_all(b"data")?;
    file.write_all(&data_bytes.to_le_bytes())?;
    Ok(())
}

/// Convert interleaved f32 in [-1, 1] to interleaved i16, clamping overflow.
fn f32_to_s16(input: &[f32], output: &mut [i16]) {
    for (out, &sample) in output.iter_mut().zip(input) {
        *out = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    }
}

/// Non-RT consumer: drain exact 10 ms chunks, convert, and write them to disk.
fn consumer_loop(mut consumer: Consumer<u8>, stats: Arc<Stats>) {
    let mut wav = match WavWriter::create(WAV_PATH) {
        Ok(wav) => wav,
        Err(err) => {
            eprintln!("failed to open {WAV_PATH}: {err}");
            return;
        }
    };

    let mut bytes = [0u8; CHUNK_BYTES];
    let mut samples = [0f32; FRAMES_PER_CHUNK * CHANNELS];
    let mut s16 = [0i16; FRAMES_PER_CHUNK * CHANNELS];
    let mut seq: u64 = 0;

    loop {
        while consumer.slots() >= CHUNK_BYTES {
            if consumer.pop_entire_slice(&mut bytes).is_err() {
                break;
            }

            for (sample, raw) in samples.iter_mut().zip(bytes.chunks_exact(4)) {
                *sample = f32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
            }
            f32_to_s16(&samples, &mut s16);

            // Hook WebRTC's RecordedDataIsAvailable here with `s16`.
            if let Err(err) = wav.write_samples(&s16) {
                eprintln!("failed to write {WAV_PATH}: {err}");
            }

            if seq.is_multiple_of(100) {
                let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
                let rms = (sum / samples.len() as f64).sqrt();
                println!(
                    "chunk {seq:8}  frames={FRAMES_PER_CHUNK}  rms={rms:.5}  in_quantum={}  overruns={}",
                    stats.last_in_frames.load(Ordering::Relaxed),
                    stats.overruns.load(Ordering::Relaxed),
                );
            }
            seq += 1;
        }

        if !stats.running.load(Ordering::Relaxed) {
            break;
        }
        thread::park();
    }

    if let Err(err) = wav.finalize() {
        eprintln!("failed to finalize {WAV_PATH}: {err}");
    }
}

/// RT `process` callback: copy whole-frame bytes into the ring, or drop the
/// quantum and count an overrun. No allocation, no locks, no blocking.
fn process_capture(
    stream: &pw::stream::Stream,
    producer: &mut Producer<u8>,
    consumer_thread: &Thread,
    stats: &Stats,
) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    if datas.is_empty() {
        return;
    }

    let data = &mut datas[0];
    let offset = data.chunk().offset() as usize;
    let size = data.chunk().size() as usize;

    let Some(bytes) = data.data() else {
        return;
    };
    let max = bytes.len();
    let offset = offset.min(max);
    let size = (size.min(max - offset)) & !(BYTES_PER_FRAME - 1);
    stats
        .last_in_frames
        .store((size / BYTES_PER_FRAME) as u32, Ordering::Relaxed);

    if size == 0 {
        return;
    }

    let src = &bytes[offset..offset + size];
    if producer.slots() >= size {
        producer
            .push_entire_slice(src)
            .expect("slots were checked above");
        consumer_thread.unpark();
    } else {
        stats.overruns.fetch_add(1, Ordering::Relaxed);
    }
}

/// Connect to the portal's PipeWire socket and capture until SIGINT/SIGTERM.
fn run_capture(
    fd: OwnedFd,
    target: Option<String>,
    node_id: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    pw::init();

    let mainloop = Rc::new(pw::main_loop::MainLoopBox::new(None)?);
    let context = pw::context::ContextBox::new(mainloop.loop_(), None)?;
    let core = context.connect_fd(fd, None)?;

    let stats = Arc::new(Stats::default());
    stats.running.store(true, Ordering::Relaxed);
    let (mut producer, consumer) = RingBuffer::<u8>::new(RING_BYTES);

    let consumer_stats = Arc::clone(&stats);
    let consumer_handle = thread::Builder::new()
        .name("ashpd-wav-writer".into())
        .spawn(move || consumer_loop(consumer, consumer_stats))?;
    let consumer_thread = consumer_handle.thread().clone();

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Communication",
        *pw::keys::APP_NAME => "ashpd-screencast-audio-capture",
    };
    props.insert(
        *pw::keys::NODE_LATENCY,
        format!("{QUANTUM_FRAMES}/{SAMPLE_RATE}"),
    );
    // Targets are source nodes, so stream.capture.sink is intentionally not set.
    if let Some(target) = target.as_deref() {
        // `pw::keys::TARGET_OBJECT` is gated behind the `v0_3_44` feature, which
        // this crate does not enable; use the key value directly (the C reference
        // defines `PW_KEY_TARGET_OBJECT` as `"target.object"`).
        props.insert("target.object", target);
    }

    let stream = pw::stream::StreamBox::new(&core, "ashpd-audio-capture", props)?;

    let state_loop = Rc::clone(&mainloop);
    let process_stats = Arc::clone(&stats);
    let listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, old, new| {
            eprintln!("stream state: {old:?} -> {new:?}");
            if matches!(new, pw::stream::StreamState::Error(_)) {
                state_loop.quit();
            }
        })
        .param_changed(|_, _, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type != SpaMediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            let mut info = AudioInfoRaw::new();
            if info.parse(param).is_ok() {
                eprintln!(
                    "negotiated: format={:?} rate={} channels={}",
                    info.format(),
                    info.rate(),
                    info.channels()
                );
            }
        })
        .process(move |stream, _| {
            process_capture(stream, &mut producer, &consumer_thread, &process_stats)
        })
        .register()?;

    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    audio_info.set_rate(SAMPLE_RATE);
    audio_info.set_channels(CHANNELS as u32);
    let mut positions = [0u32; 64];
    positions[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    positions[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    audio_info.set_position(positions);

    let object = Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(object))
        .unwrap()
        .0
        .into_inner();
    let mut params = [Pod::from_bytes(&values).unwrap()];

    stream.connect(
        Direction::Input,
        if target.is_some() {
            None
        } else {
            Some(node_id)
        },
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    let sigint_loop = Rc::clone(&mainloop);
    let _sigint = mainloop
        .loop_()
        .add_signal_local(pw::loop_::Signal::SIGINT, move || sigint_loop.quit());
    let sigterm_loop = Rc::clone(&mainloop);
    let _sigterm = mainloop
        .loop_()
        .add_signal_local(pw::loop_::Signal::SIGTERM, move || sigterm_loop.quit());

    println!("capturing to {WAV_PATH}; press Ctrl+C to stop");
    mainloop.run();

    // Stop the RT callbacks before tearing down the consumer.
    drop(stream);
    drop(listener);

    stats.running.store(false, Ordering::Relaxed);
    consumer_handle.thread().unpark();
    if consumer_handle.join().is_err() {
        eprintln!("WAV writer thread panicked");
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let proxy = Screencast::new().await?;
    println!("version: {}", proxy.version());

    let sources = args
        .types
        .iter()
        .copied()
        .map(SourceType::from)
        .fold(ashpd::enumflags2::BitFlags::empty(), |sources, source| {
            sources | source
        });

    let session = proxy.create_session(Default::default()).await?;
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
    let audio: Vec<&Stream> = streams
        .iter()
        .filter(|stream| stream.media_type() == MediaType::Audio)
        .collect();
    let stream = audio.get(args.stream_index).copied().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no audio stream at index {}", args.stream_index),
        )
    })?;

    let target = stream.pipewire_serial().map(|serial| serial.to_string());
    let node_id = stream.pipe_wire_node_id();
    println!(
        "capturing audio node {node_id} (target={})",
        target.as_deref().unwrap_or("node-id")
    );

    let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await?;

    let result = run_capture(fd, target, node_id);

    if !args.keep_session {
        if let Err(err) = session.close().await {
            eprintln!("failed to close session: {err}");
        }
    }

    result
}
