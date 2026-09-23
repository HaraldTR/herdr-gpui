use super::*;
use crate::frame::ImageWriter;

#[test]
fn read_batches_bound_progress_and_stop_on_the_first_idle_read() {
    struct Input {
        bytes: io::Cursor<Vec<u8>>,
        calls: usize,
        idle: Option<io::ErrorKind>,
    }
    impl Read for Input {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            if let Some(kind) = self.idle {
                return Err(kind.into());
            }
            self.bytes.read(bytes)
        }
    }
    let expected = ServerMessage::Graphics {
        bytes: vec![17; 2 * 1024 * 1024],
    };
    let mut input = Input {
        bytes: io::Cursor::new(encode_message(&expected, MAX_GRAPHICS_FRAME_SIZE).unwrap()),
        calls: 0,
        idle: None,
    };
    let mut reader = FrameReader::new();
    assert!(reader.poll_batch(&mut input).unwrap().is_none());
    assert!((1..=128).contains(&input.calls));
    assert!(reader.bytes.len() <= 1024 * 1024);
    let partial = reader.bytes.len();
    for kind in [
        io::ErrorKind::WouldBlock,
        io::ErrorKind::TimedOut,
        io::ErrorKind::Interrupted,
    ] {
        input.idle = Some(kind);
        input.calls = 0;
        assert!(reader.poll_batch(&mut input).unwrap().is_none());
        assert_eq!(input.calls, 1);
        assert_eq!(reader.bytes.len(), partial);
    }
    input.idle = None;
    loop {
        input.calls = 0;
        let message = reader.poll_batch(&mut input).unwrap();
        assert!(input.calls <= 128);
        if let Some(message) = message {
            assert_eq!(message, expected);
            break;
        }
    }
}

#[test]
fn image_writer_preserves_offsets_through_short_writes_and_timeouts() {
    struct ShortWriter {
        bytes: Vec<u8>,
        calls: usize,
    }
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            assert!(bytes.len() <= 64 * 1024);
            self.calls += 1;
            match self.calls % 4 {
                0 => Err(io::ErrorKind::TimedOut.into()),
                1 => Err(io::ErrorKind::Interrupted.into()),
                2 => Err(io::ErrorKind::WouldBlock.into()),
                _ => {
                    let n = bytes.len().min(997);
                    self.bytes.extend_from_slice(&bytes[..n]);
                    Ok(n)
                }
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("must not flush a partial image")
        }
    }
    let bytes = encode_clipboard_image(
        ClientClipboardImageTarget::Popup("popup".into()),
        "PNG",
        vec![42; MAX_FRAME_SIZE + 1],
    )
    .unwrap();
    let mut stream = ShortWriter {
        bytes: Vec::new(),
        calls: 0,
    };
    let mut writer = ImageWriter::default();
    for _ in 0..20_000 {
        if writer.poll(&mut stream, &bytes).unwrap() {
            break;
        }
    }
    assert_eq!(stream.bytes, bytes);
    writer.started = Some(Instant::now() - COMMAND_TIMEOUT);
    assert!(matches!(
        writer.check_timeout(),
        Err(Error::ClipboardImageWriteTimeout)
    ));
}

#[cfg(windows)]
#[test]
fn images_refuse_unbounded_pipe_writes() {
    let (client, _server, worker) = test_client();
    assert!(matches!(
        client
            .handle
            .reserve_clipboard_image("boot", ClientClipboardImageTarget::DirectTerminal),
        Err(Error::ClipboardImageUnsupported)
    ));
    client.handle.disconnect();
    worker.join().unwrap().unwrap();
}

#[cfg(unix)]
mod unix {
    use super::*;

    fn reserve(client: &Client) -> ClipboardImageUpload {
        client
            .handle
            .reserve_clipboard_image("boot-v1", ClientClipboardImageTarget::Pane("w1:p1".into()))
            .unwrap()
    }

    fn ready() -> (Client, Stream, thread::JoinHandle<Result<()>>) {
        let (client, mut server, worker) = test_client();
        handshake(&mut server);
        event(&client);
        event(&client);
        (client, server, worker)
    }

