//! Turning the guest's virtio-console byte stream into something a page can show.
//!
//! Two problems sit between `Machine::virtio_tx_output()` and a readable span list,
//! and both are this module's job:
//!
//! - **Frames straddle drain boundaries.** The page drains whatever bytes exist at
//!   the end of an animation frame, which is an arbitrary instant with no relation to
//!   the guest's framing. A frame *will* be cut in half. The tail has to be kept and
//!   joined to the next drain, or the page silently loses telemetry — and loses it
//!   more often the smoother the frame rate.
//! - **Names arrive separately from their uses.** The wire interns strings: a
//!   `StringRegister` establishes `StringId -> "kernel.boot"`, and every later
//!   `SpanStart` cites the id. A page that rendered ids would be unreadable, so the
//!   table is accumulated here and names resolved on the way out.
//!
//! `protocol::stream::try_decode_frame` does the actual decoding, and its doc calls
//! out this exact use — "a caller holding a *growing in-memory* buffer … advancing an
//! offset by the returned count". This module is that caller.

use protocol::stream::{OwnedFrame, try_decode_frame};
use protocol::StringId;
use std::collections::HashMap;

/// One decoded frame, flattened into the handful of fields a page renders.
///
/// Deliberately lossy. The wire has twenty-odd variants with bespoke payloads, and
/// projecting each into its own JS shape would be a lot of surface for a boot-log
/// page to consume. What every view needs is: what kind of thing happened, what it
/// was called, and when. Anything richer is a later milestone's problem, and
/// `OwnedFrame` is still there for it.
///
/// `Serialize` is the page's view of it — see [`Status`](crate::budget::Status) for
/// why the shell serializes rather than converts by hand. `None` fields become JSON
/// `null`, which is what lets the page distinguish "no name" from a name it should
/// render.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FrameView {
    /// The variant name, e.g. `"SpanStart"`. A contract with the page, not an
    /// internal detail — it is what JS branches on.
    pub kind: &'static str,
    /// The interned name, resolved through the table. `None` when the frame has no
    /// name, or when its `StringId` has not been registered yet — never a
    /// stand-in string, so the page can tell "unnamed" from "not yet known".
    pub name: Option<String>,
    /// Guest timestamp, for frames that carry one.
    pub t: Option<u64>,
    /// A `Metric`'s value.
    pub value: Option<i64>,
}

impl FrameView {
    /// Project `frame`, resolving any interned name through `lookup`.
    ///
    /// Takes a closure rather than the table itself so this stays a pure function of
    /// its inputs — which is what lets it be tested without building a `Decoder`.
    #[must_use]
    pub fn of(frame: &OwnedFrame, lookup: &dyn Fn(StringId) -> Option<String>) -> Self {
        let (kind, name_id, t, value) = match *frame {
            OwnedFrame::Hello { .. } => ("Hello", None, None, None),
            OwnedFrame::SpanStart { name_id, t, .. } => ("SpanStart", Some(name_id), Some(t), None),
            OwnedFrame::SpanEnd { t, .. } => ("SpanEnd", None, Some(t), None),
            OwnedFrame::Event { name_id, t, .. } => ("Event", Some(name_id), Some(t), None),
            OwnedFrame::Metric { name_id, value, t, .. } => {
                ("Metric", Some(name_id), Some(t), Some(value))
            }
            OwnedFrame::Dropped { count } => ("Dropped", None, None, Some(i64::from(count))),
            OwnedFrame::StringRegister { id, .. } => ("StringRegister", Some(id), None, None),
            OwnedFrame::MetricRegister { name_id, .. } => {
                ("MetricRegister", Some(name_id), None, None)
            }
            OwnedFrame::ThreadRegister { .. } => ("ThreadRegister", None, None, None),
            OwnedFrame::ContextSwitch { t, .. } => ("ContextSwitch", None, Some(t), None),
            OwnedFrame::HartRegister { .. } => ("HartRegister", None, None, None),
            OwnedFrame::CapEvent { t, .. } => ("CapEvent", None, Some(t), None),
            OwnedFrame::SyscallRefused { t, .. } => ("SyscallRefused", None, Some(t), None),
            OwnedFrame::Log { t, .. } => ("Log", None, Some(t), None),
            OwnedFrame::Message { t, .. } => ("Message", None, Some(t), None),
            OwnedFrame::NotifySignal { t, .. } => ("NotifySignal", None, Some(t), None),
            OwnedFrame::NotifyWait { t, .. } => ("NotifyWait", None, Some(t), None),
            OwnedFrame::AudioXRun { t, .. } => ("AudioXRun", None, Some(t), None),
            OwnedFrame::BuildInfo { .. } => ("BuildInfo", None, None, None),
        };

        // A `ThreadRegister`/`Log`/`BuildInfo` carries its own text rather than an
        // interned id; those stay `None` here and the page reads `OwnedFrame` if it
        // wants them.
        let name = match *frame {
            OwnedFrame::StringRegister { ref value, .. } => Some(value.clone()),
            _ => name_id.and_then(lookup),
        };

        Self { kind, name, t, value }
    }
}

