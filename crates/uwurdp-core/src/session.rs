//! The active session: one task per connection, modeled on ironrdp-client's
//! `active_session`.
//!
//! The task owns the connection and the decoded desktop image. It reacts to
//! three things: PDUs from the server, [`Command`]s from the app (input,
//! resize, acks, close — pushed from synchronous code through an unbounded
//! channel), and its own timers (frame flush, resize debounce, close
//! deadline).
//!
//! ## Frames and flow control
//!
//! Decoded updates only mark the image dirty ([`DirtyRegion`]). At most every
//! [`FLUSH_INTERVAL`], and only while fewer than [`ACK_WINDOW`] `BITMAPS`
//! messages wait for the page's `ack`, the *current* contents of the dirty
//! region are sent. A page that falls behind therefore sees fewer, larger
//! updates — never stale or missing pixels. Pointer and size messages are
//! small and go out immediately.
//!
//! With the graphics pipeline the pixels come from [`gfx::Pipeline`]'s
//! output instead of IronRDP's image; after every step the session takes the
//! pipeline's changes (new size, dirty rectangles) into the same machinery.

use crate::clipboard::{ClipboardHandle, WorkerMsg};
use crate::connect::{Established, Stream};
use crate::dirty::{DirtyRegion, Rect};
use crate::frame::{self, CloseReason};
use crate::gfx;
use crate::input::{InputEvent, InputState};
use crate::sink::{FrameSink, SinkError};
use ironrdp_cliprdr::backend::ClipboardMessage;
use ironrdp_cliprdr::CliprdrClient;
use ironrdp_connector::connection_activation::ConnectionActivationState;
use ironrdp_core::WriteBuf;
use ironrdp_displaycontrol::pdu::MonitorLayoutEntry;
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_pdu::geometry::InclusiveRectangle;
use ironrdp_pdu::rdp::server_error_info::{ErrorInfo, ProtocolIndependentCode};
use ironrdp_session::image::DecodedImage;
use ironrdp_session::{fast_path, ActiveStage, ActiveStageBuilder, ActiveStageOutput};
use ironrdp_session::{GracefulDisconnectReason, SessionResult};
use ironrdp_svc::SvcProcessorMessages;
use ironrdp_tokio::{split_tokio_framed, FramedWrite as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{sleep_until, Instant};
use tracing::{debug, info, trace, warn};

/// Shortest time between two `BITMAPS` messages: one frame at 60 Hz.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(16);

/// How many `BITMAPS` messages may be unacknowledged before we hold back.
/// Two keeps the page busy (one drawing, one queued) without letting a slow
/// page build up a backlog.
pub const ACK_WINDOW: u32 = 2;

/// Resize requests arrive in bursts while the user drags the window; each
/// one costs the server a deactivation-reactivation, so only the last one
/// within this window is sent.
pub const RESIZE_DEBOUNCE: Duration = Duration::from_millis(250);

/// How long to wait for the server to acknowledge a graceful shutdown before
/// dropping the connection anyway.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

/// Fast-path input PDUs can carry up to 255 events; stay well below.
const MAX_EVENTS_PER_PDU: usize = 64;

/// What the app asks the session to do.
#[derive(Debug)]
pub(crate) enum Command {
    Input(Vec<InputEvent>),
    Resize {
        width: u16,
        height: u16,
        scale_factor: u32,
    },
    Ack,
    ClipboardChanged,
    /// From the clipboard worker or backend.
    Clipboard(ClipboardMessage),
    Close,
}

/// Why the session loop ended.
struct Ending {
    reason: CloseReason,
    message: String,
}

impl Ending {
    fn new(reason: CloseReason, message: impl Into<String>) -> Self {
        Self {
            reason,
            message: message.into(),
        }
    }
}

/// The desktop image and what the page has not seen of it yet.
struct Screen {
    image: DecodedImage,
    dirty: DirtyRegion,
    unacked: u32,
    last_flush: Instant,
}

impl Screen {
    fn new(width: u16, height: u16) -> Self {
        let mut screen = Self {
            image: DecodedImage::new(PixelFormat::RgbA32, width, height),
            dirty: DirtyRegion::new(),
            unacked: 0,
            last_flush: Instant::now(),
        };
        screen.mark_all_dirty();
        screen
    }

    fn mark_all_dirty(&mut self) {
        self.dirty.clear();
        self.dirty
            .add(Rect::new(0, 0, self.image.width(), self.image.height()));
    }

    fn mark_dirty(&mut self, rect: &InclusiveRectangle) {
        let rect = Rect::new(
            rect.left,
            rect.top,
            rect.right.saturating_sub(rect.left).saturating_add(1),
            rect.bottom.saturating_sub(rect.top).saturating_add(1),
        )
        .clip(self.image.width(), self.image.height());
        self.dirty.add(rect);
    }

    fn can_flush(&self) -> bool {
        !self.dirty.is_empty() && self.unacked < ACK_WINDOW
    }

    /// Sends as much of the dirty region as the ack window allows; whatever
    /// does not fit stays dirty for the next round. The pixels come from the
    /// graphics pipeline once it draws, from IronRDP's image before.
    fn flush(&mut self, out: &Output<'_>, graphics: Option<&gfx::Shared>) {
        let pipeline = graphics.map(|g| g.lock());
        let (pixels, stride, source_width, source_height) =
            match pipeline.as_ref().and_then(|p| p.output()) {
                Some(output) => (
                    &output.data[..],
                    output.stride(),
                    output.width,
                    output.height,
                ),
                None => (
                    self.image.data(),
                    self.image.stride(),
                    self.image.width(),
                    self.image.height(),
                ),
            };
        let width = self.image.width().min(source_width);
        let height = self.image.height().min(source_height);
        let rects: Vec<Rect> = self
            .dirty
            .take()
            .into_iter()
            .map(|r| r.clip(width, height))
            .filter(|r| !r.is_empty())
            .collect();
        let mut messages = frame::plan_bitmaps(&rects).into_iter();
        for message in messages.by_ref() {
            out.send_owned(frame::bitmaps(pixels, stride, &message));
            self.unacked += 1;
            if self.unacked >= ACK_WINDOW {
                break;
            }
        }
        for rest in messages.flatten() {
            self.dirty.add(rest);
        }
        self.last_flush = Instant::now();
    }
}

/// The sink, plus a note of whether it went away.
struct Output<'a> {
    sink: &'a dyn FrameSink,
    gone: AtomicBool,
}