    fn no_command(server: &mut Stream) {
        server
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        assert!(matches!(
            server.read(&mut [0]).unwrap_err().kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        server
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
    }

    #[test]
    fn reservation_validation_busy_drop_and_queue_bounds() {
        let (commands, rx) = bounded(1);
        let handle = ClientHandle {
            inner: Arc::new(HandleInner {
                commands,
                stop: Arc::new(AtomicBool::new(false)),
                next_request: AtomicU64::new(1),
                image_busy: Arc::new(AtomicBool::new(false)),
            }),
        };
        let target = ClientClipboardImageTarget::Pane("p".into());
        assert!(matches!(
            handle.reserve_clipboard_image("", target.clone()),
            Err(Error::MissingBootId)
        ));
        for id in [
            String::new(),
            "x".repeat(MAX_CLIPBOARD_IMAGE_TARGET_BYTES + 1),
        ] {
            assert!(matches!(
                handle.reserve_clipboard_image("boot", ClientClipboardImageTarget::Pane(id)),
                Err(Error::Protocol(protocol::Error::ClipboardImageTarget))
            ));
        }
        let upload = handle
            .reserve_clipboard_image("boot", target.clone())
            .unwrap();
        assert!(matches!(
            handle.reserve_clipboard_image("boot", target.clone()),
            Err(Error::ClipboardImageBusy)
        ));
        drop(upload);
        // A dropped slot still occupies the one lease until the worker skips it.
        assert!(matches!(
            handle.reserve_clipboard_image("boot", target.clone()),
            Err(Error::ClipboardImageBusy)
        ));
        drop(rx.try_recv().unwrap());
        handle.set_focus("boot", true).unwrap();
        assert!(matches!(
            handle.reserve_clipboard_image("boot", target.clone()),
            Err(Error::Full)
        ));
        drop(rx.try_recv().unwrap());
        let upload = handle
            .reserve_clipboard_image("boot", target.clone())
            .unwrap();
        let cancel = upload.cancellation_handle();
        cancel.cancel();
        assert!(upload.is_cancelled());
        assert!(matches!(
            upload.complete("png", vec![1]),
            Err(Error::ClipboardImageCancelled)
        ));
        drop(rx.try_recv().unwrap());
        // Retaining the cancellation handle cannot keep the lease busy.
        let upload = handle
            .reserve_clipboard_image("boot", target.clone())
            .unwrap();
        handle.disconnect();
        assert!(upload.is_cancelled());
        assert!(matches!(
            upload.complete("png", vec![1]),
            Err(Error::Disconnected)
        ));
        assert!(matches!(
            handle.reserve_clipboard_image("boot", target),
            Err(Error::Disconnected)
        ));
    }

    #[test]
    fn input_reservations_share_queue_bound_without_claiming_or_releasing_image_lease() {
        use std::sync::atomic::Ordering;
        let (commands, rx) = bounded(COMMAND_CAPACITY);
        let handle = ClientHandle {
            inner: Arc::new(HandleInner {
                commands,
                stop: Arc::new(AtomicBool::new(false)),
                next_request: AtomicU64::new(1),
                image_busy: Arc::new(AtomicBool::new(false)),
            }),
        };
        let target = ClientClipboardImageTarget::Pane("p".into());
        for _ in 0..COMMAND_CAPACITY {
            drop(
                handle
                    .reserve_clipboard_input("boot", target.clone())
                    .unwrap(),
            );
        }
        assert!(!handle.inner.image_busy.load(Ordering::Acquire));
        assert!(matches!(
            handle.reserve_clipboard_input("boot", target.clone()),
            Err(Error::Full)
        ));
        assert!(matches!(
            handle.reserve_clipboard_image("boot", target.clone()),
            Err(Error::Full)
        ));
        assert!(!handle.inner.image_busy.load(Ordering::Acquire));
        assert!(matches!(handle.set_focus("boot", true), Err(Error::Full)));
        for _ in 0..COMMAND_CAPACITY {
            drop(rx.try_recv().unwrap());
        }

        let image = handle
            .reserve_clipboard_image("boot", target.clone())
            .unwrap();
        let image_slot = rx.try_recv().unwrap();
        let text = handle
            .reserve_clipboard_input("boot", target.clone())
            .unwrap();
        text.complete_input(ClientPaneInputEvent::Paste("text".into()))
            .unwrap();
        drop(rx.try_recv().unwrap());
        assert!(handle.inner.image_busy.load(Ordering::Acquire));
        let competing = handle
            .reserve_clipboard_input("boot", target.clone())
            .unwrap();
        // Busy takes precedence even over invalid data: no image encoding occurs.
        assert!(matches!(
            competing.complete("invalid", vec![]),
            Err(Error::ClipboardImageBusy)
        ));
        drop(rx.try_recv().unwrap());
        assert!(handle.inner.image_busy.load(Ordering::Acquire));
        drop(image);
        drop(image_slot);
        assert!(!handle.inner.image_busy.load(Ordering::Acquire));

        let deferred = handle
            .reserve_clipboard_input("boot", target.clone())
            .unwrap();
        assert!(!handle.inner.image_busy.load(Ordering::Acquire));
        deferred.complete("png", vec![1]).unwrap();
        assert!(handle.inner.image_busy.load(Ordering::Acquire));
        let deferred_slot = rx.try_recv().unwrap();
        let competing = handle
            .reserve_clipboard_input("boot", target.clone())
            .unwrap();
        assert!(matches!(
            competing.complete("png", vec![2]),
            Err(Error::ClipboardImageBusy)
        ));
        drop(rx.try_recv().unwrap());
        assert!(handle.inner.image_busy.load(Ordering::Acquire));
        drop(deferred_slot);
        assert!(!handle.inner.image_busy.load(Ordering::Acquire));
        let invalid = handle.reserve_clipboard_input("boot", target).unwrap();
        assert!(matches!(
            invalid.complete("png", vec![]),
            Err(Error::Protocol(protocol::Error::ClipboardImageSize))
        ));
        drop(rx.try_recv().unwrap());
        assert!(!handle.inner.image_busy.load(Ordering::Acquire));
    }

    #[test]
    fn text_reserved_during_image_stays_before_later_input_and_competing_image_is_busy() {
        let (client, mut server, worker) = ready();
        let first = reserve(&client);
        let first_done = first.cancellation_handle();
        let target = ClientClipboardImageTarget::Pane("w1:p1".into());
        let text = client
            .handle
            .reserve_clipboard_input("boot-v1", target.clone())
            .unwrap();
        let text_done = text.cancellation_handle();
        let competing = client
            .handle
            .reserve_clipboard_input("boot-v1", target)
            .unwrap();
        let competing_done = competing.cancellation_handle();
        client
            .handle
            .send_input(
                "boot-v1",
                "w1:p1",
                [ClientPaneInputEvent::TextCommit("later".into())],
            )
            .unwrap();
        text.complete_input(ClientPaneInputEvent::Paste("original text".into()))
            .unwrap();
        assert!(matches!(
            competing.complete("png", vec![2]),
            Err(Error::ClipboardImageBusy)
        ));
        no_command(&mut server);
        assert!(!first_done.is_finished());
        assert!(!text_done.is_finished());
        first.complete("png", vec![1]).unwrap();
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClipboardImage {
                target: ClientClipboardImageTarget::Pane("w1:p1".into()),
                extension: "png".into(),
                data: vec![1],
            }
        );
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellPaneInput {
                pane_id: "w1:p1".into(),
                events: vec![ClientPaneInputEvent::Paste("original text".into())],
            }
        );
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellPaneInput {
                pane_id: "w1:p1".into(),
                events: vec![ClientPaneInputEvent::TextCommit("later".into())],
            }
        );
        assert!(
            first_done.is_finished() && text_done.is_finished() && competing_done.is_finished()
        );
        drop(reserve(&client));
        client.handle.disconnect();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn finished_tracks_slot_drop_not_publication_or_cancellation() {
        for mode in 0..4 {
            let (commands, rx) = bounded(1);
            let handle = ClientHandle {
                inner: Arc::new(HandleInner {
                    commands,
                    stop: Arc::new(AtomicBool::new(false)),
                    next_request: AtomicU64::new(1),
                    image_busy: Arc::new(AtomicBool::new(false)),
                }),
            };
            let upload = handle
                .reserve_clipboard_image("boot", ClientClipboardImageTarget::Pane("p".into()))
                .unwrap();
            let cancel = upload.cancellation_handle();
            let observer = cancel.clone();
            assert!(!cancel.is_finished());
            match mode {
                0 => upload.complete("png", vec![1]).unwrap(),
                1 => upload
                    .complete_input(ClientPaneInputEvent::Paste("missing.png".into()))
                    .unwrap(),
                2 => {
                    cancel.cancel();
                    drop(upload);
                }
                _ => {
                    drop(upload);
                }
            }
            assert!(!observer.is_finished());
            let command = rx.try_recv().unwrap();
            assert!(!observer.is_finished());
            drop(command);
            assert!(cancel.is_finished());
            assert!(observer.is_finished());
        }
    }