/// Accumulates virtio-console bytes and hands back whole frames.
///
/// Owns the two pieces of state a drain-based consumer cannot do without: the
/// incomplete tail of the last push, and the intern table built from every
/// `StringRegister` seen so far.
/// How many *stream* frames a live tab keeps for folding.
///
/// A comfort knob, not a correctness one — the frames whose loss would make a view
/// untrue are held durably regardless (see [`crate::store`]). This bounds the span
/// and switch views to a recent window, which is what you want to look at anyway, and
/// bounds the cost of re-folding, which is paid per render.
pub const FRAME_WINDOW: usize = 4_000;

#[derive(Debug)]
pub struct Decoder {
    /// Bytes received but not yet forming a whole frame. Bounded by one frame:
    /// everything complete is consumed and dropped on each push.
    pending: Vec<u8>,
    strings: HashMap<StringId, String>,
    /// Decoded frames, kept for the folds that build the panels.
    store: crate::store::FrameStore,
    /// Metric samples, kept per metric — see `crate::series` for why they cannot
    /// share the frame window.
    series: crate::series::SeriesStore,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            strings: HashMap::new(),
            store: crate::store::FrameStore::new(FRAME_WINDOW),
            series: crate::series::SeriesStore::new(),
        }
    }

    /// Every metric's history, in first-seen order.
    #[must_use]
    pub fn series(&self) -> Vec<crate::series::Series> {
        self.series.series()
    }

    /// The frames retained so far, in arrival order, ready to fold.
    ///
    /// Whole `OwnedFrame`s rather than [`FrameView`]s: the projection is deliberately
    /// lossy — kind, name, timestamp, value — and a derivation tree needs `cap_id`,
    /// `parent_cap_id`, `holder` and `rights`. The two views of a frame serve
    /// different consumers and neither subsumes the other.
    #[must_use]
    pub fn frames(&self) -> Vec<OwnedFrame> {
        self.store.frames()
    }

    /// How many cumulative facts are being held — unbounded by design, so worth
    /// being able to watch.
    #[must_use]
    pub fn durable_len(&self) -> usize {
        self.store.durable_len()
    }

    /// How many bytes are held awaiting completion. Diagnostic — it exists so a test
    /// can assert the buffer stays bounded rather than accumulating the whole boot.
    #[must_use]
    pub fn held(&self) -> usize {
        self.pending.len()
    }

    /// Feed freshly drained bytes; get back every frame that is now complete.
    ///
    /// Frames come back in wire order. An incomplete tail is retained for the next
    /// call. A chunk that fails to decode is skipped and the stream resyncs at the
    /// next `0x00` — free with COBS framing, since the delimiter is already where
    /// the following frame begins.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<FrameView> {
        self.pending.extend_from_slice(bytes);

        let mut views = Vec::new();
        let mut offset = 0;
        // Bounded by construction. Every iteration consumes at least one byte — a
        // decoded frame includes its `0x00` terminator, and a rejected chunk skips
        // through one — so `pending.len()` iterations is an upper bound a correct run
        // never reaches. Stated as a `for` rather than a `loop` for the same reason
        // `budget::run` is: this runs inside an animation frame, where a spin is not
        // an interruptible hang but a tab you have to kill. Mutation testing found
        // this exact hazard (`offset += n` → `*=` pins offset at zero, forever).
        let bound = self.pending.len();
        for _ in 0..bound {
            match try_decode_frame(&self.pending[offset..]) {
                Ok(Some((frame, consumed))) => {
                    offset += consumed;
                    // Register before projecting, so a `StringRegister` and a frame
                    // citing it in the same push resolve correctly.
                    if let OwnedFrame::StringRegister { id, ref value } = frame {
                        self.strings.insert(id, value.clone());
                    }
                    let strings = &self.strings;
                    views.push(FrameView::of(&frame, &|id| strings.get(&id).cloned()));
                    self.series.observe(&frame, &|id| strings.get(&id).cloned());
                    // Retained *after* projecting, so the borrow of `strings` above
                    // is done with before the frame moves into the store.
                    self.store.push(frame);
                }
                // No terminator yet — the rest is an incomplete frame.
                Ok(None) => break,
                // Undecodable chunk: skip past its terminator and carry on.
                Err(e) => offset += e.consumed,
            }
        }

        self.pending.drain(..offset);
        views
    }
}