impl Output<'_> {
    fn send(&self, message: &[u8]) {
        if !self.gone.load(Ordering::Relaxed) {
            self.delivered(self.sink.send(message));
        }
    }

    fn send_owned(&self, message: Vec<u8>) {
        if !self.gone.load(Ordering::Relaxed) {
            self.delivered(self.sink.send_owned(message));
        }
    }

    fn delivered(&self, result: Result<(), SinkError>) {
        match result {
            Ok(()) => {}
            Err(SinkError::Closed) => {
                debug!("the page is gone; closing the session");
                self.gone.store(true, Ordering::Relaxed);
            }
            Err(SinkError::Other(e)) => warn!(error = %e, "could not deliver a frame message"),
        }
    }
}

pub(crate) struct SessionParts {
    pub established: Established,
    pub commands: mpsc::UnboundedReceiver<Command>,
    pub clipboard: Option<ClipboardHandle>,
    pub graphics: Option<gfx::Shared>,
}

/// Runs the session to its end, then sends `CLOSED` and finishes the sink.
pub(crate) async fn run(parts: SessionParts, sink: &dyn FrameSink) {
    let out = Output {
        sink,
        gone: AtomicBool::new(false),
    };
    let ending = drive(parts, &out).await;
    info!(reason = ?ending.reason, message = %ending.message, "RDP session ended");
    out.send(&frame::closed(ending.reason, &ending.message));
    sink.finish();
}