    #[test]
    fn sustained_inbound_progress_survives_a_backpressured_outgoing_image() {
        let (client, mut server, worker) = ready();
        let upload = reserve(&client);
        let cancel = upload.cancellation_handle();
        upload
            .complete("png", vec![99; MAX_CLIPBOARD_IMAGE_PAYLOAD])
            .unwrap();
        let mut prefix = [0; 4];
        server.read_exact(&mut prefix).unwrap();
        // Never drain the image. Repeated large inbound frames must still finish
        // within the unchanged partial-frame deadline while outbound writes stall.
        for value in [11, 22] {
            let started = Instant::now();
            send(
                &mut server,
                ServerMessage::Graphics {
                    bytes: vec![value; 24 * 1024 * 1024],
                },
            );
            assert!(
                matches!(event(&client), ClientEvent::Message(ServerMessage::Graphics { bytes })
                if bytes.len() == 24 * 1024 * 1024 && bytes.iter().all(|byte| *byte == value))
            );
            assert!(started.elapsed() < TIMEOUT);
            assert!(
                !cancel.is_finished(),
                "publication is not worker completion"
            );
        }
        cancel.cancel();
        assert!(matches!(
            worker.join().unwrap(),
            Err(Error::ClipboardImageCancelled)
        ));
        assert!(cancel.is_finished());
    }

