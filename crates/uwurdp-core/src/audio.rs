//! Remote audio (RDPSND), played on this machine through cpal.
//!
//! This replaces the cpal backend of `ironrdp-rdpsnd-native`, which builds
//! its output stream but never starts it. On WASAPI such a stream stays
//! stopped: nothing is heard, nothing drains the backend's unbounded queue,
//! and every wave the server sends stays in memory — over 600 MB an hour per
//! session while the remote plays sound, until the app runs out of memory.
//!
//! Here the stream is started, and between the session and the device sits a
//! [`Pcm`] buffer that never holds more than [`MAX_BUFFERED`] of sound. The
//! server does not wait for playback (IronRDP confirms each wave as it
//! arrives), so whatever the device cannot play in time is dropped, oldest
//! first. The device's callback never blocks: a dry buffer plays silence.

use ironrdp_rdpsnd::client::RdpsndClientHandler;
use ironrdp_rdpsnd::pdu::{AudioFormat, PitchPdu, VolumePdu, WaveFormat};
use parking_lot::Mutex;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;
use tracing::{debug, warn};

/// The most sound waiting for the device; anything older is dropped.
pub(crate) const MAX_BUFFERED: Duration = Duration::from_millis(500);

/// Plays the session's sound on the default output device.
#[derive(Debug)]
pub(crate) struct CpalBackend {
    formats: Vec<AudioFormat>,
    output: Option<Output>,
}

impl CpalBackend {
    pub(crate) fn new() -> Self {
        Self {
            // What cpal plays without a decoder, on every platform.
            formats: vec![AudioFormat {
                format: WaveFormat::PCM,
                n_channels: 2,
                n_samples_per_sec: 44100,
                n_avg_bytes_per_sec: 176_400,
                n_block_align: 4,
                bits_per_sample: 16,
                data: None,
            }],
            output: None,
        }
    }
}

impl RdpsndClientHandler for CpalBackend {
    fn get_formats(&self) -> &[AudioFormat] {
        &self.formats
    }

    fn wave(&mut self, format_no: usize, _ts: u32, data: Cow<'_, [u8]>) {
        if self
            .output
            .as_ref()
            .is_some_and(|output| output.format_no != format_no)
        {
            self.output = None;
        }
        if self.output.is_none() {
            let Some(format) = self.formats.get(format_no) else {
                debug!(format_no, "wave in a format we never offered");
                return;
            };
            self.output = Some(Output::start(format_no, format.clone()));
        }
        if let Some(output) = &self.output {
            output.pcm.lock().push(&data);
        }
    }

    fn set_volume(&mut self, volume: VolumePdu) {
        debug!(?volume, "remote volume change ignored");
    }

    fn set_pitch(&mut self, pitch: PitchPdu) {
        debug!(?pitch, "remote pitch change ignored");
    }

    fn close(&mut self) {
        self.output = None;
    }
}