async fn drive(parts: SessionParts, out: &Output<'_>) -> Ending {
    let SessionParts {
        established: Established { result, framed },
        mut commands,
        clipboard,
        graphics,
    } = parts;

    let (mut reader, mut writer) = split_tokio_framed(framed);
    let size = result.desktop_size;
    let activation_factory = result.activation_factory;
    let mut active_stage = ActiveStageBuilder {
        static_channels: result.static_channels,
        user_channel_id: result.user_channel_id,
        io_channel_id: result.io_channel_id,
        message_channel_id: result.message_channel_id,
        share_id: result.share_id,
        compression_type: result.compression_type,
        enable_server_pointer: result.enable_server_pointer,
        pointer_software_rendering: result.pointer_software_rendering,
    }
    .build();

    let mut screen = Screen::new(size.width, size.height);
    let mut input = InputState::new(size.width, size.height);
    out.send(&frame::desktop_size(size.width, size.height));

    let mut pending_resize: Option<(u32, u32, u32)> = None;
    let mut resize_at = Instant::now();
    let mut close_deadline: Option<Instant> = None;
    let mut commands_open = true;

    loop {
        if out.gone.load(Ordering::Relaxed) && close_deadline.is_none() {
            // Nobody is watching any more: leave like the user closed it.
            close_deadline = Some(Instant::now() + CLOSE_TIMEOUT);
            if let Some(ending) = write_outputs(&mut writer, active_stage.graceful_shutdown()).await
            {
                return ending;
            }
        }

        let can_flush = screen.can_flush();
        let flush_at = screen.last_flush + FLUSH_INTERVAL;
        let far = Instant::now() + Duration::from_secs(3600);

        let outputs = tokio::select! {
            frame = reader.read_pdu() => match frame {
                Ok((action, payload)) => {
                    trace!(?action, len = payload.len(), "PDU");
                    active_stage.process(&mut screen.image, action, &payload)
                }
                Err(e) => {
                    return if close_deadline.is_some() {
                        Ending::new(CloseReason::Disconnect, "Disconnected.")
                    } else if matches!(e.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted) {
                        Ending::new(CloseReason::Server, "The server closed the connection.")
                    } else {
                        Ending::new(CloseReason::Error, format!("The connection was lost: {e}"))
                    };
                }
            },
            command = commands.recv(), if commands_open => match command {
                // Every sender is gone: nobody can talk to this session any
                // more, so close it (and stop polling the closed channel).
                None => {
                    commands_open = false;
                    close_request(&active_stage, &mut close_deadline)
                }
                Some(Command::Close) => close_request(&active_stage, &mut close_deadline),
                Some(Command::Ack) => {
                    screen.unacked = screen.unacked.saturating_sub(1);
                    Ok(Vec::new())
                }
                Some(Command::Input(events)) => {
                    let events = input.translate(&events);
                    let mut outputs = Vec::new();
                    for chunk in events.chunks(MAX_EVENTS_PER_PDU) {
                        match active_stage.process_fastpath_input(&mut screen.image, chunk) {
                            Ok(o) => outputs.extend(o),
                            Err(e) => warn!(error = %e.report(), "input could not be encoded"),
                        }
                    }
                    Ok(outputs)
                }
                Some(Command::Resize { width, height, scale_factor }) => {
                    let (w, h) = MonitorLayoutEntry::adjust_display_size(u32::from(width), u32::from(height));
                    pending_resize = Some((w, h, scale_factor));
                    resize_at = Instant::now() + RESIZE_DEBOUNCE;
                    Ok(Vec::new())
                }
                Some(Command::ClipboardChanged) => {
                    if let Some(clipboard) = &clipboard {
                        clipboard.send(WorkerMsg::Reannounce);
                    }
                    Ok(Vec::new())
                }
                Some(Command::Clipboard(message)) => Ok(clipboard_message(&mut active_stage, message)),
            },
            () = sleep_until(flush_at), if can_flush => {
                screen.flush(out, graphics.as_ref());
                continue;
            }
            () = sleep_until(resize_at), if pending_resize.is_some() => {
                match pending_resize.take() {
                    Some((w, h, scale)) => Ok(resize(&mut active_stage, &screen, w, h, scale)),
                    None => Ok(Vec::new()),
                }
            }
            () = sleep_until(close_deadline.unwrap_or(far)), if close_deadline.is_some() => {
                return Ending::new(CloseReason::Disconnect, "Disconnected.");
            }
        };

        let outputs = match outputs {
            Ok(outputs) => outputs,
            Err(e) => {
                return Ending::new(
                    CloseReason::Error,
                    format!("Protocol error: {}", e.report()),
                )
            }
        };

        for output in outputs {
            match output {
                ActiveStageOutput::ResponseFrame(frame) => {
                    if frame.is_empty() {
                        continue;
                    }
                    if let Err(e) = writer.write_all(&frame).await {
                        return Ending::new(
                            CloseReason::Error,
                            format!("The connection was lost: {e}"),
                        );
                    }
                }
                ActiveStageOutput::GraphicsUpdate(region) => screen.mark_dirty(&region),
                ActiveStageOutput::PointerDefault => out.send(&frame::pointer_default()),
                ActiveStageOutput::PointerHidden => out.send(&frame::pointer_hidden()),
                ActiveStageOutput::PointerPosition { x, y } => {
                    out.send(&frame::pointer_position(x, y))
                }
                ActiveStageOutput::PointerBitmap(pointer) => {
                    match frame::pointer_bitmap(
                        pointer.hotspot_x,
                        pointer.hotspot_y,
                        pointer.width,
                        pointer.height,
                        &pointer.bitmap_data,
                    ) {
                        Some(message) => out.send(&message),
                        None => debug!("dropping a malformed pointer bitmap"),
                    }
                }
                ActiveStageOutput::Terminate(reason) => {
                    return ending_for(&reason, close_deadline.is_some())
                }
                ActiveStageOutput::DeactivateAll => {
                    // Deactivation-Reactivation Sequence (MS-RDPBCGR 1.3.1.3):
                    // what a resize through DisplayControl ends in.
                    debug!("deactivation-reactivation");
                    let mut activation = activation_factory.create();
                    let mut buf = WriteBuf::new();
                    loop {
                        let written = match ironrdp_tokio::single_sequence_step_read(
                            &mut reader,
                            &mut activation,
                            &mut buf,
                        )
                        .await
                        {
                            Ok(written) => written,
                            Err(e) => {
                                return Ending::new(
                                    CloseReason::Error,
                                    format!("Reactivation failed: {}", e.report()),
                                )
                            }
                        };
                        if written.size().is_some() {
                            if let Err(e) = writer.write_all(buf.filled()).await {
                                return Ending::new(
                                    CloseReason::Error,
                                    format!("The connection was lost: {e}"),
                                );
                            }
                        }
                        if let ConnectionActivationState::Finalized {
                            desktop_size,
                            share_id,
                            enable_server_pointer,
                            pointer_software_rendering,
                        } = activation.connection_activation_state()
                        {
                            debug!(?desktop_size, "reactivated");
                            active_stage.set_fastpath_processor(
                                fast_path::ProcessorBuilder {
                                    io_channel_id: activation.io_channel_id(),
                                    user_channel_id: activation.user_channel_id(),
                                    share_id,
                                    enable_server_pointer,
                                    pointer_software_rendering,
                                    // We never negotiate bulk compression.
                                    bulk_decompressor: None,
                                }
                                .build(),
                            );
                            active_stage.set_share_id(share_id);
                            active_stage.set_enable_server_pointer(enable_server_pointer);

                            // The page must see the new size before any
                            // pixels of it; everything old is void.
                            screen = Screen {
                                unacked: screen.unacked,
                                ..Screen::new(desktop_size.width, desktop_size.height)
                            };
                            input.set_desktop_size(desktop_size.width, desktop_size.height);
                            out.send(&frame::desktop_size(
                                desktop_size.width,
                                desktop_size.height,
                            ));
                            break;
                        }
                    }
                }
                ActiveStageOutput::MultitransportRequest(_) => {
                    debug!("ignoring a multitransport (UDP) request");
                }
                ActiveStageOutput::AutoDetect(request) => {
                    trace!(?request, "auto-detect");
                }
            }
        }

        if let Some(graphics) = &graphics {
            let changes = graphics.lock().take_changes();
            if let Some((width, height)) = changes.resized {
                if (width, height) == (screen.image.width(), screen.image.height()) {
                    // The size the page already has: redraw, but don't make
                    // it start over (that would flash black).
                    screen.mark_all_dirty();
                } else {
                    // The same as a reactivation: new size first, old pixels void.
                    screen = Screen {
                        unacked: screen.unacked,
                        ..Screen::new(width, height)
                    };
                    input.set_desktop_size(width, height);
                    out.send(&frame::desktop_size(width, height));
                }
            }
            for rect in changes.dirty {
                screen.dirty.add(rect);
            }
        }
    }
}

