#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
//! SSE line framing: [`SseEvent`] composes and parses the `event:`/`data:`/`id:`
//! text form. The expected text and the expected parse are the SSE definition's,
//! not the codec's own output. The `id:` is where the connector puts the frontier
//! token, so `Last-Event-ID` resume falls out of this framing.

use liasse_wire::SseEvent;

#[test]
fn a_frontier_stamped_data_frame_encodes_to_the_expected_lines() {
    let event = SseEvent::data(r#"{"type":"frontier"}"#).with_id("42").with_event("frontier");
    assert_eq!(event.encode(), "id: 42\nevent: frontier\ndata: {\"type\":\"frontier\"}\n\n");
}

#[test]
fn multiline_data_becomes_one_data_line_each_and_round_trips() {
    let event = SseEvent::data("line one\nline two").with_id("7");
    assert_eq!(event.encode(), "id: 7\ndata: line one\ndata: line two\n\n");
    let parsed = SseEvent::parse_stream(&event.encode());
    assert_eq!(parsed, vec![event]);
}

#[test]
fn a_stream_of_events_round_trips() {
    let events = vec![
        SseEvent::data(r#"{"type":"init","sub":"s","rows":[]}"#).with_id("1").with_event("init"),
        SseEvent::data(r#"{"type":"patch","sub":"s","ops":[]}"#).with_id("2").with_event("patch"),
        SseEvent { event: None, id: Some("3".into()), data: "{}".into(), retry: Some(1500) },
    ];
    let text = SseEvent::encode_stream(&events);
    assert_eq!(SseEvent::parse_stream(&text), events);
}

#[test]
fn parsing_tolerates_comments_crlf_and_ignores_data_less_groups() {
    // Two events, CRLF line endings, a comment (keep-alive) line, and a trailing
    // group that carries only an id (no data) which the SSE definition drops.
    let text = "\
: keep-alive\r\n\
id: a\r\n\
data: first\r\n\
\r\n\
data: second\r\n\
\r\n\
id: no-data\r\n\
\r\n";
    let events = SseEvent::parse_stream(text);
    assert_eq!(events.len(), 2, "the id-only group dispatches nothing");
    assert_eq!(events[0], SseEvent { event: None, id: Some("a".into()), data: "first".into(), retry: None });
    assert_eq!(events[1].data, "second");
}

#[test]
fn the_leading_space_after_the_colon_is_stripped_once() {
    // SSE strips exactly one optional leading space; a second space is data.
    let events = SseEvent::parse_stream("data:  padded\n\n");
    assert_eq!(events[0].data, " padded");
}