    #[test]
    fn pending_image_keeps_reads_alive_and_drop_skips_fifo_slot() {
        let (client, mut server, worker) = ready();
        client.handle.set_focus("boot-v1", true).unwrap();
        let upload = reserve(&client);
        client.handle.set_focus("boot-v1", false).unwrap();
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellFocus { focused: true }
        );
        send(&mut server, ServerMessage::TerminalBell { count: 7 });
        assert!(matches!(
            event(&client),
            ClientEvent::Message(ServerMessage::TerminalBell { count: 7 })
        ));
        no_command(&mut server);
        drop(upload);
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellFocus { focused: false }
        );
        drop(reserve(&client));
        client.handle.disconnect();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn invalid_completion_skips_slot_and_preserves_protocol_source() {
        let (client, mut server, worker) = ready();
        let upload = reserve(&client);
        client.handle.set_focus("boot-v1", false).unwrap();
        let error = upload.complete("svg", vec![1]).unwrap_err();
        assert!(matches!(
            error,
            Error::Protocol(protocol::Error::ClipboardImageExtension)
        ));
        assert!(
            std::error::Error::source(&error)
                .unwrap()
                .is::<protocol::Error>()
        );
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellFocus { focused: false }
        );
        client.handle.disconnect();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn fallback_paste_uses_reserved_target_and_precedes_later_enter() {
        for target in [
            ClientClipboardImageTarget::Pane("w1:p1".into()),
            ClientClipboardImageTarget::Popup("popup-1".into()),
        ] {
            let (client, mut server, worker) = ready();
            let upload = client
                .handle
                .reserve_clipboard_image("boot-v1", target.clone())
                .unwrap();
            let enter = ClientPaneInputEvent::Key {
                code: ClientKeyCode::Enter,
                modifiers: 0,
                kind: ClientKeyKind::Press,
                repeat_count: 1,
                shifted_codepoint: None,
                generated_text: None,
                tracks_release: false,
                physical_key_id: None,
                windows_record: None,
            };
            client
                .handle
                .send_input("boot-v1", "w1:p2", [enter.clone()])
                .unwrap();
            no_command(&mut server);
            // File eligibility is determined by the caller; the client preserves
            // the original text verbatim, including shell quoting and newlines.
            let text = "'/missing image.png'\n";
            upload
                .complete_input(ClientPaneInputEvent::Paste(text.into()))
                .unwrap();
            let events = vec![ClientPaneInputEvent::Paste(text.into())];
            let expected = match target {
                ClientClipboardImageTarget::Pane(pane_id) => {
                    ClientMessage::ClientShellPaneInput { pane_id, events }
                }
                ClientClipboardImageTarget::Popup(terminal_id) => {
                    ClientMessage::ClientShellPopupInput {
                        terminal_id,
                        events,
                    }
                }
                ClientClipboardImageTarget::DirectTerminal => unreachable!(),
            };
            assert_eq!(receive(&mut server), expected);
            assert_eq!(
                receive(&mut server),
                ClientMessage::ClientShellPaneInput {
                    pane_id: "w1:p2".into(),
                    events: vec![enter]
                }
            );
            client.handle.disconnect();
            worker.join().unwrap().unwrap();
        }
    }

    #[test]
    fn ctrl_v_fallback_preserves_original_key_before_later_input() {
        for target in [
            ClientClipboardImageTarget::Pane("w1:p1".into()),
            ClientClipboardImageTarget::Popup("popup-1".into()),
        ] {
            let (client, mut server, worker) = ready();
            let upload = client
                .handle
                .reserve_clipboard_image("boot-v1", target.clone())
                .unwrap();
            let cancellation = upload.cancellation_handle();
            let key = ClientPaneInputEvent::Key {
                code: ClientKeyCode::Char('v'),
                modifiers: 2,
                kind: ClientKeyKind::Press,
                repeat_count: 1,
                shifted_codepoint: Some(u32::from('V')),
                generated_text: Some("\u{16}".into()),
                tracks_release: true,
                physical_key_id: Some(9),
                windows_record: Some(WindowsKeyRecord {
                    key_down: true,
                    repeat_count: 1,
                    virtual_key_code: 86,
                    virtual_scan_code: 47,
                    unicode: 22,
                    control_key_state: 8,
                }),
            };
            client
                .handle
                .send_input(
                    "boot-v1",
                    "w1:p2",
                    [ClientPaneInputEvent::TextCommit("later".into())],
                )
                .unwrap();
            no_command(&mut server);
            assert!(!cancellation.is_finished());
            // The background clipboard result is empty: preserve the exact
            // original key, including platform and physical-key metadata.
            upload.complete_input(key.clone()).unwrap();
            let events = vec![key];
            let expected = match target {
                ClientClipboardImageTarget::Pane(pane_id) => {
                    ClientMessage::ClientShellPaneInput { pane_id, events }
                }
                ClientClipboardImageTarget::Popup(terminal_id) => {
                    ClientMessage::ClientShellPopupInput {
                        terminal_id,
                        events,
                    }
                }
                ClientClipboardImageTarget::DirectTerminal => unreachable!(),
            };
            assert_eq!(receive(&mut server), expected);
            assert_eq!(
                receive(&mut server),
                ClientMessage::ClientShellPaneInput {
                    pane_id: "w1:p2".into(),
                    events: vec![ClientPaneInputEvent::TextCommit("later".into())],
                }
            );
            assert!(cancellation.is_finished());
            client.handle.disconnect();
            worker.join().unwrap().unwrap();
        }
    }

    #[test]
    fn fallback_respects_cancellation_direct_target_and_ordinary_limit() {
        for mode in 0..4 {
            let (client, mut server, worker) = ready();
            let target = if mode == 0 {
                ClientClipboardImageTarget::DirectTerminal
            } else {
                ClientClipboardImageTarget::Pane("w1:p1".into())
            };
            let upload = client
                .handle
                .reserve_clipboard_image("boot-v1", target)
                .unwrap();
            client.handle.set_focus("boot-v1", false).unwrap();
            if mode == 2 {
                upload.cancellation_handle().cancel();
            }
            if mode == 3 {
                client.handle.disconnect();
            }
            let text = if mode == 1 {
                "x".repeat(MAX_FRAME_SIZE)
            } else {
                "missing.png".into()
            };
            let error = upload
                .complete_input(ClientPaneInputEvent::Paste(text))
                .unwrap_err();
            match mode {
                0 => assert!(matches!(error, Error::ClipboardImageInputTarget)),
                1 => assert!(matches!(error, Error::Protocol(protocol::Error::Encode(_)))),
                2 => assert!(matches!(error, Error::ClipboardImageCancelled)),
                _ => assert!(matches!(error, Error::Disconnected)),
            }
            if mode != 3 {
                assert_eq!(
                    receive(&mut server),
                    ClientMessage::ClientShellFocus { focused: false }
                );
            }
            client.handle.disconnect();
            worker.join().unwrap().unwrap();
        }
    }

    #[test]
    fn expired_preparation_skips_retained_permit_but_not_published_data() {
        use crate::clipboard::{ImageLease, ImageSlot};
        use crate::handle::Command;
        use std::sync::atomic::Ordering;

        for published in [false, true] {
            let (client, mut server, worker) = ready();
            // Inject an aged reservation rather than sleeping for its deadline.
            let lease = Arc::new(ImageLease {
                busy: client.handle.inner.image_busy.clone(),
                claimed: AtomicBool::new(true),
                cancelled: Arc::new(AtomicBool::new(false)),
                finished: Arc::new(AtomicBool::new(false)),
                reserved_at: Instant::now() - COMMAND_TIMEOUT,
            });
            assert!(!lease.busy.swap(true, Ordering::AcqRel));
            let (sender, receiver) = bounded(1);
            let upload = ClipboardImageUpload {
                target: ClientClipboardImageTarget::Pane("w1:p1".into()),
                sender,
                lease: lease.clone(),
                stop: client.handle.inner.stop.clone(),
            };
            let expected = ClientMessage::ClientShellPaneInput {
                pane_id: "w1:p1".into(),
                events: vec![ClientPaneInputEvent::Paste("original.png".into())],
            };
            if published {
                upload
                    .sender
                    .try_send(encode_message(&expected, MAX_FRAME_SIZE).unwrap())
                    .unwrap();
            }
            client
                .handle
                .inner
                .commands
                .try_send(Command {
                    boot_id: "boot-v1".into(),
                    bytes: Vec::new(),
                    request: None,
                    image: Some(ImageSlot {
                        receiver,
                        lease,
                        writer: Default::default(),
                    }),
                })
                .unwrap_or_else(|_| panic!("empty queue"));
            client.handle.set_focus("boot-v1", false).unwrap();
            assert!(upload.is_cancelled());
            if published {
                assert_eq!(receive(&mut server), expected);
            } else {
                assert!(matches!(
                    event(&client),
                    ClientEvent::CommandRejected {
                        request_id: None,
                        reason: Error::ClipboardImagePreparationTimeout,
                    }
                ));
            }
            // The retained permit cannot stall ordinary input after expiration.
            assert_eq!(
                receive(&mut server),
                ClientMessage::ClientShellFocus { focused: false }
            );
            assert!(matches!(
                client
                    .handle
                    .reserve_clipboard_image("boot-v1", ClientClipboardImageTarget::DirectTerminal),
                Err(Error::ClipboardImageBusy)
            ));
            assert!(matches!(
                upload.complete_input(ClientPaneInputEvent::Paste("late.png".into())),
                Err(Error::ClipboardImageCancelled)
            ));
            drop(reserve(&client));
            client.handle.disconnect();
            worker.join().unwrap().unwrap();
        }
    }

    #[test]
    fn partial_inbound_frame_cannot_stall_an_already_started_upload() {
        let (client, mut server, worker) = ready();
        reserve(&client)
            .complete("png", vec![88; MAX_CLIPBOARD_IMAGE_PAYLOAD])
            .unwrap();
        client.handle.set_focus("boot-v1", false).unwrap();
        let mut prefix = [0; 4];
        server.read_exact(&mut prefix).unwrap();
        // The outbound image has started. Simulate a peer that must drain that
        // frame before it can finish producing its own inbound frame.
        let inbound =
            encode_message(&ServerMessage::TerminalBell { count: 12 }, MAX_FRAME_SIZE).unwrap();
        server.write_all(&inbound[..4]).unwrap();
        let mut payload = vec![0; u32::from_le_bytes(prefix) as usize];
        server.read_exact(&mut payload).unwrap();
        assert!(matches!(decode_payload::<ClientMessage>(&payload).unwrap(),
            ClientMessage::ClipboardImage { data, .. }
            if data.len() == MAX_CLIPBOARD_IMAGE_PAYLOAD && data.iter().all(|b| *b == 88)));
        // Only the continuation bypasses partial inbound state, not later input.
        no_command(&mut server);
        server.write_all(&inbound[4..]).unwrap();
        assert!(matches!(
            event(&client),
            ClientEvent::Message(ServerMessage::TerminalBell { count: 12 })
        ));
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellFocus { focused: false }
        );
        client.handle.disconnect();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn ready_image_waits_behind_request_lease_and_precedes_later_input() {
        let (client, mut server, worker) = ready();
        let first = client.handle.focus_pane("boot-v1", "w1:p1").unwrap();
        let second = client.handle.focus_pane("boot-v1", "w1:p2").unwrap();
        let upload = reserve(&client);
        client.handle.set_focus("boot-v1", false).unwrap();
        upload
            .complete("jpeg", vec![42; MAX_FRAME_SIZE + 1])
            .unwrap();
        assert!(matches!(
            receive(&mut server),
            ClientMessage::ClientShellEndpointRequest { .. }
        ));
        no_command(&mut server);
        send(
            &mut server,
            ServerMessage::ClientShellEndpointResponseChunk {
                boot_id: "boot-v1".into(),
                request_id: first.clone(),
                final_chunk: true,
                data: json!({"id": first, "result": {}}).to_string().into_bytes(),
            },
        );
        assert!(matches!(event(&client), ClientEvent::Response { .. }));
        assert!(
            matches!(receive(&mut server), ClientMessage::ClientShellEndpointRequest { request, .. }
            if serde_json::from_str::<Value>(&request).unwrap()["id"] == second)
        );
        // Images (like normal input) do not acquire or wait on the API lease.
        let message: ClientMessage =
            read_message(&mut server, MAX_CLIPBOARD_IMAGE_FRAME_SIZE).unwrap();
        assert_eq!(
            message,
            ClientMessage::ClipboardImage {
                target: ClientClipboardImageTarget::Pane("w1:p1".into()),
                extension: "jpg".into(),
                data: vec![42; MAX_FRAME_SIZE + 1],
            }
        );
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellFocus { focused: false }
        );
        drop(reserve(&client));
        client.handle.disconnect();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn preparing_upload_does_not_keep_last_client_handle_alive() {
        let (client, _server, worker) = ready();
        let upload = reserve(&client);
        drop(client.handle);
        worker.join().unwrap().unwrap();
        assert!(upload.is_cancelled());
    }

    #[test]
    fn stale_boot_rejects_pending_reservation_without_blocking_fifo() {
        let (client, mut server, worker) = ready();
        let upload = client
            .handle
            .reserve_clipboard_image("stale", ClientClipboardImageTarget::DirectTerminal)
            .unwrap();
        let cancel = upload.cancellation_handle();
        client.handle.set_focus("boot-v1", false).unwrap();
        assert!(matches!(
            event(&client),
            ClientEvent::CommandRejected {
                reason: Error::CommandBoot,
                ..
            }
        ));
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellFocus { focused: false }
        );
        assert!(upload.is_cancelled());
        assert!(cancel.is_finished());
        assert!(matches!(
            upload.complete("png", vec![1]),
            Err(Error::ClipboardImageCancelled)
        ));
        client.handle.disconnect();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn backpressured_image_reads_events_and_cancellation_or_boot_change_closes_frame() {
        for mode in 0..4 {
            let (client, mut server, worker) = ready();
            let upload = reserve(&client);
            let cancel = upload.cancellation_handle();
            upload
                .complete("png", vec![99; MAX_CLIPBOARD_IMAGE_PAYLOAD])
                .unwrap();
            client.handle.set_focus("boot-v1", false).unwrap();
            let mut prefix = [0; 4];
            server.read_exact(&mut prefix).unwrap();
            let len = u32::from_le_bytes(prefix) as usize;
            // Stop consuming a 16 MiB frame. The worker must still read messages.
            send(&mut server, ServerMessage::TerminalBell { count: 9 });
            assert!(matches!(
                event(&client),
                ClientEvent::Message(ServerMessage::TerminalBell { count: 9 })
            ));
            assert!(matches!(
                client
                    .handle
                    .reserve_clipboard_image("boot-v1", ClientClipboardImageTarget::DirectTerminal),
                Err(Error::ClipboardImageBusy)
            ));
            match mode {
                0 => cancel.cancel(),
                1 => client.handle.disconnect(),
                3 => {
                    // Resume a slow reader: offsets must produce one exact frame,
                    // with the next command following it rather than interleaving.
                    let mut payload = vec![0; len];
                    server.read_exact(&mut payload).unwrap();
                    assert!(matches!(decode_payload::<ClientMessage>(&payload).unwrap(),
                        ClientMessage::ClipboardImage { data, .. }
                        if data.len() == MAX_CLIPBOARD_IMAGE_PAYLOAD && data.iter().all(|b| *b == 99)));
                    assert_eq!(
                        receive(&mut server),
                        ClientMessage::ClientShellFocus { focused: false }
                    );
                    assert!(cancel.is_finished());
                    client.handle.disconnect();
                    worker.join().unwrap().unwrap();
                    continue;
                }
                _ => send(
                    &mut server,
                    ServerMessage::EndpointControl {
                        kind: ENDPOINT_SNAPSHOT_KIND.into(),
                        data: SNAPSHOT.replace("boot-v1", "new-boot"),
                    },
                ),
            }
            let result = worker.join().unwrap();
            match mode {
                0 => assert!(matches!(result, Err(Error::ClipboardImageCancelled))),
                1 => result.unwrap(),
                _ => assert!(matches!(result, Err(Error::SnapshotIdentity))),
            }
            let mut remainder = Vec::new();
            server.read_to_end(&mut remainder).unwrap();
            assert!(
                remainder.len() < len,
                "must close rather than complete/replay a cancelled frame"
            );
            assert!(cancel.cancelled.load(std::sync::atomic::Ordering::Acquire));
        }
    }

    #[test]
    fn completing_image_cannot_bypass_a_partial_inbound_boot_change() {
        let (client, mut server, worker) = ready();
        let upload = reserve(&client);
        let message = encode_message(
            &ServerMessage::EndpointControl {
                kind: ENDPOINT_SNAPSHOT_KIND.into(),
                data: SNAPSHOT.replace("boot-v1", "new-boot"),
            },
            MAX_FRAME_SIZE,
        )
        .unwrap();
        server.write_all(&message[..5]).unwrap();
        // Hold the inbound frame incomplete across read polls. Completing the
        // upload must not let it bypass that frame's boot validation.
        no_command(&mut server);
        upload.complete("png", vec![1; MAX_FRAME_SIZE + 1]).unwrap();
        no_command(&mut server);
        server.write_all(&message[5..]).unwrap();
        assert!(matches!(
            worker.join().unwrap(),
            Err(Error::SnapshotIdentity)
        ));
        assert_eq!(server.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn pending_upload_allows_health_probe_and_response() {
        let (client, mut server, worker) = test_client_mode(true, true);
        receive(&mut server);
        let mut welcome: Value = serde_json::from_str(WELCOME).unwrap();
        welcome["capabilities"] = json!([
            "health_check",
            "surface_interest",
            "presentation_effects_fence"
        ]);
        welcome["methods"]
            .as_array_mut()
            .unwrap()
            .push(json!("client_shell.surface.set"));
        send(
            &mut server,
            ServerMessage::EndpointControl {
                kind: ENDPOINT_WELCOME_KIND.into(),
                data: welcome.to_string(),
            },
        );
        send(
            &mut server,
            ServerMessage::EndpointControl {
                kind: ENDPOINT_SNAPSHOT_KIND.into(),
                data: SNAPSHOT.into(),
            },
        );
        event(&client);
        event(&client);
        let upload = reserve(&client);
        client.handle.set_focus("boot-v1", false).unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(7)))
            .unwrap();
        assert!(
            matches!(receive(&mut server), ClientMessage::EndpointControl { kind, .. } if kind == "endpoint.health.ping.v1")
        );
        send(&mut server, ServerMessage::TerminalBell { count: 11 });
        assert!(matches!(
            event(&client),
            ClientEvent::Message(ServerMessage::TerminalBell { count: 11 })
        ));
        upload.cancellation_handle().cancel();
        assert_eq!(
            receive(&mut server),
            ClientMessage::ClientShellFocus { focused: false }
        );
        assert!(upload.is_cancelled());
        client.handle.disconnect();
        worker.join().unwrap().unwrap();
    }
}