/// The write half of the session's stream.
type Writer = ironrdp_tokio::TokioFramed<tokio::io::WriteHalf<Stream>>;

async fn write_outputs(
    writer: &mut Writer,
    outputs: SessionResult<Vec<ActiveStageOutput>>,
) -> Option<Ending> {
    match outputs {
        Ok(outputs) => {
            for output in outputs {
                if let ActiveStageOutput::ResponseFrame(frame) = output {
                    if let Err(e) = writer.write_all(&frame).await {
                        return Some(Ending::new(
                            CloseReason::Error,
                            format!("The connection was lost: {e}"),
                        ));
                    }
                }
            }
            None
        }
        Err(e) => Some(Ending::new(
            CloseReason::Error,
            format!("Protocol error: {}", e.report()),
        )),
    }
}

fn close_request(
    active_stage: &ActiveStage,
    close_deadline: &mut Option<Instant>,
) -> SessionResult<Vec<ActiveStageOutput>> {
    if close_deadline.is_some() {
        return Ok(Vec::new());
    }
    *close_deadline = Some(Instant::now() + CLOSE_TIMEOUT);
    active_stage.graceful_shutdown()
}

fn resize(
    active_stage: &mut ActiveStage,
    screen: &Screen,
    width: u32,
    height: u32,
    scale_factor: u32,
) -> Vec<ActiveStageOutput> {
    if u32::from(screen.image.width()) == width && u32::from(screen.image.height()) == height {
        trace!("resize to the current size, skipped");
        return Vec::new();
    }
    let scale = (100..=500).contains(&scale_factor).then_some(scale_factor);
    match active_stage.encode_resize(width, height, scale, None) {
        Some(Ok(frame)) => {
            debug!(width, height, ?scale, "requested a resize");
            vec![ActiveStageOutput::ResponseFrame(frame)]
        }
        Some(Err(e)) => {
            warn!(error = %e.report(), "resize could not be encoded");
            Vec::new()
        }
        // The server does not offer DisplayControl: the page scales instead.
        None => {
            debug!("the server does not support dynamic resize");
            Vec::new()
        }
    }
}