/// One open output stream. The cpal stream is not `Send`, so it lives on a
/// thread of its own; dropping this stops and joins that thread.
#[derive(Debug)]
struct Output {
    format_no: usize,
    pcm: Arc<Mutex<Pcm>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Output {
    fn start(format_no: usize, format: AudioFormat) -> Self {
        let pcm = Arc::new(Mutex::new(Pcm::new(capacity(&format), frame_size(&format))));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = std::thread::Builder::new()
            .name("uwurdp-audio".into())
            .spawn({
                let pcm = pcm.clone();
                let stop = stop.clone();
                move || play(&format, pcm, &stop)
            });
        let thread = match thread {
            Ok(thread) => Some(thread),
            Err(error) => {
                warn!(%error, "cannot start the audio thread; remote sound is off");
                None
            }
        };
        Self {
            format_no,
            pcm,
            stop,
            thread,
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// The audio thread: opens the stream, starts it, and keeps it until told to
/// stop. A machine without a usable output device just stays silent.
fn play(format: &AudioFormat, pcm: Arc<Mutex<Pcm>>, stop: &AtomicBool) {
    use cpal::traits::StreamTrait as _;

    let stream = match open_stream(format, pcm) {
        Ok(stream) => stream,
        Err(error) => {
            warn!(%error, "remote sound cannot play here");
            return;
        }
    };
    if let Err(error) = stream.play() {
        warn!(%error, "the audio device would not start");
        return;
    }
    debug!("remote sound playing");
    while !stop.load(Ordering::Acquire) {
        std::thread::park();
    }
    drop(stream);
    debug!("remote sound stopped");
}

fn open_stream(format: &AudioFormat, pcm: Arc<Mutex<Pcm>>) -> Result<cpal::Stream, String> {
    use cpal::traits::{DeviceTrait as _, HostTrait as _};

    if format.format != WaveFormat::PCM {
        return Err(format!("unsupported wave format {:?}", format.format));
    }
    let (sample_format, silence) = match format.bits_per_sample {
        8 => (cpal::SampleFormat::U8, 0x80),
        16 => (cpal::SampleFormat::I16, 0),
        bits => return Err(format!("unsupported sample size of {bits} bits")),
    };
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("no audio output device")?;
    let config = cpal::StreamConfig {
        channels: format.n_channels,
        sample_rate: format.n_samples_per_sec,
        buffer_size: cpal::BufferSize::Default,
    };
    device
        .build_output_stream_raw(
            &config,
            sample_format,
            move |data: &mut cpal::Data, _: &cpal::OutputCallbackInfo| {
                pcm.lock().pull(data.bytes_mut(), silence);
            },
            |error| warn!(%error, "audio output failed"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// Bytes per sample frame (all channels), never zero.
fn frame_size(format: &AudioFormat) -> usize {
    let from_format = usize::from(format.n_channels) * usize::from(format.bits_per_sample) / 8;
    match usize::from(format.n_block_align) {
        0 => from_format.max(1),
        align => align,
    }
}

/// Bytes of [`MAX_BUFFERED`] sound in `format`.
fn capacity(format: &AudioFormat) -> usize {
    let per_second = u128::from(format.n_samples_per_sec) * frame_size(format) as u128;
    usize::try_from(per_second * MAX_BUFFERED.as_millis() / 1000).unwrap_or(usize::MAX)
}

/// Sound on its way to the device: a byte queue of whole sample frames that
/// drops its oldest frames rather than grow past its capacity.
#[derive(Debug)]
struct Pcm {
    bytes: VecDeque<u8>,
    capacity: usize,
    frame: usize,
}

impl Pcm {
    fn new(capacity: usize, frame: usize) -> Self {
        let frame = frame.max(1);
        // At least one frame, and whole frames only.
        let capacity = (capacity - capacity % frame).max(frame);
        Self {
            bytes: VecDeque::with_capacity(capacity),
            capacity,
            frame,
        }
    }

    fn push(&mut self, data: &[u8]) {
        let whole = data.len() - data.len() % self.frame;
        // More than fits on its own: only its newest part can still play.
        let data = &data[whole.saturating_sub(self.capacity)..whole];
        self.bytes.extend(data);
        let excess = self.bytes.len().saturating_sub(self.capacity);
        if excess > 0 {
            self.bytes.drain(..excess);
        }
    }

    /// Fills `out` from the front of the queue, and with `silence` where the
    /// queue runs dry.
    fn pull(&mut self, out: &mut [u8], silence: u8) {
        let n = self.bytes.len().min(out.len());
        for (dst, src) in out.iter_mut().zip(self.bytes.drain(..n)) {
            *dst = src;
        }
        out[n..].fill(silence);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_never_grows_past_its_capacity() {
        let mut pcm = Pcm::new(40, 4);
        for i in 0..100u8 {
            pcm.push(&[i; 8]);
        }
        assert_eq!(pcm.len(), 40);
        // The newest sound is what is left.
        let mut out = [0u8; 40];
        pcm.pull(&mut out, 0);
        assert_eq!(&out[32..], &[99; 8]);
        assert_eq!(&out[..8], &[95; 8]);
    }

    #[test]
    fn a_dry_queue_plays_silence() {
        let mut pcm = Pcm::new(64, 4);
        pcm.push(&[7; 8]);
        let mut out = [1u8; 16];
        pcm.pull(&mut out, 0x80);
        assert_eq!(&out[..8], &[7; 8]);
        assert_eq!(&out[8..], &[0x80; 8]);
        assert_eq!(pcm.len(), 0);
    }

    #[test]
    fn only_whole_frames_are_kept() {
        let mut pcm = Pcm::new(10, 4);
        // Capacity rounds down to two frames.
        pcm.push(&[1; 6]);
        assert_eq!(pcm.len(), 4);
        pcm.push(&[2; 100]);
        assert_eq!(pcm.len(), 8);
        let mut out = [0u8; 8];
        pcm.pull(&mut out, 0);
        assert_eq!(out, [2; 8]);
    }

    /// Needs a sound device, which CI machines lack:
    /// `cargo test -p uwurdp-core audio -- --ignored`
    #[test]
    #[ignore]
    fn the_default_device_drains_the_queue() {
        let mut backend = CpalBackend::new();
        let tenth = vec![0u8; 17_640];
        for _ in 0..5 {
            backend.wave(0, 0, Cow::Borrowed(&tenth));
        }
        let pcm = backend.output.as_ref().expect("an output").pcm.clone();
        assert_eq!(pcm.lock().len(), 88_200);
        // Opening a WASAPI stream can take seconds on some machines.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while pcm.lock().len() == 88_200 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(pcm.lock().len() < 88_200, "the stream never started");
        backend.close();
        assert!(backend.output.is_none());
    }

    #[test]
    fn half_a_second_of_cd_audio_is_the_limit() {
        let backend = CpalBackend::new();
        let format = &backend.get_formats()[0];
        assert_eq!(frame_size(format), 4);
        assert_eq!(capacity(format), 88_200);
    }
}