#[cfg(test)]
mod tests {
    use super::{Decoder, FrameView};
    use protocol::{Frame, SpanId, StringId, wire_encode};

    /// Encode frames the way the kernel does, so tests run against real wire bytes
    /// rather than hand-rolled fixtures that could encode a fiction.
    fn wire(frames: &[Frame<'_>]) -> Vec<u8> {
        let mut out = Vec::new();
        for f in frames {
            let mut buf = [0u8; 512];
            out.extend_from_slice(wire_encode(f, &mut buf).expect("encodes"));
        }
        out
    }

    fn register(id: u32, value: &str) -> Frame<'_> {
        Frame::StringRegister { id: StringId(id), value }
    }

    fn span_start(id: u64, name_id: u32) -> Frame<'static> {
        Frame::SpanStart {
            id: SpanId(id),
            parent: SpanId(0),
            name_id: StringId(name_id),
            t: 42,
            task_id: 0,
            hart_id: 0,
        }
    }

    #[test]
    fn a_whole_frame_decodes_in_one_push() {
        let mut d = Decoder::new();
        let views = d.push(&wire(&[register(1, "kernel.boot")]));
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].kind, "StringRegister");
    }

    /// The property the whole module exists for. A frame cut at an arbitrary byte
    /// must survive, and must be delivered exactly once.
    #[test]
    fn a_frame_split_across_two_pushes_decodes_once_when_whole() {
        let bytes = wire(&[register(1, "kernel.boot")]);
        let cut = bytes.len() / 2;

        let mut d = Decoder::new();
        assert!(d.push(&bytes[..cut]).is_empty(), "half a frame yields nothing yet");
        let views = d.push(&bytes[cut..]);
        assert_eq!(views.len(), 1, "the completed frame arrives on the second push");
        assert_eq!(views[0].name.as_deref(), Some("kernel.boot"));
    }

    /// Not just one boundary — every boundary. A byte-at-a-time feed is the extreme
    /// case of the same hazard, and it must lose nothing.
    #[test]
    fn feeding_one_byte_at_a_time_loses_no_frames() {
        let bytes = wire(&[register(1, "a.name"), span_start(7, 1), register(2, "b.name")]);

        let mut d = Decoder::new();
        let mut seen = Vec::new();
        for b in &bytes {
            seen.extend(d.push(&[*b]));
        }
        assert_eq!(seen.len(), 3, "all three frames survived a byte-wise feed");
    }

    /// The interning contract: a `SpanStart` cites a `StringId`, and the page must
    /// see the name the earlier `StringRegister` bound to it.
    #[test]
    fn a_span_start_resolves_its_name_through_an_earlier_string_register() {
        let mut d = Decoder::new();
        let views = d.push(&wire(&[register(3, "kernel.heartbeat"), span_start(9, 3)]));

        let span = views.iter().find(|v| v.kind == "SpanStart").expect("a SpanStart");
        assert_eq!(span.name.as_deref(), Some("kernel.heartbeat"));
    }

    /// The table must span pushes too — the register and its use will routinely land
    /// in different animation frames.
    #[test]
    fn the_intern_table_persists_across_pushes() {
        let mut d = Decoder::new();
        d.push(&wire(&[register(4, "task_a.tick")]));
        let views = d.push(&wire(&[span_start(1, 4)]));

        assert_eq!(views[0].name.as_deref(), Some("task_a.tick"));
    }

    /// A name that has not been registered yet must not masquerade as a resolved
    /// one. Rendering an unresolved id as a name would be a lie the page cannot
    /// detect; `None` lets it show the id instead.
    #[test]
    fn an_unregistered_name_id_resolves_to_nothing_rather_than_a_wrong_name() {
        let mut d = Decoder::new();
        let views = d.push(&wire(&[span_start(1, 99)]));

        assert_eq!(views[0].name, None);
    }

    /// A push carrying nothing new is not a special case — the page will make many
    /// of them, one per idle animation frame.
    #[test]
    fn an_empty_push_yields_nothing_and_disturbs_nothing() {
        let mut d = Decoder::new();
        d.push(&wire(&[register(1, "kernel.boot")]));
        assert!(d.push(&[]).is_empty());
        let views = d.push(&wire(&[span_start(1, 1)]));
        assert_eq!(views[0].name.as_deref(), Some("kernel.boot"), "table intact");
    }

    /// Several frames in one push all come back, in wire order. Order is the whole
    /// point of a trace view.
    #[test]
    fn several_frames_in_one_push_come_back_in_wire_order() {
        let mut d = Decoder::new();
        let views = d.push(&wire(&[register(1, "first"), register(2, "second")]));

        let names: Vec<_> = views.iter().filter_map(|v| v.name.as_deref()).collect();
        assert_eq!(names, ["first", "second"]);
    }

    /// Corrupt bytes must not wedge the stream. COBS framing makes resync free — the
    /// `0x00` delimiter is already where the next frame starts — so a bad chunk
    /// costs one frame, not the rest of the boot.
    #[test]
    fn a_corrupt_frame_is_skipped_and_the_stream_recovers() {
        let mut bytes = wire(&[register(1, "before")]);
        bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0x00]); // garbage + terminator
        bytes.extend_from_slice(&wire(&[register(2, "after")]));

        let mut d = Decoder::new();
        let names: Vec<_> =
            d.push(&bytes).iter().filter_map(|v| v.name.clone()).collect();
        assert_eq!(names, ["before", "after"], "the frame after the corruption survived");
    }

    /// The buffer must not grow without bound across a long boot — it holds only the
    /// incomplete tail, never the whole history.
    #[test]
    fn the_held_buffer_keeps_only_the_incomplete_tail() {
        let bytes = wire(&[register(1, "kernel.boot")]);
        let mut d = Decoder::new();
        d.push(&bytes);
        assert_eq!(d.held(), 0, "a clean boundary holds nothing");

        d.push(&bytes[..3]);
        assert_eq!(d.held(), 3, "and an incomplete frame holds exactly its bytes");
    }

    /// Decoded frames are kept for the folds, not only projected and dropped.
    ///
    /// The panels reconstruct their views from a *slice* of frames, so a decoder that
    /// projected and discarded would leave them nothing to work with.
    #[test]
    fn decoded_frames_are_retained_for_folding() {
        let mut d = Decoder::new();
        d.push(&wire(&[register(1, "kernel.boot"), span_start(7, 1)]));

        assert_eq!(d.frames().len(), 2, "both frames kept");
    }

    /// Retention spans pushes, like the intern table — a registration and the frame
    /// that needs it routinely arrive in different animation frames.
    #[test]
    fn retention_spans_pushes() {
        let mut d = Decoder::new();
        d.push(&wire(&[register(1, "a")]));
        d.push(&wire(&[span_start(1, 1)]));

        assert_eq!(d.frames().len(), 2);
    }

    /// Metric samples reach the series store as the decoder decodes them.
    ///
    /// Tested *through the decoder* rather than only on `SeriesStore`: the wiring
    /// between them is its own claim, and a pass-through accessor that returned
    /// nothing would leave every chart empty while the store beneath it was perfectly
    /// correct. Mutation testing has now found this same shape three times on this
    /// type — a delegating method is not covered by its delegate's tests.
    #[test]
    fn decoded_metrics_reach_the_series_store() {
        let mut d = Decoder::new();
        assert!(d.series().is_empty());

        d.push(&wire(&[
            register(5, "snitchos.heap.bytes_used"),
            Frame::Metric { name_id: StringId(5), value: 4096, t: 100, hart_id: 0 },
            Frame::Metric { name_id: StringId(5), value: 8192, t: 200, hart_id: 0 },
        ]));

        let series = d.series();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].name, "snitchos.heap.bytes_used");
        assert_eq!(series[0].points, vec![(100, 4096), (200, 8192)]);
    }

    /// The unbounded bucket is reportable through the decoder, not only the store.
    ///
    /// It is what the page displays so the "registrations are naturally few"
    /// assumption can be watched rather than trusted — so it has to move with what
    /// arrives. Counts chosen to differ from each other and from one: a constant
    /// would satisfy a single equality, which is how the store's own version of this
    /// accessor first shipped untested.
    #[test]
    fn the_decoder_reports_its_cumulative_frames() {
        let mut d = Decoder::new();
        assert_eq!(d.durable_len(), 0);

        d.push(&wire(&[register(1, "a"), register(2, "b"), register(3, "c")]));
        assert_eq!(d.durable_len(), 3);

        // A stream frame is windowed, so it must not move the durable count.
        d.push(&wire(&[span_start(1, 1)]));
        assert_eq!(d.durable_len(), 3, "a span is not a cumulative fact");
    }

    /// A corrupt chunk is skipped rather than retained: a frame that failed to decode
    /// is not a frame, and folding it is not possible.
    #[test]
    fn a_chunk_that_failed_to_decode_is_not_retained() {
        let mut bytes = wire(&[register(1, "before")]);
        bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD, 0x00]);
        bytes.extend_from_slice(&wire(&[register(2, "after")]));

        let mut d = Decoder::new();
        d.push(&bytes);

        assert_eq!(d.frames().len(), 2, "the two real frames, not the garbage");
    }

    /// A metric carries a value the page wants to show, and its name interns like
    /// any other.
    #[test]
    fn a_metric_resolves_its_name_and_keeps_its_value() {
        let metric = Frame::Metric { name_id: StringId(5), value: 1234, t: 7, hart_id: 0 };
        let mut d = Decoder::new();
        let views = d.push(&wire(&[register(5, "snitchos.frames.allocated_total"), metric]));

        let m = views.iter().find(|v| v.kind == "Metric").expect("a Metric");
        assert_eq!(m.name.as_deref(), Some("snitchos.frames.allocated_total"));
        assert_eq!(m.value, Some(1234));
    }

    /// The JSON shape the page consumes, pinned for the same reason `Status`'s is.
    /// Note `null` rather than an omitted key for an unresolved name: the page can
    /// then tell "not yet known" from "not applicable" without guessing.
    #[test]
    fn the_serialized_shape_is_what_the_page_renders() {
        let view = FrameView {
            kind: "Metric",
            name: Some("snitchos.heap.bytes_used".into()),
            t: Some(9),
            value: Some(-2),
        };
        assert_eq!(
            serde_json::to_string(&view).expect("serializes"),
            r#"{"kind":"Metric","name":"snitchos.heap.bytes_used","t":9,"value":-2}"#
        );

        let bare = FrameView { kind: "SpanEnd", name: None, t: None, value: None };
        assert_eq!(
            serde_json::to_string(&bare).expect("serializes"),
            r#"{"kind":"SpanEnd","name":null,"t":null,"value":null}"#
        );
    }

    /// `FrameView` is what crosses into JS, so its kind label is a contract with the
    /// page, not an internal detail.
    #[test]
    fn every_view_carries_a_kind_label() {
        let view = FrameView::of(&protocol::stream::OwnedFrame::Dropped { count: 3 }, &|_| None);
        assert_eq!(view.kind, "Dropped");
    }

    /// A `StringRegister` names itself: its value is right there in the frame, so
    /// projecting one must not depend on the caller having already put it in a table.
    ///
    /// Inside `Decoder::push` this is masked — the table is populated before the
    /// frame is projected, so a lookup would find it either way. Mutation testing
    /// caught that masking (deleting the special-case arm changed nothing), which is
    /// the tell that `FrameView::of`'s own contract was untested. Here the lookup is
    /// deliberately empty.
    #[test]
    fn a_string_register_projects_its_own_value_without_any_table() {
        let frame = protocol::stream::OwnedFrame::StringRegister {
            id: StringId(1),
            value: "kernel.boot".into(),
        };
        let view = FrameView::of(&frame, &|_| None);
        assert_eq!(view.name.as_deref(), Some("kernel.boot"));
    }

    /// Trailing bytes that are not yet a frame must be held, not mistaken for one,
    /// even when they follow perfectly good frames in the same push. This is the
    /// realistic drain shape: complete frames, then a severed tail.
    #[test]
    fn a_push_ending_mid_frame_yields_the_whole_ones_and_holds_the_rest() {
        let whole = wire(&[register(1, "first"), register(2, "second")]);
        let tail = wire(&[register(3, "third")]);
        let cut = tail.len() / 2;

        let mut d = Decoder::new();
        let mut bytes = whole.clone();
        bytes.extend_from_slice(&tail[..cut]);

        let views = d.push(&bytes);
        assert_eq!(views.len(), 2, "both complete frames delivered");
        assert_eq!(d.held(), cut, "and only the severed tail retained");

        let views = d.push(&tail[cut..]);
        assert_eq!(views.len(), 1, "the third arrives once completed");
        assert_eq!(views[0].name.as_deref(), Some("third"));
    }
}