/// Carries a clipboard message to CLIPRDR. Failures are logged, never fatal.
fn clipboard_message(
    active_stage: &mut ActiveStage,
    message: ClipboardMessage,
) -> Vec<ActiveStageOutput> {
    let Some(cliprdr) = active_stage.get_svc_processor_mut::<CliprdrClient>() else {
        debug!("clipboard message without a clipboard channel");
        return Vec::new();
    };
    let messages: Result<SvcProcessorMessages<CliprdrClient>, _> = match message {
        ClipboardMessage::SendInitiateCopy(formats) => cliprdr.initiate_copy(&formats),
        ClipboardMessage::SendFormatData(response) => cliprdr.submit_format_data(response),
        ClipboardMessage::SendInitiatePaste(format) => cliprdr.initiate_paste(format),
        ClipboardMessage::Error(e) => {
            warn!(error = %e, "clipboard error");
            return Vec::new();
        }
        // File transfer is not offered, so nothing ever produces these.
        other => {
            debug!(?other, "ignoring an unsupported clipboard message");
            return Vec::new();
        }
    };
    let messages = match messages {
        Ok(messages) => messages,
        Err(e) => {
            warn!(error = %e, "clipboard message could not be encoded");
            return Vec::new();
        }
    };
    match active_stage.process_svc_processor_messages(messages) {
        Ok(frame) => vec![ActiveStageOutput::ResponseFrame(frame)],
        Err(e) => {
            warn!(error = %e.report(), "clipboard message could not be sent");
            Vec::new()
        }
    }
}

/// Protocol-independent codes that mean "logged off".
const LOGOFF_CODES: [ProtocolIndependentCode; 2] = [
    ProtocolIndependentCode::LogoffByUser,
    ProtocolIndependentCode::RpcInitiatedLogoff,
];

/// Codes that mean "disconnected, the session lives on".
const DISCONNECT_CODES: [ProtocolIndependentCode; 3] = [
    ProtocolIndependentCode::RpcInitiatedDisconnect,
    ProtocolIndependentCode::RpcInitiatedDisconnectByuser,
    ProtocolIndependentCode::DisconnectedByOtherconnection,
];

fn matches_code(description: &str, codes: &[ProtocolIndependentCode]) -> bool {
    codes
        .iter()
        .any(|c| ErrorInfo::ProtocolIndependentCode(*c).description() == description)
}

/// Drops IronRDP's "[Protocol independent error] " style prefix.
fn plain(description: &str) -> String {
    let text = match description.strip_prefix('[') {
        Some(rest) => rest
            .split_once(']')
            .map(|(_, t)| t.trim_start_matches(':').trim())
            .unwrap_or(description),
        None => description,
    };
    let mut text = text.trim().to_owned();
    if !text.is_empty() && !text.ends_with('.') {
        text.push('.');
    }
    text
}

fn ending_for(reason: &GracefulDisconnectReason, we_asked: bool) -> Ending {
    match reason {
        GracefulDisconnectReason::UserInitiated if we_asked => {
            Ending::new(CloseReason::Disconnect, "Disconnected.")
        }
        GracefulDisconnectReason::UserInitiated => Ending::new(
            CloseReason::Disconnect,
            "The remote session was disconnected.",
        ),
        GracefulDisconnectReason::ServerInitiated => {
            Ending::new(CloseReason::Server, "The server ended the connection.")
        }
        GracefulDisconnectReason::Other(description) => {
            if matches_code(description, &LOGOFF_CODES) {
                Ending::new(CloseReason::Logoff, "The remote session was logged off.")
            } else if matches_code(description, &DISCONNECT_CODES) {
                Ending::new(CloseReason::Disconnect, plain(description))
            } else {
                Ending::new(CloseReason::Server, plain(description))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[test]
    fn logoff_is_recognized() {
        let description =
            ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::LogoffByUser).description();
        let ending = ending_for(&GracefulDisconnectReason::Other(description), false);
        assert_eq!(ending.reason, CloseReason::Logoff);
    }

    #[test]
    fn admin_disconnect_is_a_disconnect_in_plain_english() {
        let description = ErrorInfo::ProtocolIndependentCode(
            ProtocolIndependentCode::DisconnectedByOtherconnection,
        )
        .description();
        let ending = ending_for(&GracefulDisconnectReason::Other(description), false);
        assert_eq!(ending.reason, CloseReason::Disconnect);
        assert!(!ending.message.starts_with('['), "{}", ending.message);
        assert!(ending.message.starts_with("Another user connected"));
    }

    #[test]
    fn other_server_reasons_are_server() {
        let description =
            ErrorInfo::ProtocolIndependentCode(ProtocolIndependentCode::IdleTimeout).description();
        let ending = ending_for(&GracefulDisconnectReason::Other(description), false);
        assert_eq!(ending.reason, CloseReason::Server);
        assert_eq!(
            ending.message,
            "The idle session limit timer on the server has elapsed."
        );
        assert_eq!(
            ending_for(&GracefulDisconnectReason::ServerInitiated, false).reason,
            CloseReason::Server
        );
    }

    #[test]
    fn our_own_close_is_a_disconnect() {
        let ending = ending_for(&GracefulDisconnectReason::UserInitiated, true);
        assert_eq!(ending.reason, CloseReason::Disconnect);
    }

    #[derive(Default)]
    struct Collect(Mutex<Vec<Vec<u8>>>);

    impl FrameSink for Collect {
        fn send(&self, frame: &[u8]) -> Result<(), SinkError> {
            self.0.lock().push(frame.to_vec());
            Ok(())
        }
        fn finish(&self) {}
    }

    #[test]
    fn flush_respects_the_ack_window_and_keeps_the_rest_dirty() {
        let sink = Collect::default();
        let out = Output {
            sink: &sink,
            gone: AtomicBool::new(false),
        };
        let mut screen = Screen::new(64, 64);
        screen.flush(&out, None);
        assert_eq!(screen.unacked, 1);
        assert!(screen.dirty.is_empty());
        assert_eq!(sink.0.lock().len(), 1);
        assert_eq!(sink.0.lock()[0][0], frame::KIND_BITMAPS);

        screen.dirty.add(Rect::new(0, 0, 8, 8));
        assert!(screen.can_flush());
        screen.flush(&out, None);
        assert_eq!(screen.unacked, 2);

        // Window full: updates pile up but are not sent.
        screen.dirty.add(Rect::new(10, 10, 4, 4));
        assert!(!screen.can_flush());
        screen.unacked -= 1; // one ack
        assert!(screen.can_flush());
    }

    #[test]
    fn inclusive_rectangles_are_converted_and_clipped() {
        let mut screen = Screen::new(100, 100);
        screen.dirty.clear();
        screen.mark_dirty(&InclusiveRectangle {
            left: 90,
            top: 0,
            right: 120,
            bottom: 9,
        });
        assert_eq!(screen.dirty.rects(), &[Rect::new(90, 0, 10, 10)]);
    }
}
